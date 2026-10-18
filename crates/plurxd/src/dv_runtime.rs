//! Daemon-owned, private DV window execution. No helper JSON field grants a route.
use plurx_core::transcode::dv_processing::{DvBackendIdentity, DvDigest};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(target_os = "linux")]
type RegistrationHook = Box<dyn FnOnce(&crate::prodrun::ProducerRegistration) + Send>;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    path: PathBuf,
    sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    graph: String,
    abi: BTreeMap<String, u32>,
    tools: BTreeMap<String, Artifact>,
    libraries: Vec<Artifact>,
    parser_sha256: String,
    source_sha256: String,
    environment: BTreeMap<String, String>,
}
#[derive(Clone)]
pub(crate) struct ToolSet {
    pub(crate) renderer: PathBuf,
    pub(crate) renderer_env: Vec<(String, std::ffi::OsString)>,
    pub(crate) identity: DvBackendIdentity,
    artifacts: Arc<Vec<(PathBuf, String)>>,
}
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn renderer_source_identity() -> String {
    let inputs: [(&str, &[u8]); 9] = [
        (
            "base_dv_renderer.h",
            include_bytes!("../../../tools/dv_processing/base_dv_renderer.h"),
        ),
        (
            "dv_trace.h",
            include_bytes!("../../../tools/dv_processing/dv_trace.h"),
        ),
        (
            "fel_renderer.h",
            include_bytes!("../../../tools/dv_processing/fel_renderer.h"),
        ),
        (
            "hevc-rasl-frame-thread.patch",
            include_bytes!("../../../tools/dv_processing/hevc-rasl-frame-thread.patch"),
        ),
        (
            "nlq_clipping.h",
            include_bytes!("../../../tools/dv_processing/nlq_clipping.h"),
        ),
        (
            "nut_timing.h",
            include_bytes!("../../../tools/dv_processing/nut_timing.h"),
        ),
        (
            "rgb48.h",
            include_bytes!("../../../tools/dv_processing/rgb48.h"),
        ),
        (
            "segment_decode_render.c",
            include_bytes!("../../../tools/dv_processing/segment_decode_render.c"),
        ),
        (
            "yuv420p10.h",
            include_bytes!("../../../tools/dv_processing/yuv420p10.h"),
        ),
    ];
    let mut identity = Sha256::new();
    for (name, bytes) in inputs {
        identity.update(name.as_bytes());
        identity.update([0]);
        identity.update(Sha256::digest(bytes));
    }
    hex::encode(identity.finalize())
}
/// Decode on the GPU the renderer already draws on. The helper maps VA-API
/// surfaces straight into its Vulkan renderer; a host copy of a decoded 4K
/// picture costs more than decoding it in software. The VA-API runtime and
/// drivers are the media runtime's (the same ones the encoder uses), so its
/// library directory follows the bundle's own on the search path.
fn gpu_decode_environment(
    environment: &mut Vec<(String, std::ffi::OsString)>,
    device: &str,
    ffmpeg: &str,
) {
    if !Path::new(device).exists() {
        return;
    }
    environment.push(("PLURX_DV_DECODE_DEVICE".into(), device.into()));
    let Some(media_lib) = Path::new(ffmpeg)
        .parent()
        .map(|bin| bin.join("lib"))
        .filter(|lib| lib.join("libva.so.2").exists())
    else {
        return;
    };
    match environment
        .iter_mut()
        .find(|(name, _)| name == "LD_LIBRARY_PATH")
    {
        Some((_, value)) => {
            let mut joined = value.clone();
            joined.push(":");
            joined.push(media_lib.as_os_str());
            *value = joined;
        }
        None => environment.push(("LD_LIBRARY_PATH".into(), media_lib.into_os_string())),
    }
}
fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}
fn digest(bytes: &[u8]) -> Result<DvDigest, String> {
    DvDigest::new(hash(bytes)).map_err(|e| e.to_string())
}

#[cfg(target_os = "linux")]
pub(crate) fn failure_episode_key(
    playback_id: &str,
    source_version: &str,
    tools: &ToolSet,
    preferences: plurx_core::transcode::dv_processing::DvPreferences,
    destination: plurx_core::transcode::dv_processing::DvDestination,
) -> Result<DvDigest, String> {
    let backend = digest(&serde_json::to_vec(&tools.identity).map_err(|e| e.to_string())?)?;
    Ok(super::dv_failed_episode::key(
        playback_id,
        source_version,
        &backend,
        preferences.planning_input_generation,
        destination,
    ))
}

#[cfg(target_os = "linux")]
pub(crate) fn episode_failed(key: &DvDigest) -> bool {
    super::dv_failed_episode::failed(key)
}

#[cfg(target_os = "linux")]
fn fail_episode(key: &DvDigest) {
    super::dv_failed_episode::refuse(key)
}
fn contained(root: &Path, path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err("DV artifact path is not relative to its owned root".into());
    }
    let resolved = root.join(path).canonicalize().map_err(|e| e.to_string())?;
    if !resolved.starts_with(root) || !resolved.is_file() {
        return Err("DV artifact escapes its owned root".into());
    }
    Ok(resolved)
}
impl ToolSet {
    /// Configuration is captured by the worker, never supplied by a playback
    /// request. Absence preserves ordinary output and the saved preference.
    pub(crate) async fn capture() -> Result<Option<Arc<Self>>, String> {
        let root = std::env::var_os("PLURX_DV_PROCESSING_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| "/usr/lib/plurx/dv-processing".into());
        tokio::task::spawn_blocking(move || {
            let manifest_path = root.join("build-identity.json");
            let bytes = match std::fs::read(&manifest_path) {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e.to_string()),
            };
            if bytes.len() > 64 * 1024 {
                return Err("DV build identity exceeds its bound".into());
            }
            let root = root.canonicalize().map_err(|e| e.to_string())?;
            let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            if manifest.schema != 1
                || manifest.source_sha256 != renderer_source_identity()
                || manifest.graph != "p7-fel-linear-dz-bt2020-pq-master-clip-v1"
                || manifest.abi.get("libplacebo") != Some(&374)
                || !["avcodec", "avformat", "avutil"]
                    .iter()
                    .all(|key| manifest.abi.get(*key).is_some_and(|major| *major > 0))
                || !manifest.tools.contains_key("renderer")
                || manifest.tools.len() > 3
                || manifest.libraries.is_empty()
                || manifest.libraries.len() > 32
            {
                return Err("unsupported DV build identity".into());
            }
            let mut artifacts = Vec::new();
            let mut tools = BTreeMap::new();
            for (name, artifact) in &manifest.tools {
                if !matches!(name.as_str(), "renderer" | "muxer" | "author") {
                    return Err("unknown DV helper".into());
                }
                // Optional standalone tools do not participate in the actual
                // streaming renderer + daemon packet-authoring graph.
                if name != "renderer" {
                    continue;
                }
                let path = contained(&root, &artifact.path)?;
                let actual = hash_file(&path)?;
                if actual != artifact.sha256 {
                    return Err("DV helper differs from its build identity".into());
                }
                tools.insert(name.clone(), path.clone());
                artifacts.push((path, actual));
            }
            let mut library_identity = Vec::new();
            for artifact in &manifest.libraries {
                let path = contained(&root, &artifact.path)?;
                let actual = hash_file(&path)?;
                if actual != artifact.sha256 {
                    return Err("DV library differs from its build identity".into());
                }
                library_identity.extend_from_slice(actual.as_bytes());
                artifacts.push((path, actual));
            }
            let mut environment = Vec::new();
            for (name, value) in &manifest.environment {
                if !matches!(name.as_str(), "LD_LIBRARY_PATH" | "VK_ICD_FILENAMES") {
                    return Err("unsupported DV helper environment".into());
                }
                let paths: Result<Vec<_>, String> = value
                    .split(':')
                    .map(|value| {
                        let path = Path::new(value);
                        if path.is_absolute()
                            || path
                                .components()
                                .any(|part| !matches!(part, std::path::Component::Normal(_)))
                        {
                            return Err("DV environment escapes its root".into());
                        }
                        let path = root.join(path).canonicalize().map_err(|e| e.to_string())?;
                        if !path.starts_with(&root) {
                            return Err("DV environment escapes its root".into());
                        }
                        Ok(path)
                    })
                    .collect();
                environment.push((
                    name.clone(),
                    std::env::join_paths(paths?).map_err(|e| e.to_string())?,
                ));
            }
            let executable_identity = manifest
                .tools
                .get("renderer")
                .ok_or("missing renderer")?
                .sha256
                .as_bytes();
            let identity = DvBackendIdentity::new(
                digest(executable_identity)?,
                digest(&library_identity)?,
                DvDigest::new(manifest.parser_sha256).map_err(|e| e.to_string())?,
                DvDigest::new(manifest.source_sha256).map_err(|e| e.to_string())?,
                1,
                digest(&bytes)?,
            )
            .map_err(|e| e.to_string())?;
            artifacts.push((root.join("build-identity.json"), hash(&bytes)));
            Ok(Some(Arc::new(Self {
                renderer: tools.remove("renderer").ok_or("missing renderer")?,
                renderer_env: environment,
                identity,
                artifacts: Arc::new(artifacts),
            })))
        })
        .await
        .map_err(|e| e.to_string())?
    }
    pub(crate) async fn is_current(&self) -> bool {
        let artifacts = self.artifacts.clone();
        tokio::task::spawn_blocking(move || {
            artifacts
                .iter()
                .all(|(path, expected)| hash_file(path).is_ok_and(|actual| actual == *expected))
        })
        .await
        .unwrap_or(false)
    }
}

#[cfg(target_os = "linux")]
pub(crate) struct PrivateWindow {
    encoded: Vec<u8>,
    pub(crate) evidence: crate::dv_segment::SegmentEvidence,
    registration: crate::prodrun::ProducerRegistration,
}

#[cfg(target_os = "linux")]
pub(crate) struct VerifiedWindow {
    pub(crate) bytes: Vec<u8>,
    pub(crate) observed: ObservedWindow,
    pub(crate) encoded_payloads: Vec<DvDigest>,
    pub(crate) registration: crate::prodrun::ProducerRegistration,
    pub(crate) receipt: Option<plurx_core::transcode::dv_processing::DvProcessingReceipt>,
}

#[cfg(target_os = "linux")]
impl PrivateWindow {
    /// The encoder maps original source pictures to the generation-local
    /// AAC preroll lattice. Original keys and the global grid remain in
    /// `observed`; final sample validation checks the explicit local origin.
    #[expect(
        clippy::too_many_arguments,
        reason = "final validation binds independent source, output, timing and audio facts"
    )]
    pub(crate) async fn finalize(
        self,
        observed: ObservedWindow,
        grid: plurx_core::transcode::VodFrameGrid,
        first_global_frame: u64,
        raster: (u32, u32),
        destination: plurx_core::transcode::dv_processing::DvDestination,
        source_level: Option<u8>,
        runtime_cache: &Path,
        audio_channels: Option<u32>,
    ) -> Result<VerifiedWindow, String> {
        use plurx_core::transcode::dv_processing::{
            dv_author_profile81_window, dv_validate_encoded_window, DvDestination,
        };
        let rpus: Vec<_> = observed
            .pictures
            .iter()
            .map(|picture| picture.rpu.clone())
            .collect();
        let origin =
            plurx_core::transcode::vod_reconstructed_video_origin(grid, first_global_frame)
                .map_err(str::to_owned)?;
        let bytes = match destination {
            DvDestination::Hdr10 => self.encoded,
            DvDestination::Profile81 => dv_author_profile81_window(
                &self.encoded,
                raster,
                grid.numerator,
                origin,
                grid.denominator,
                &rpus,
                source_level.ok_or("held source has no DV level")?,
            )
            .map_err(|e| e.to_string())?,
        };
        let encoded_payloads = dv_validate_encoded_window(
            &bytes,
            raster,
            grid.numerator,
            origin,
            grid.denominator,
            &rpus,
            destination,
        )
        .map_err(|e| e.to_string())?;
        let cache = runtime_cache.to_owned();
        let (bytes, held) = tokio::task::spawn_blocking(move || {
            let held = crate::ffmpeg::stage_completed_probe_file(&bytes, &cache)
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((bytes, held))
        })
        .await
        .map_err(|e| e.to_string())??;
        // Production held probes name the reporter they launch. This output
        // check has no frozen source-clock reporter to reuse, so it captures
        // the current one for exactly this query.
        let reporter =
            crate::ffmpeg::EncodedExecutable::capture_program(&crate::ffmpeg::ffprobe_bin())
                .await?;
        let document = crate::ffmpeg::held_source_probe_json_bound(
            held.as_file(),
            crate::process_control::ChildWork::realtime("DV completed output header probe"),
            &reporter,
        )
        .await?;
        let document: serde_json::Value =
            serde_json::from_str(&document).map_err(|e| e.to_string())?;
        let streams = document
            .get("streams")
            .and_then(serde_json::Value::as_array)
            .ok_or("completed output has no probed streams")?;
        let videos: Vec<_> = streams
            .iter()
            .filter(|stream| {
                stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("video")
            })
            .collect();
        let audios: Vec<_> = streams
            .iter()
            .filter(|stream| {
                stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("audio")
            })
            .collect();
        if videos.len() != 1 || streams.len() != 1 + usize::from(audio_channels.is_some()) {
            return Err("completed output stream membership differs".into());
        }
        let video = videos[0];
        if video.get("width").and_then(serde_json::Value::as_u64) != Some(u64::from(raster.0))
            || video.get("height").and_then(serde_json::Value::as_u64) != Some(u64::from(raster.1))
            || [
                ("codec_name", "hevc"),
                ("profile", "Main 10"),
                ("pix_fmt", "yuv420p10le"),
                ("color_range", "tv"),
                ("color_space", "bt2020nc"),
                ("color_transfer", "smpte2084"),
                ("color_primaries", "bt2020"),
            ]
            .iter()
            .any(|(field, value)| {
                video.get(*field).and_then(serde_json::Value::as_str) != Some(*value)
            })
        {
            return Err("independent encoded header probe differs from HDR10 contract".into());
        }
        if let Some(channels) = audio_channels {
            if audios.len() != 1
                || audios[0]
                    .get("codec_name")
                    .and_then(serde_json::Value::as_str)
                    != Some("aac")
                || audios[0]
                    .get("sample_rate")
                    .and_then(serde_json::Value::as_str)
                    != Some("48000")
                || audios[0]
                    .get("channels")
                    .and_then(serde_json::Value::as_u64)
                    != Some(u64::from(channels))
            {
                return Err(
                    "independent AAC header probe differs from frozen audio contract".into(),
                );
            }
        } else if !audios.is_empty() {
            return Err("unexpected completed audio track".into());
        }
        Ok(VerifiedWindow {
            bytes,
            observed,
            encoded_payloads,
            registration: self.registration,
            receipt: None,
        })
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn completed_window_encoder(
    preferred: plurx_core::transcode::Encoder,
    vaapi_upload_proved: bool,
    plan: &plurx_core::transcode::ResolvedTranscode,
    _grid: plurx_core::transcode::VodFrameGrid,
) -> plurx_core::transcode::Encoder {
    use plurx_core::transcode::Encoder;
    if preferred == Encoder::Nvenc {
        Encoder::Nvenc
    } else if preferred == Encoder::Vaapi
        && vaapi_upload_proved
        && plan
            .completed_reconstructed_output(Encoder::Vaapi)
            .is_some()
    {
        // Any raster and cadence: the window's HEVC level follows them.
        Encoder::Vaapi
    } else {
        Encoder::Software
    }
}

#[cfg(target_os = "linux")]
pub(crate) struct RuntimeRecipe {
    pub(crate) tools: Arc<ToolSet>,
    pub(crate) source: Arc<crate::fragment_index_cluster::SourceFence>,
    pub(crate) source_offsets: Arc<tokio::sync::Semaphore>,
    pub(crate) source_tick: (u32, u32),
    pub(crate) native_bl: (u32, u32),
    /// The held demux origin the Encoding froze for its soundtrack.
    pub(crate) source_clock: plurx_core::transcode::VodSourceClock,
    pub(crate) preferences: plurx_core::transcode::dv_processing::DvPreferences,
    pub(crate) destination: plurx_core::transcode::dv_processing::DvDestination,
    pub(crate) mode: plurx_core::transcode::dv_processing::DvProcessingMode,
    pub(crate) encoder: plurx_core::transcode::Encoder,
    pub(crate) vaapi_hdr_policy: Option<plurx_core::transcode::VaapiHdrPolicy>,
    pub(crate) runtime_cache: PathBuf,
    pub(crate) shader_cache: Arc<tempfile::TempDir>,
    pub(crate) prepared: std::sync::Mutex<Option<(u32, VerifiedWindow)>>,
    pub(crate) failure_episode: DvDigest,
    pub(crate) preparation_budget: std::time::Duration,
    pub(crate) report_revision: std::sync::atomic::AtomicU64,
    pub(crate) published_receipt: std::sync::Mutex<
        Option<(
            plurx_core::transcode::dv_processing::DvProcessingReceipt,
            DvDigest,
            crate::vodencode::dv_report_lifetime::PublicationOwners,
        )>,
    >,
}

#[cfg(target_os = "linux")]
impl RuntimeRecipe {
    pub(crate) fn clear_published_window(&self) {
        self.published_receipt
            .lock()
            .expect("DV published receipt")
            .take();
    }
    pub(crate) fn effective_report(
        &self,
        plan: &plurx_core::transcode::dv_processing::DvProcessingPlan,
        session_id: &str,
        physical_owner: &Arc<()>,
    ) -> Option<plurx_core::transcode::dv_processing::DvEffectiveProcessingReport> {
        if episode_failed(&self.failure_episode) {
            return None;
        }
        let mut published = self.published_receipt.lock().expect("DV published receipt");
        let (receipt, shape, owners) = published.as_mut()?;
        let generation = owners.authorize(physical_owner, session_id)?;
        let mut receipt = receipt.clone();
        receipt.bind_publication(&generation, shape.clone()).ok()?;
        let report = receipt
            .hdr10_effective_report(
                &plurx_core::transcode::dv_processing::DvProductionRegistry,
                plan,
                &generation,
                shape,
            )
            .ok()
            .flatten()?;
        if owners.mark_report_logged(physical_owner, &generation) {
            tracing::info!(target: "plurxd::vodserve",
                effective_processing = %serde_json::to_string(&report).expect("bounded processing report"),
                "accepted HDR10 processing report");
        }
        Some(report)
    }

    pub(crate) fn published_window(
        &self,
        receipt: plurx_core::transcode::dv_processing::DvProcessingReceipt,
        encoded_payloads: &[DvDigest],
        plan_digest: &DvDigest,
        physical_owners: &[Arc<()>],
        report_revision: u64,
    ) {
        if !episode_failed(&self.failure_episode) {
            let shape =
                digest(&serde_json::to_vec(encoded_payloads).expect("encoded sample digest list"))
                    .expect("encoded sample shape");
            let mut published = self.published_receipt.lock().expect("DV published receipt");
            if self
                .report_revision
                .load(std::sync::atomic::Ordering::Acquire)
                != report_revision
            {
                return;
            }
            tracing::debug!(target: "plurxd::vodserve",
                destination = ?self.destination, processing_mode = ?self.mode,
                frames = receipt.served_frames().len(), el_frames = receipt.counts().el_accepted,
                nlq_frames = receipt.counts().operations_applied.get(
                    &plurx_core::transcode::dv_processing::DvOperation::LinearNlqResidual
                ).copied().unwrap_or(0),
                plan_digest = %plan_digest.as_str(), object_shape = %shape.as_str(),
                "DV processed window published");
            let mut owners = published
                .take()
                .map(|(_, _, owners)| owners)
                .unwrap_or_default();
            let accepted = owners.published(
                physical_owners,
                report_revision,
                self.report_revision
                    .load(std::sync::atomic::Ordering::Acquire),
            );
            debug_assert!(accepted, "publication revision checked under the same lock");
            published.replace((receipt, shape, owners));
        }
    }
    pub(crate) fn invalidate_report_owner(&self, physical: &Arc<()>) {
        let mut published = self.published_receipt.lock().expect("DV published receipt");
        self.report_revision
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if let Some((_, _, owners)) = published.as_mut() {
            owners.invalidate(physical);
        }
    }

    pub(crate) fn refuse_episode(&self) {
        fail_episode(&self.failure_episode);
        self.clear_published_window();
    }
    fn window(
        &self,
        entry: &plurx_core::segplan::PlanEntry,
        grid: plurx_core::transcode::VodFrameGrid,
    ) -> Result<crate::dv_segment::SegmentWindow, String> {
        fn subtract_tick(value: u64, grid: u32, tick: (u32, u32)) -> Result<(i64, u32), String> {
            let numerator = (i128::from(value) * i128::from(tick.1)
                - i128::from(tick.0) * i128::from(grid))
            .max(0);
            let denominator = u64::from(grid) * u64::from(tick.1);
            let mut a = numerator as u128;
            let mut b = u128::from(denominator);
            while b != 0 {
                (a, b) = (b, a % b);
            }
            let divisor = a.max(1);
            Ok((
                i64::try_from(numerator / divisor as i128)
                    .map_err(|_| "source window numerator overflow")?,
                u32::try_from(u128::from(denominator) / divisor)
                    .map_err(|_| "source window clock overflow")?,
            ))
        }
        let end = entry
            .start_ticks
            .checked_add(entry.duration_ticks)
            .ok_or("window endpoint overflow")?;
        Ok(crate::dv_segment::SegmentWindow {
            start: subtract_tick(entry.start_ticks, grid.numerator, self.source_tick)?,
            end: subtract_tick(end, grid.numerator, self.source_tick)?,
            max_preroll: 512,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute(
        &self,
        file: &plurx_core::domain::MediaFile,
        plan: &plurx_core::transcode::ResolvedTranscode,
        options: &plurx_core::transcode::TranscodeOptions,
        grid: plurx_core::transcode::VodFrameGrid,
        entry: &plurx_core::segplan::PlanEntry,
        encoder: &Path,
        admission: crate::dv_segment::SegmentAdmission,
        producer: Arc<crate::prodrun::ProducerSlot>,
        cancel: tokio_util::sync::CancellationToken,
        registration_hook: Option<RegistrationHook>,
    ) -> Result<
        (
            VerifiedWindow,
            plurx_core::transcode::dv_processing::DvProcessingPlan,
        ),
        String,
    > {
        self.execute_until(
            file,
            plan,
            options,
            grid,
            entry,
            encoder,
            admission,
            producer,
            cancel,
            registration_hook,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_until(
        &self,
        file: &plurx_core::domain::MediaFile,
        plan: &plurx_core::transcode::ResolvedTranscode,
        options: &plurx_core::transcode::TranscodeOptions,
        grid: plurx_core::transcode::VodFrameGrid,
        entry: &plurx_core::segplan::PlanEntry,
        encoder: &Path,
        admission: crate::dv_segment::SegmentAdmission,
        producer: Arc<crate::prodrun::ProducerSlot>,
        cancel: tokio_util::sync::CancellationToken,
        registration_hook: Option<RegistrationHook>,
        absolute_deadline: Option<tokio::time::Instant>,
    ) -> Result<
        (
            VerifiedWindow,
            plurx_core::transcode::dv_processing::DvProcessingPlan,
        ),
        String,
    > {
        if episode_failed(&self.failure_episode) {
            return Err(
                "DV processing previously failed for this playback/source/settings/backend".into(),
            );
        }
        let result = async {
            use plurx_core::transcode::dv_processing::{
                dv_plan_completed_window, DvCompletedWindowInput, DvPlaneRepresentation,
                DvPlaneShape, DvRpuState, DvSourceCoverage, DvSourceFacts,
            };
            use plurx_core::transcode::{Pacing, TranscodeExecution};
            let frames = usize::try_from(entry.duration_ticks / u64::from(grid.denominator))
                .map_err(|_| "window picture count overflow")?;
            if frames == 0
                || frames > 64
                || !entry
                    .duration_ticks
                    .is_multiple_of(u64::from(grid.denominator))
                || !entry
                    .start_ticks
                    .is_multiple_of(u64::from(grid.denominator))
                || !self.source.unchanged()
                || !self.tools.is_current().await
            {
                return Err("source/backend/window changed before processing".into());
            }
            let window = self.window(entry, grid)?;
            let start_seconds = entry.start_ticks as f64 / f64::from(grid.numerator);
            let end_seconds =
                (entry.start_ticks + entry.duration_ticks) as f64 / f64::from(grid.numerator);
            let mut options = options.clone();
            // Execution coordinates belong to the retained ordinary options;
            // media options intentionally omit source paths and execution times.
            options.start_seconds = start_seconds;
            let execution =
                TranscodeExecution::from_options(file, &options, Pacing::unpaced(), ".")
                    .map_err(|e| e.to_string())?;
            let output = plan
                .completed_reconstructed_output(self.encoder)
                .ok_or("unsupported reconstructed output encoder")?
                .with_vaapi_hdr_policy(self.vaapi_hdr_policy);
            let raster = plan
                .output_contract()
                .effective_width()
                .zip(plan.output_contract().effective_height())
                .ok_or("no immutable output raster")?;
            // Both destinations leave the GPU as 10-bit YCbCr at the output
            // raster: the encoder only uploads it, it never converts colour.
            let export_policy =
                plurx_core::transcode::dv_processing::DvExportPolicy::PqBilinearYuv420p10V1 {
                    raster,
                };
            let export_raster = export_policy
                .raster(self.native_bl)
                .map_err(|e| e.to_string())?;
            let args = plurx_core::transcode::vod_completed_reconstructed_pipe_args(
                file,
                &output,
                &execution,
                grid,
                end_seconds,
                "/proc/self/fd/3",
                "/proc/self/fd/4",
                self.source_clock,
            )
            .map_err(str::to_owned)?;
            tracing::debug!(target: "plurxd::dv", entry = entry.index, encoder = ?args,
                "processed window encoder recipe");
            let deadline = crate::media_pool::create_stage_deadline(self.preparation_budget);
            let deadline = absolute_deadline.map_or(deadline, |absolute| deadline.min(absolute));
            let mut renderer_env = self.tools.renderer_env.clone();
            renderer_env.extend([
                (
                    "MESA_SHADER_CACHE_DIR".into(),
                    self.shader_cache.path().as_os_str().to_owned(),
                ),
                ("MESA_SHADER_CACHE_MAX_SIZE".into(), "64M".into()),
            ]);
            gpu_decode_environment(
                &mut renderer_env,
                &plurx_core::transcode::vaapi_device(),
                &crate::ffmpeg::ffmpeg_bin(),
            );
            let request = crate::dv_segment::StreamingSegmentRequest {
                mode: self.mode,
                export_raster: Some(export_raster),
                request: crate::dv_segment::SegmentRequest {
                    renderer: self.tools.renderer.clone(),
                    renderer_env,
                    muxer: self.tools.renderer.clone(),
                    encoder: encoder.to_owned(),
                    encoder_args: args,
                    runtime_cache: self.runtime_cache.clone(),
                    shape: crate::dv_segment::SegmentShape {
                        video_index: u8::try_from(plan.decode().input_video_stream())
                            .map_err(|_| "video stream index exceeds renderer bound")?,
                        max_frames: u8::try_from(frames)
                            .map_err(|_| "picture count exceeds renderer bound")?,
                        bl: self.native_bl,
                        el: (0, 0),
                    },
                    source: self.source.clone(),
                    source_offsets: self.source_offsets.clone(),
                    admission,
                    producer,
                    at: entry.index,
                    preparation_deadline: deadline,
                    cancel,
                },
                renderer_deadline: deadline,
                window: Some(window),
            };
            let private = run_private_window(request, registration_hook).await?;
            let first_frame = entry.start_ticks / u64::from(grid.denominator);
            let observed = private.observe(
                &window,
                plan.decode().input_video_stream(),
                0,
                grid,
                first_frame,
                frames,
                self.mode,
                export_raster,
            )?;
            if observed.source_tick != self.source_tick
                || observed
                    .pictures
                    .iter()
                    .any(|picture| picture.bl_shape != self.native_bl)
            {
                return Err(
                    "renderer source clock or native geometry differs from held probe".into(),
                );
            }
            let backend = self
                .tools
                .identity
                .with_completed_encoder_policy(self.encoder, self.vaapi_hdr_policy)
                .with_runtime_device(&observed.runtime_device);
            let source_keys: Vec<_> = observed
                .pictures
                .iter()
                .map(|picture| picture.key.clone())
                .collect();
            let source_rpus: Vec<_> = observed
                .pictures
                .iter()
                .map(|picture| picture.rpu.clone())
                .collect();
            let el = observed
                .pictures
                .first()
                .ok_or("no rendered picture")?
                .el_shape;
            if observed
                .pictures
                .iter()
                .any(|picture| picture.el_shape != el)
            {
                return Err("native EL shape changes within window".into());
            }
            let first = observed
                .pictures
                .first()
                .ok_or("no observed source frame")?;
            if observed.pictures.iter().any(|picture| {
                picture.profile != first.profile || picture.source_el_kind != first.source_el_kind
            }) {
                return Err("source RPU profile or EL kind changes within the window".into());
            }
            let full_fel = self.mode == plurx_core::transcode::dv_processing::DvProcessingMode::Fel;
            let source = DvSourceFacts::new(
                file.dolby_vision,
                plan.decode().input_video_stream(),
                plan.observed_source_identity().clone(),
                plan.cache_identity().clone(),
                plan.source_binding(),
                backend.parser_identity().clone(),
                DvRpuState::ValidatedFresh,
                first.source_el_kind,
                Some(DvSourceCoverage::sampled(0, source_keys.clone()).map_err(|e| e.to_string())?),
                Some(
                    DvPlaneShape::new(
                        self.native_bl.0,
                        self.native_bl.1,
                        if full_fel {
                            DvPlaneRepresentation::P7BaseYuv420P10Limited
                        } else if first.profile == 5 {
                            DvPlaneRepresentation::DoviBaseP10Full
                        } else {
                            DvPlaneRepresentation::DoviBaseP10Limited
                        },
                    )
                    .map_err(|e| e.to_string())?,
                ),
                if full_fel {
                    Some(
                        DvPlaneShape::new(el.0, el.1, DvPlaneRepresentation::P7ElYuv420P10Residual)
                            .map_err(|e| e.to_string())?,
                    )
                } else {
                    None
                },
            )
            .map_err(|e| e.to_string())?;
            let raster = plan
                .output_contract()
                .effective_width()
                .zip(plan.output_contract().effective_height())
                .ok_or("no immutable output raster")?;
            let trace = observed.source_trace.clone();
            let decoder = observed.decoder;
            let mut verified = private
                .finalize(
                    observed,
                    grid,
                    first_frame,
                    raster,
                    self.destination,
                    file.dolby_vision
                        .level
                        .and_then(|level| u8::try_from(level).ok()),
                    &self.runtime_cache,
                    plan.options()
                        .input_has_audio
                        .then_some(plan.options().audio_channels),
                )
                .await?;
            if !self.source.unchanged() || !self.tools.is_current().await {
                return Err("source/backend changed before publication validation".into());
            }
            let selected = dv_plan_completed_window(DvCompletedWindowInput {
                source,
                preferences: self.preferences,
                backend,
                destination: self.destination,
                encoder: self.encoder,
                mode: self.mode,
                source_keys: &source_keys,
                source_rpus: &source_rpus,
                source_tick: self.source_tick,
                grid,
                first_global_frame: first_frame,
                encoded: &verified.bytes,
                raster,
                export_policy,
                source_trace: trace,
                decoder,
                observed_at: i64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_err(|e| e.to_string())?
                        .as_secs(),
                )
                .map_err(|_| "runtime clock overflow")?,
            })
            .map_err(|e| e.to_string())?;
            let durations: Vec<_> = verified
                .observed
                .pictures
                .iter()
                .map(|picture| picture.duration)
                .collect();
            verified.receipt = Some(
                plurx_core::transcode::dv_processing::dv_receipt_completed_window(
                    &selected,
                    &source_keys,
                    &durations,
                    &source_rpus,
                )
                .map_err(|e| e.to_string())?,
            );
            Ok((verified, selected))
        }
        .await;
        // Record before the caller wakes failed waiters: a rapid fresh create
        // must already see the ordinary-route exclusion.
        if result.is_err() {
            self.refuse_episode();
        }
        result
    }
}

/// The stdout spool is private and bounded. Neither init nor a media sample is
/// visible to VOD until helper completion, exact encoder success and confirmed
/// owned-job retirement have all been observed. Cancellation drops the pipe
/// owner and retires the same registered process, never a replacement PID.
#[cfg(target_os = "linux")]
pub(crate) async fn run_private_window(
    mut request: crate::dv_segment::StreamingSegmentRequest,
    registration_hook: Option<RegistrationHook>,
) -> Result<PrivateWindow, String> {
    use std::time::Duration;
    use tokio::io::AsyncReadExt;
    const OUTPUT_LIMIT: u64 = 64 * 1024 * 1024;
    let slot = request.request.producer.clone();
    let cancel = request.request.cancel.child_token();
    let _cancel_on_drop = cancel.clone().drop_guard();
    request.request.cancel = cancel.clone();
    let deadline = request.renderer_deadline;
    let mut producer = crate::dv_segment::spawn_streaming(request).await?;
    let evidence = producer
        .take_evidence()
        .ok_or("missing owned renderer evidence")?;
    let helper = producer
        .take_helper_completion()
        .ok_or("missing helper completion")?;
    let (registration, writers, stdout, stderr) = producer.into_parts();
    if let Some(registered) = registration_hook {
        registered(&registration);
    }
    let result = {
        let capture = async {
            let output = async {
                let mut bytes = Vec::new();
                stdout
                    .take(OUTPUT_LIMIT + 1)
                    .read_to_end(&mut bytes)
                    .await
                    .map_err(|e| e.to_string())?;
                // Only an over-limit spool fails here: the reader stopped,
                // so the encoder can never finish. An empty spool waits for
                // the helper and encoder outcomes, which name the real cause.
                if bytes.len() as u64 > OUTPUT_LIMIT {
                    return Err("DV encoder private-output bound exceeded".to_owned());
                }
                Ok(bytes)
            };
            let diagnostic = async {
                let mut text = Vec::new();
                stderr
                    .take(64 * 1024 + 1)
                    .read_to_end(&mut text)
                    .await
                    .map_err(|e| e.to_string())?;
                if text.len() > 64 * 1024 {
                    return Err("DV encoder diagnostic bound exceeded".to_owned());
                }
                Ok(text)
            };
            let completed_helper = async {
                helper
                    .await
                    .map_err(|_| "DV helper completion owner lost".to_owned())?
            };
            let exit = async {
                loop {
                    if let Some(status) = slot
                        .try_wait_registered(&registration)
                        .await
                        .map_err(|e| e.to_string())?
                    {
                        return Ok(status);
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            };
            let (bytes, diagnostic, (), status) =
                tokio::try_join!(output, diagnostic, completed_helper, exit)?;
            private_window_outcome(bytes, &diagnostic, status)
        };
        tokio::pin!(capture);
        tokio::select! {
            result = &mut capture => result,
            _ = cancel.cancelled() => Err("DV private window cancelled".into()),
            _ = tokio::time::sleep_until(deadline) => Err("DV private window deadline expired".into()),
        }
    };
    writers.settled();
    slot.request_registered_retirement(&registration)
        .await
        .map_err(|e| e.to_string())?;
    registration.wait_confirmed_reap().await;
    Ok(PrivateWindow {
        encoded: result?,
        evidence,
        registration,
    })
}

/// Decide a completed private window from its encoder outcome. The helper's
/// own refusal already failed the join; what remains is the encoder's exit
/// status, its last diagnostic line and whether it wrote anything.
#[cfg(target_os = "linux")]
fn private_window_outcome(
    bytes: Vec<u8>,
    diagnostic: &[u8],
    status: std::process::ExitStatus,
) -> Result<Vec<u8>, String> {
    if !status.success() {
        return Err(match encoder_diagnostic_tail(diagnostic) {
            Some(line) => format!("DV encoder failed: {status}: {line}"),
            None => format!("DV encoder failed: {status}"),
        });
    }
    if bytes.is_empty() {
        return Err(match encoder_diagnostic_tail(diagnostic) {
            Some(line) => format!("DV encoder exited cleanly with no output: {line}"),
            None => "DV encoder exited cleanly with no output".to_owned(),
        });
    }
    Ok(bytes)
}

/// The last ffmpeg message that is not a `-progress` key=value line, bounded
/// for a log/plan note.
#[cfg(any(target_os = "linux", test))]
fn encoder_diagnostic_tail(diagnostic: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(diagnostic);
    let line = text.lines().rev().map(str::trim).find(|line| {
        !line.is_empty()
            && !line.split_once('=').is_some_and(|(key, _)| {
                !key.is_empty()
                    && key
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            })
    })?;
    Some(line.chars().take(300).collect())
}

#[cfg(target_os = "linux")]
#[derive(Deserialize)]
struct PairRow {
    frame: u8,
    pts: String,
    duration: String,
    bl_sha256: Option<String>,
    el_sha256: Option<String>,
    rpu_sha256: String,
    bl_width: u32,
    bl_height: u32,
    profile: Option<u8>,
    fel_contributed: Option<bool>,
    #[serde(default)]
    el_width: u32,
    #[serde(default)]
    el_height: u32,
}
#[cfg(target_os = "linux")]
#[derive(Deserialize)]
struct RenderRow {
    frame: u8,
    pts: String,
    duration: String,
    rgb_sha256: Option<String>,
    yuv10_sha256: Option<String>,
    width: u32,
    height: u32,
    el_bound: bool,
    nlq_active: bool,
    render_errors: u32,
    profile: Option<u8>,
    fel_contributed: Option<bool>,
    creative_trims_applied: Option<bool>,
    polynomial_segments: Option<u32>,
    mmr_segments: Option<u32>,
    applied_operations: Option<Vec<plurx_core::transcode::dv_processing::DvOperation>>,
}
#[cfg(target_os = "linux")]
#[derive(Deserialize)]
struct CodedRow {
    pts_ticks: i64,
    duration_ticks: i64,
    packet_sha256: String,
    rpu_sha256: String,
}
#[cfg(target_os = "linux")]
#[derive(Deserialize, serde::Serialize)]
struct GpuRuntime {
    api_version: u32,
    vendor_id: u32,
    device_id: u32,
    driver_version: u32,
    device_type: u32,
    device_uuid: String,
    driver_uuid: String,
}
#[cfg(target_os = "linux")]
#[derive(Deserialize)]
struct WindowComplete {
    requested_start: String,
    requested_end: String,
    source_origin: String,
    origin_provenance: String,
    frames: usize,
    preroll_pairs: usize,
    paired_pictures: usize,
    boundary_pts_ticks: i64,
    boundary_observed: bool,
    source_eof_observed: bool,
    first_emitted_pts_ticks: i64,
    last_emitted_end_ticks: i64,
    source_time_base: String,
    source_bytes_read: u64,
    membership: String,
}
#[cfg(target_os = "linux")]
pub(crate) struct ObservedPicture {
    pub(crate) key: plurx_core::transcode::dv_processing::DvFrameKey,
    pub(crate) duration: plurx_core::transcode::dv_processing::DvDuration,
    pub(crate) rpu: Vec<u8>,
    pub(crate) bl_shape: (u32, u32),
    pub(crate) el_shape: (u32, u32),
    pub(crate) profile: u8,
    pub(crate) source_el_kind: plurx_core::transcode::dv_processing::DvElKind,
}
#[cfg(target_os = "linux")]
pub(crate) struct ObservedWindow {
    pub(crate) pictures: Vec<ObservedPicture>,
    pub(crate) source_trace: DvDigest,
    pub(crate) source_tick: (u32, u32),
    pub(crate) runtime_device: DvDigest,
    /// The helper's own account of how it decoded the source pictures.
    pub(crate) decoder: plurx_core::transcode::DecodeBackend,
}
/// Read the helper's decoder report. A GPU decode that fell back (no device,
/// no HEVC Main10 decode, surfaces the Vulkan device could not import) is
/// still a valid window; the reason is logged so the fallback is visible.
#[cfg(target_os = "linux")]
fn helper_decoder(
    value: &serde_json::Value,
) -> Result<plurx_core::transcode::DecodeBackend, String> {
    use plurx_core::transcode::DecodeBackend;
    let mode = value
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .ok_or("decoder report has no mode")?;
    let reason = value
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let backend = match mode {
        "vaapi" | "vaapi-host-copy" => DecodeBackend::Vaapi,
        "software" | "vaapi-software-fallback" => DecodeBackend::Software,
        _ => return Err("decoder report names an unknown mode".into()),
    };
    if mode != "vaapi" && reason != "no-decode-device" {
        tracing::debug!(target: "plurxd::dv", mode, reason, "processed window decoder fallback");
    }
    Ok(backend)
}
#[cfg(target_os = "linux")]
fn rational(value: &str) -> Result<(i64, u32), String> {
    let (numerator, denominator) = value.split_once('/').ok_or("missing rational separator")?;
    let numerator = numerator.parse::<i64>().map_err(|e| e.to_string())?;
    let denominator = denominator.parse::<u32>().map_err(|e| e.to_string())?;
    if denominator == 0 {
        return Err("zero source time base".into());
    }
    Ok((numerator, denominator))
}
#[cfg(target_os = "linux")]
fn timestamp(value: &str) -> Result<plurx_core::transcode::dv_processing::DvTimestamp, String> {
    let (numerator, denominator) = rational(value)?;
    plurx_core::transcode::dv_processing::DvTimestamp::new(numerator, denominator)
        .map_err(|e| e.to_string())
}
#[cfg(target_os = "linux")]
fn tick_timestamp(
    ticks: i64,
    clock: (u32, u32),
) -> Result<plurx_core::transcode::dv_processing::DvTimestamp, String> {
    plurx_core::transcode::dv_processing::DvTimestamp::new(
        ticks
            .checked_mul(i64::from(clock.0))
            .ok_or("source clock overflow")?,
        clock.1,
    )
    .map_err(|e| e.to_string())
}
#[cfg(target_os = "linux")]
impl PrivateWindow {
    /// Validate the complete coded-input set independently of the emitted set.
    /// Raw RPUs are read through the held private directory and parsed again;
    /// JSON can describe observations but cannot replace those actual bytes.
    #[expect(
        clippy::too_many_arguments,
        reason = "observation independently binds source window, producer epoch and output grid"
    )]
    pub(crate) fn observe(
        &self,
        window: &crate::dv_segment::SegmentWindow,
        stream: u32,
        epoch: u64,
        grid: plurx_core::transcode::VodFrameGrid,
        first_frame: u64,
        frames: usize,
        mode: plurx_core::transcode::dv_processing::DvProcessingMode,
        export_raster: (u32, u32),
    ) -> Result<ObservedWindow, String> {
        use plurx_core::transcode::dv_processing::*;
        let events = self.evidence.renderer_events()?;
        let mut pairs = BTreeMap::new();
        let mut renders = BTreeMap::new();
        let mut coded = BTreeMap::new();
        let mut complete = None;
        let mut segment = None;
        let mut gpu = None;
        let mut base_complete = None;
        let mut decoder = None;
        for line in events
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let value: serde_json::Value =
                serde_json::from_slice(line).map_err(|e| e.to_string())?;
            match value.get("kind").and_then(serde_json::Value::as_str) {
                Some("gpu_runtime") => {
                    if gpu.is_some() {
                        return Err("duplicate Vulkan runtime identity".into());
                    }
                    gpu = Some(
                        serde_json::from_value::<GpuRuntime>(value).map_err(|e| e.to_string())?,
                    );
                }
                Some(kind @ ("accepted_source_pair" | "accepted_source_base")) => {
                    if (kind == "accepted_source_base") != (mode == DvProcessingMode::BaseRpu) {
                        return Err("source observation uses the wrong processing mode".into());
                    }
                    let row: PairRow = serde_json::from_value(value).map_err(|e| e.to_string())?;
                    if pairs.insert(row.frame, row).is_some() {
                        return Err("duplicate accepted source picture".into());
                    }
                }
                Some("rendered_frame") => {
                    let row: RenderRow =
                        serde_json::from_value(value).map_err(|e| e.to_string())?;
                    if renders.insert(row.frame, row).is_some() {
                        return Err("duplicate rendered source picture".into());
                    }
                }
                Some("coded_window_input") => {
                    let row: CodedRow = serde_json::from_value(value).map_err(|e| e.to_string())?;
                    if coded.insert(row.pts_ticks, row).is_some() {
                        return Err("duplicate coded source picture".into());
                    }
                }
                Some("window_complete") => {
                    if complete.is_some() {
                        return Err("duplicate source window completion".into());
                    }
                    complete = Some(
                        serde_json::from_value::<WindowComplete>(value)
                            .map_err(|e| e.to_string())?,
                    );
                }
                Some("decoder") => {
                    if decoder.replace(helper_decoder(&value)?).is_some() {
                        return Err("duplicate decoder report".into());
                    }
                }
                Some("segment_complete") => {
                    if segment.replace(value).is_some() {
                        return Err("duplicate renderer completion".into());
                    }
                }
                Some("base_processing_complete") => {
                    if base_complete.replace(value).is_some() {
                        return Err("duplicate base completion".into());
                    }
                }
                Some(_) => {}
                None => return Err("renderer event has no kind".into()),
            }
        }
        let complete = complete.ok_or("no source window completion")?;
        let segment = segment.ok_or("no renderer completion")?;
        let decoder = decoder.ok_or("no decoder report")?;
        if mode == DvProcessingMode::BaseRpu {
            let base = base_complete
                .as_ref()
                .ok_or("no base processing completion")?;
            if base.get("frames").and_then(serde_json::Value::as_u64) != Some(frames as u64)
                || base
                    .get("decoded_layers")
                    .and_then(serde_json::Value::as_u64)
                    != Some(1)
                || ["el_bound", "fel_contributed", "creative_trims_applied"]
                    .iter()
                    .any(|field| {
                        base.get(*field).and_then(serde_json::Value::as_bool) != Some(false)
                    })
            {
                return Err("base completion claims unsupported operations".into());
            }
        } else if base_complete.is_some() {
            return Err("FEL graph emitted base completion".into());
        }
        let gpu = gpu.ok_or("no observed Vulkan runtime identity")?;
        if gpu.api_version == 0
            || gpu.device_type > 4
            || [&gpu.device_uuid, &gpu.driver_uuid].iter().any(|uuid| {
                uuid.len() != 32
                    || !uuid
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        {
            return Err("invalid Vulkan runtime identity".into());
        }
        let runtime_device = digest(&serde_json::to_vec(&gpu).map_err(|e| e.to_string())?)?;
        let (tick_num, tick_den) = rational(&complete.source_time_base)?;
        let tick = (
            u32::try_from(tick_num).map_err(|_| "invalid source tick")?,
            tick_den,
        );
        if tick.0 == 0
            || complete.frames != frames
            || pairs.len() != frames
            || renders.len() != frames
            || coded.len() != frames
            || frames == 0
            || frames > 64
            || complete.preroll_pairs > usize::from(window.max_preroll)
            || complete.paired_pictures > usize::from(window.max_preroll) + frames + 1
            || complete.source_bytes_read > 128 * 1024 * 1024
            || complete.source_origin != "0/1"
            || complete.origin_provenance != "observed_stream_start_time"
            || complete.membership != "frame_pts_in_half_open_window_durations_unclipped"
            || timestamp(&complete.requested_start)?
                != DvTimestamp::new(window.start.0, window.start.1).map_err(|e| e.to_string())?
            || timestamp(&complete.requested_end)?
                != DvTimestamp::new(window.end.0, window.end.1).map_err(|e| e.to_string())?
            || segment.get("frames").and_then(serde_json::Value::as_u64) != Some(frames as u64)
            || segment
                .get("output_policy")
                .and_then(serde_json::Value::as_str)
                != Some("bt2020-pq-master-clip")
        {
            return Err("source window completion differs from owned request".into());
        }
        let end = DvTimestamp::new(window.end.0, window.end.1).map_err(|e| e.to_string())?;
        if !(complete.boundary_observed
            && tick_timestamp(complete.boundary_pts_ticks, tick)? >= end
            || complete.source_eof_observed
                && tick_timestamp(complete.last_emitted_end_ticks, tick)? >= end)
        {
            return Err("source window has no proved end boundary".into());
        }
        let mut pictures = Vec::with_capacity(frames);
        let mut trace = Vec::new();
        let timing = self.evidence.timing()?;
        let expected_timing = String::from_utf8(timing).map_err(|e| e.to_string())?;
        let mut observed_timing = String::new();
        for index in 0..frames {
            let index = u8::try_from(index).map_err(|_| "picture index overflow")?;
            let pair = pairs
                .remove(&index)
                .ok_or("accepted picture index missing")?;
            let rendered = renders
                .remove(&index)
                .ok_or("rendered picture index missing")?;
            let pts = timestamp(&pair.pts)?;
            let (duration_num, duration_den) = rational(&pair.duration)?;
            let duration = DvDuration::new(
                duration_num,
                duration_den,
                DvDurationProvenance::CodedPacketObserved,
            )
            .map_err(|e| e.to_string())?;
            let source = coded
                .values()
                .find(|row| tick_timestamp(row.pts_ticks, tick).is_ok_and(|value| value == pts))
                .ok_or("rendered picture absent from coded source trace")?;
            if source.duration_ticks <= 0
                || tick_timestamp(source.duration_ticks, tick)? != timestamp(&pair.duration)?
                || source.rpu_sha256 != pair.rpu_sha256
                || rendered.pts != pair.pts
                || rendered.duration != pair.duration
                || (rendered.width, rendered.height) != export_raster
                || rendered.el_bound != (mode == DvProcessingMode::Fel)
                || rendered.nlq_active != (mode == DvProcessingMode::Fel)
                || rendered.render_errors != 0
                || pair.bl_width == 0
                || pair.bl_height == 0
                || pair.bl_width > 4096
                || pair.bl_height > 2160
                || (mode == DvProcessingMode::Fel
                    && (pair.el_width == 0
                        || pair.el_height == 0
                        || !((pair.bl_width == pair.el_width && pair.bl_height == pair.el_height)
                            || (pair.el_width.checked_mul(2) == Some(pair.bl_width)
                                && pair.el_height.checked_mul(2) == Some(pair.bl_height)))))
                || (mode == DvProcessingMode::BaseRpu
                    && (pair.el_width != 0 || pair.el_height != 0))
            {
                return Err("source, decoded pair and rendered picture do not agree".into());
            }
            let rpu = self.evidence.rpu(index)?;
            if hash(&rpu) != pair.rpu_sha256 {
                return Err("raw RPU differs from source observation".into());
            }
            let (profile, source_el_kind) = if mode == DvProcessingMode::Fel {
                let parsed = DvParsedMetadata::from_raw_rpu(&rpu).map_err(|e| e.to_string())?;
                if !parsed.runtime_supported() {
                    return Err("unsupported runtime RPU metadata".into());
                }
                (7, DvElKind::Fel)
            } else {
                let parsed = DvBaseMetadata::from_raw_rpu(&rpu, (pair.bl_width, pair.bl_height))
                    .map_err(|e| e.to_string())?;
                let mut actual = parsed.applied_operations.clone();
                actual.retain(|operation| {
                    !matches!(
                        operation,
                        DvOperation::Bt2020NclConversion | DvOperation::Main10Encoding
                    )
                });
                if pair.profile != Some(parsed.profile)
                    || rendered.profile != Some(parsed.profile)
                    || pair.fel_contributed != Some(false)
                    || rendered.fel_contributed != Some(false)
                    || rendered.creative_trims_applied != Some(false)
                    || rendered.applied_operations.as_ref() != Some(&actual)
                    || rendered.polynomial_segments != Some(parsed.polynomial_segments)
                    || rendered.mmr_segments != Some(parsed.mmr_segments)
                    || base_complete
                        .as_ref()
                        .and_then(|value| value.get("profile"))
                        .and_then(serde_json::Value::as_u64)
                        != Some(u64::from(parsed.profile))
                {
                    return Err("base operations differ from actual raw RPU".into());
                }
                (parsed.profile, parsed.source_el_kind)
            };
            let key = DvFrameKey {
                absolute_video_index: stream,
                continuity_epoch: epoch,
                pts,
                display_ordinal: first_frame + u64::from(index),
            };
            DvDigest::new(source.packet_sha256.clone()).map_err(|e| e.to_string())?;
            trace.extend_from_slice(&stream.to_be_bytes());
            trace.extend_from_slice(&epoch.to_be_bytes());
            trace.extend_from_slice(&tick.0.to_be_bytes());
            trace.extend_from_slice(&tick.1.to_be_bytes());
            trace.extend_from_slice(&source.pts_ticks.to_be_bytes());
            trace.extend_from_slice(&source.duration_ticks.to_be_bytes());
            trace.extend_from_slice(source.packet_sha256.as_bytes());
            trace.extend_from_slice(source.rpu_sha256.as_bytes());
            observed_timing.push_str(&format!("{}\t{}\n", pair.pts, pair.duration));
            let optional_digest = |value: Option<String>| {
                value
                    .map(DvDigest::new)
                    .transpose()
                    .map_err(|e| e.to_string())
            };
            pictures.push(ObservedPicture {
                key,
                duration,
                rpu,
                bl_shape: (pair.bl_width, pair.bl_height),
                el_shape: (pair.el_width, pair.el_height),
                profile,
                source_el_kind,
            });
            optional_digest(pair.bl_sha256)?;
            optional_digest(pair.el_sha256)?;
            optional_digest(rendered.rgb_sha256)?;
            optional_digest(rendered.yuv10_sha256)?;
        }
        if observed_timing != expected_timing
            || pictures.first().map(|frame| frame.key.pts)
                != Some(tick_timestamp(complete.first_emitted_pts_ticks, tick)?)
        {
            return Err("private timing differs from the source trace".into());
        }
        let source_keys: Vec<_> = pictures.iter().map(|frame| frame.key.clone()).collect();
        dv_map_source_to_output_grid(&source_keys, tick, grid, first_frame, frames)
            .map_err(|e| e.to_string())?;
        Ok(ObservedWindow {
            pictures,
            source_trace: digest(&trace)?,
            source_tick: tick,
            runtime_device,
            decoder,
        })
    }
}

#[cfg(test)]
mod private_window_outcome_tests {
    use super::encoder_diagnostic_tail;

    #[test]
    fn diagnostic_tail_skips_progress_lines_and_is_bounded() {
        let stderr = b"[hevc_vaapi @ 0x1] Failed to create encode pipeline\n\
            Error while opening encoder\nframe=0\nfps=0.0\nprogress=end\n";
        assert_eq!(
            encoder_diagnostic_tail(stderr).as_deref(),
            Some("Error while opening encoder")
        );
        assert_eq!(encoder_diagnostic_tail(b"frame=1\nprogress=end\n"), None);
        let long = "x".repeat(1000);
        assert_eq!(encoder_diagnostic_tail(long.as_bytes()).unwrap().len(), 300);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn empty_or_failed_encoder_output_names_its_cause() {
        use super::private_window_outcome;
        use std::os::unix::process::ExitStatusExt;
        let ok = std::process::ExitStatus::from_raw(0);
        let failed = std::process::ExitStatus::from_raw(1 << 8);
        let stderr = b"Impossible to convert between the formats\nprogress=end\n";
        let error = private_window_outcome(Vec::new(), stderr, ok).unwrap_err();
        assert!(error.contains("no output"), "{error}");
        assert!(error.contains("Impossible to convert"), "{error}");
        assert!(!error.contains("bound exceeded"), "{error}");
        let error = private_window_outcome(vec![1], stderr, failed).unwrap_err();
        assert!(error.starts_with("DV encoder failed:"), "{error}");
        assert!(error.contains("Impossible to convert"), "{error}");
        assert_eq!(private_window_outcome(vec![7], b"", ok).unwrap(), vec![7]);
    }
}

#[cfg(test)]
mod processed_selection_has_no_software_gates {
    /// The processed HDR10 entry point: processing is the selected delivery
    /// once the Developer setting is on and the first window verifies. These
    /// were silent gates (720p clamp, speed qualification, Auto only) that
    /// kept HDR10-E from ever playing; they must not return.
    fn entry_body() -> &'static str {
        let source = include_str!("transcode/manager/create.rs");
        source
            .split_once("pub(crate) async fn prepare_processed_hdr10(")
            .expect("processed HDR10 entry point")
            .1
            .split_once("pub(super) async fn prepare_vod_encoding_with_preference(")
            .expect("entry point end")
            .0
    }

    #[test]
    fn no_resolution_clamp_speed_threshold_or_auto_only_gate() {
        let body = entry_body();
        assert!(!body.contains(".min(720)"), "resolution clamp is back");
        assert!(
            !body.contains("milli_realtime"),
            "speed qualification is back"
        );
        assert!(
            !body.contains("request.automatic"),
            "Auto-only gate is back"
        );
        // Every decline after the setting check is reported, never silent.
        let after_setting = body
            .split_once("// The setting is on for a DV source")
            .expect("setting check")
            .1;
        let reported = after_setting
            .split_once("let Ok(_preparation_slot)")
            .expect("resource stage")
            .0;
        assert!(!reported.contains("return Ok(None)"), "{reported}");
    }

    #[test]
    fn every_catalog_start_is_offered_processing() {
        let source = include_str!("http/hls/create.rs");
        let guard = source
            .split_once(".prepare_processed_hdr10(")
            .expect("processed call")
            .0;
        let guard = &guard[guard.rfind("    if ").expect("call guard")..];
        assert!(!guard.contains("candidate_auto_policy"), "{guard}");
        assert!(!guard.contains("continuous.is_none()"), "{guard}");
    }

    #[test]
    fn prepared_processing_admission_has_no_speed_proof() {
        let source = include_str!("transcode/manager/create.rs");
        let admit = source
            .split_once("admitted processed executable changed")
            .expect("prepared admission")
            .0;
        let admit = &admit[admit.len().saturating_sub(2500)..];
        assert!(!admit.contains("production_evidence"), "{admit}");
    }
}
