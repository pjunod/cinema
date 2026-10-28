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

#[derive(Debug)]
pub(crate) enum WindowOutcome<T> {
    Completed(T),
    Cancelled,
}

/// Capture retirement before our own cleanup retires this same registration.
/// A deliberate stop/restart is not evidence that this backend failed.
fn private_window_result<T>(
    result: Result<T, String>,
    cancel: &tokio_util::sync::CancellationToken,
    retirement: &tokio_util::sync::CancellationToken,
    helper: Option<crate::dv_segment::HelperOutcome>,
) -> Result<WindowOutcome<T>, String> {
    match helper {
        Some(crate::dv_segment::HelperOutcome::Failed(cause)) => {
            return Err(cause);
        }
        Some(crate::dv_segment::HelperOutcome::Cancelled) => return Ok(WindowOutcome::Cancelled),
        _ => {}
    }
    if cancel.is_cancelled() || retirement.is_cancelled() {
        Ok(WindowOutcome::Cancelled)
    } else {
        result.map(WindowOutcome::Completed)
    }
}

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
    pub(crate) scanner: PathBuf,
    pub(crate) renderer_env: Vec<(String, std::ffi::OsString)>,
    pub(crate) identity: DvBackendIdentity,
    /// The scanner executable's own digest. It is statically linked, so this
    /// is everything that decides a complete-title aggregate.
    scanner_sha256: String,
    artifacts: Arc<Vec<(PathBuf, String)>>,
}

#[cfg(all(test, target_os = "linux"))]
impl ToolSet {
    /// An explicitly controlled scanner result with the ordinary executable
    /// artifact-current check, used only by admitted lifecycle fixtures.
    pub(crate) fn lifecycle_fixture_with_scanner(
        identity: DvBackendIdentity,
        scanner: PathBuf,
    ) -> Result<Self, String> {
        let digest = hash_file(&scanner)?;
        let mut tools = Self::lifecycle_fixture(identity);
        tools.scanner = scanner.clone();
        tools.scanner_sha256 = digest.clone();
        tools.artifacts = Arc::new(vec![(scanner, digest)]);
        Ok(tools)
    }

    /// Lifecycle fixtures never launch this renderer: they hold the exclusive
    /// worker claim while exercising attachment of a checked completed plan.
    pub(crate) fn lifecycle_fixture(identity: DvBackendIdentity) -> Self {
        Self {
            renderer: PathBuf::from("/bin/false"),
            scanner: PathBuf::from("/bin/false"),
            renderer_env: Vec::new(),
            identity,
            scanner_sha256: hash(b"/bin/false"),
            artifacts: Arc::new(Vec::new()),
        }
    }
}
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn renderer_source_identity() -> String {
    let inputs: [(&str, &[u8]); 15] = [
        (
            "base_dv_renderer.h",
            include_bytes!("../../../tools/dv_processing/base_dv_renderer.h"),
        ),
        (
            "display_target.h",
            include_bytes!("../../../tools/dv_processing/display_target.h"),
        ),
        (
            "dm_closed_form.h",
            include_bytes!("../../../tools/dv_processing/dm_closed_form.h"),
        ),
        (
            "dv_dm.h",
            include_bytes!("../../../tools/dv_processing/dv_dm.h"),
        ),
        (
            "dv_metadata.h",
            include_bytes!("../../../tools/dv_processing/dv_metadata.h"),
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
            "scan_title.c",
            include_bytes!("../../../tools/dv_processing/scan_title.c"),
        ),
        (
            "segment_decode_render.c",
            include_bytes!("../../../tools/dv_processing/segment_decode_render.c"),
        ),
        (
            "title_constants.h",
            include_bytes!("../../../tools/dv_processing/title_constants.h"),
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
pub(crate) fn gpu_decode_environment(
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
    /// The whole processing backend: renderer, libraries, parser and helper
    /// sources. FEL verdicts are measurements of that renderer, so they are
    /// keyed by it.
    #[cfg(unix)]
    pub(crate) fn backend_digest(&self) -> String {
        hash(&serde_json::to_vec(&self.identity).expect("validated backend identity serializes"))
    }
    /// What a complete-title aggregate depends on: the scanner executable and
    /// nothing else. Binding title demand to the whole backend re-queued every
    /// Dolby Vision title whenever the renderer, a shader or libplacebo changed.
    #[cfg(unix)]
    pub(crate) fn title_scanner_digest(&self) -> String {
        hash(format!("plurx-dv-title-scanner-v1\0{}", self.scanner_sha256).as_bytes())
    }
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
                || manifest.graph != "dv-native-master-or-closed-form-dm-v1"
                || manifest.abi.get("libplacebo") != Some(&374)
                || !["avcodec", "avformat", "avutil"]
                    .iter()
                    .all(|key| manifest.abi.get(*key).is_some_and(|major| *major > 0))
                || !manifest.tools.contains_key("renderer")
                || !manifest.tools.contains_key("scanner")
                || manifest.tools.len() > 4
                || manifest.libraries.is_empty()
                || manifest.libraries.len() > 32
            {
                return Err("unsupported DV build identity".into());
            }
            let mut artifacts = Vec::new();
            let mut tools = BTreeMap::new();
            for (name, artifact) in &manifest.tools {
                if !matches!(name.as_str(), "renderer" | "muxer" | "author" | "scanner") {
                    return Err("unknown DV helper".into());
                }
                // Optional standalone tools do not participate in the actual
                // streaming renderer + daemon packet-authoring graph.
                if !matches!(name.as_str(), "renderer" | "scanner") {
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
            let scanner_sha256 = manifest
                .tools
                .get("scanner")
                .ok_or("missing scanner")?
                .sha256
                .clone();
            Ok(Some(Arc::new(Self {
                renderer: tools.remove("renderer").ok_or("missing renderer")?,
                scanner: tools.remove("scanner").ok_or("missing scanner")?,
                renderer_env: environment,
                identity,
                scanner_sha256,
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
    display_target: Option<plurx_core::transcode::dv_processing::DvDisplayTarget>,
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
        if self
            .display_target
            .is_some_and(|target| target.is_sdr() != (destination == DvDestination::Sdr))
            || destination == DvDestination::Profile81 && self.display_target.is_some()
        {
            return Err("display target and output grade differ".into());
        }
        let rpus: Vec<_> = observed
            .pictures
            .iter()
            .map(|picture| picture.rpu.clone())
            .collect();
        let origin =
            plurx_core::transcode::vod_reconstructed_video_origin(grid, first_global_frame)
                .map_err(str::to_owned)?;
        let bytes = match destination {
            DvDestination::Sdr => {
                plurx_core::transcode::dv_processing::dv_author_sdr_window(&self.encoded)
                    .map_err(|error| error.to_string())?
            }
            DvDestination::Hdr10 => match self.display_target {
                Some(target) => {
                    let metadata = target
                        .output_metadata()
                        .map_err(|error| error.to_string())?;
                    plurx_core::transcode::dv_processing::dv_author_hdr10_window(
                        &self.encoded,
                        metadata,
                    )
                    .map_err(|error| error.to_string())?
                }
                None => self.encoded,
            },
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
        let sdr = destination == DvDestination::Sdr;
        if video.get("width").and_then(serde_json::Value::as_u64) != Some(u64::from(raster.0))
            || video.get("height").and_then(serde_json::Value::as_u64) != Some(u64::from(raster.1))
            || [
                ("codec_name", "hevc"),
                ("profile", "Main 10"),
                ("pix_fmt", "yuv420p10le"),
                ("color_range", "tv"),
                ("color_space", if sdr { "bt709" } else { "bt2020nc" }),
                ("color_transfer", if sdr { "bt709" } else { "smpte2084" }),
                ("color_primaries", if sdr { "bt709" } else { "bt2020" }),
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
// `vaapi_upload_proved` is supplied by the host-level HDR10 passthrough
// probe; it is not a device-bound reconstructed-upload proof. The completed
// private window must still validate the actual upload/encode before serving.
pub(crate) fn completed_window_encoder(
    preferred: plurx_core::transcode::Encoder,
    vaapi_upload_proved: bool,
    plan: &plurx_core::transcode::ResolvedTranscode,
    _grid: plurx_core::transcode::VodFrameGrid,
) -> plurx_core::transcode::Encoder {
    use plurx_core::transcode::Encoder;
    if preferred == Encoder::Nvenc {
        Encoder::Nvenc
    } else if matches!(preferred, Encoder::Qsv | Encoder::Vaapi)
        && vaapi_upload_proved
        && plan
            .completed_reconstructed_output(Encoder::Vaapi)
            .is_some()
    {
        // An Intel Quick Sync preference can use the independently proved
        // VAAPI upload/encode graph for these reconstructed pictures.
        // Any raster and cadence: the window's HEVC level follows them.
        Encoder::Vaapi
    } else {
        Encoder::Software
    }
}

pub(crate) use crate::decode_facts::mastering::{
    source_mastering_from_frames, source_mastering_minimum_from_frames,
};
#[cfg(test)]
use crate::decode_facts::mastering::{source_mastering_metadata, source_mastering_minimum};

#[cfg(target_os = "linux")]
pub(crate) async fn held_source_minimum(
    stream: Result<Option<u32>, String>,
    probe: Option<&crate::decode_facts::SourceFrameProbe>,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
) -> Result<Option<u32>, String> {
    match stream? {
        Some(minimum) => Ok(Some(minimum)),
        None => match probe {
            Some(probe) => source_mastering_minimum_from_frames(
                &serde_json::Value::Null,
                probe.document(cancelled).await.map_err(|error| {
                    format!("targeted SDR held source frame probe failed: {error}")
                })?,
                probe.absolute_stream(),
            ),
            None => Ok(None),
        },
    }
}

/// Resolve absent stream MDCV through the lazy source-bound frame projection.
/// A malformed stream record refuses rather than being overwritten by a frame.
#[cfg(target_os = "linux")]
pub(crate) async fn held_source_mastering(
    stream: Result<Option<plurx_core::transcode::dv_processing::DvSourceMastering>, String>,
    probe: Option<&crate::decode_facts::SourceFrameProbe>,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
) -> Result<Option<plurx_core::transcode::dv_processing::DvSourceMastering>, String> {
    match stream? {
        Some(source) => Ok(Some(source)),
        None => match probe {
            Some(probe) => source_mastering_from_frames(
                &serde_json::Value::Null,
                probe.document(cancelled).await.map_err(|error| {
                    format!("targeted HDR10 held source frame probe failed: {error}")
                })?,
                probe.absolute_stream(),
            ),
            None => Ok(None),
        },
    }
}

#[cfg(target_os = "linux")]
pub(crate) struct RuntimeRecipe {
    pub(crate) tools: Arc<ToolSet>,
    pub(crate) source: Arc<crate::fragment_index_cluster::SourceFence>,
    pub(crate) source_offsets: Arc<tokio::sync::Semaphore>,
    pub(crate) source_tick: (u32, u32),
    pub(crate) source_origin: plurx_core::transcode::dv_processing::DvSourceOrigin,
    pub(crate) native_bl: (u32, u32),
    /// The held demux origin the Encoding froze for its soundtrack.
    pub(crate) source_clock: plurx_core::transcode::VodSourceClock,
    /// Selected held stream side data. Parse failures affect explicit targets
    /// only; default/master processing does not consume or invent these facts.
    pub(crate) source_mastering:
        Result<Option<plurx_core::transcode::dv_processing::DvSourceMastering>, String>,
    /// Independent SDR floor parse; invalid/absent HDR coordinates are irrelevant.
    pub(crate) source_mastering_minimum: Result<Option<u32>, String>,
    /// Lazy, source/probe/absolute-stream-bound metadata. Master and SDR paths
    /// with a title floor never invoke this separate frame projection.
    pub(crate) source_frame_probe: Option<Arc<crate::decode_facts::SourceFrameProbe>>,
    pub(crate) preferences: plurx_core::transcode::dv_processing::DvPreferences,
    pub(crate) display_target: Option<plurx_core::transcode::dv_processing::DvDisplayTarget>,
    pub(crate) display: Option<plurx_core::playback::caps::DisplayCaps>,
    pub(crate) display_target_note: Option<String>,
    pub(crate) destination: plurx_core::transcode::dv_processing::DvDestination,
    pub(crate) mode: plurx_core::transcode::dv_processing::DvProcessingMode,
    /// Where the renderer was admitted to decode; it sized the graph's CPU debit.
    pub(crate) decode: crate::dv_segment::RendererDecode,
    /// The node's live FEL reconstruction slot, held for this graph's life
    /// when it composes the enhancement layer.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "held for the graph's lifetime; dropping it frees the slot"
        )
    )]
    pub(crate) reconstruction_slot: Option<tokio::sync::OwnedSemaphorePermit>,
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
    /// Physical/internal callers resolve before constructing the export policy;
    /// a frozen live target is rechecked and cannot acquire another floor.
    pub(crate) async fn resolve_display_target(
        &self,
        target: plurx_core::transcode::dv_processing::DvDisplayTarget,
        cancelled: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<plurx_core::transcode::dv_processing::DvDisplayTarget, String> {
        if target.is_sdr() {
            target
                .resolved_sdr_floor()
                .map_err(|error| error.to_string())?;
            if self.display_target != Some(target) {
                return Err("SDR target differs from the held verified policy".into());
            }
            let minimum = if target.needs_source_floor() {
                held_source_minimum(
                    self.source_mastering_minimum.clone(),
                    self.source_frame_probe.as_deref(),
                    cancelled,
                )
                .await?
            } else {
                None
            };
            return target
                .resolve_sdr_floor(minimum)
                .map_err(|error| error.to_string());
        }
        let source = if target.needs_source_mastering() {
            held_source_mastering(
                self.source_mastering.clone(),
                self.source_frame_probe.as_deref(),
                cancelled,
            )
            .await?
        } else {
            None
        };
        target
            .resolve_source(source)
            .map_err(|error| error.to_string())
    }
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
            .effective_report(
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
                "accepted DV processing report");
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
        let end = entry
            .start_ticks
            .checked_add(entry.duration_ticks)
            .ok_or("window endpoint overflow")?;
        Ok(crate::dv_segment::SegmentWindow {
            start: self
                .source_origin
                .source_boundary(entry.start_ticks, grid.numerator, self.source_tick)
                .map_err(|e| e.to_string())?,
            end: self
                .source_origin
                .source_boundary(end, grid.numerator, self.source_tick)
                .map_err(|e| e.to_string())?,
            max_preroll: 512,
            source_origin: Some(self.source_origin),
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
        WindowOutcome<(
            VerifiedWindow,
            plurx_core::transcode::dv_processing::DvProcessingPlan,
        )>,
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
        WindowOutcome<(
            VerifiedWindow,
            plurx_core::transcode::dv_processing::DvProcessingPlan,
        )>,
        String,
    > {
        self.execute_policy_until(
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
            absolute_deadline,
            self.display_target,
        )
        .await
    }

    /// Explicit immutable policy; live preparation freezes the title-qualified
    /// target, ordinary VOD/pre-encode recipes retain None. No per-window
    /// environment/caps/settings selection occurs here.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_policy_until(
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
        display_target: Option<plurx_core::transcode::dv_processing::DvDisplayTarget>,
    ) -> Result<
        WindowOutcome<(
            VerifiedWindow,
            plurx_core::transcode::dv_processing::DvProcessingPlan,
        )>,
        String,
    > {
        use plurx_core::transcode::dv_processing::DvDestination;
        if display_target
            .is_some_and(|target| target.is_sdr() != (self.destination == DvDestination::Sdr))
            || self.destination == DvDestination::Profile81 && display_target.is_some()
            || self.destination == DvDestination::Sdr && display_target.is_none()
        {
            return Err("display target and output grade differ".into());
        }
        if episode_failed(&self.failure_episode) {
            return Err(
                "DV processing previously failed for this playback/source/settings/backend".into(),
            );
        }
        let display_target = match display_target {
            Some(target) => Some(self.resolve_display_target(target, Some(&cancel)).await?),
            None => None,
        };
        let cancellation = cancel.clone();
        let result: Result<_, String> = async {
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
            let mut output = plan
                .completed_reconstructed_output(self.encoder)
                .ok_or("unsupported reconstructed output encoder")?
                .with_vaapi_hdr_policy(self.vaapi_hdr_policy);
            if self.destination == DvDestination::Sdr {
                output = output.with_sdr_output();
            }
            if self.destination == DvDestination::Profile81 {
                output = output.with_reconstructed_profile81();
            }
            let raster = plan
                .output_contract()
                .effective_width()
                .zip(plan.output_contract().effective_height())
                .ok_or("no immutable output raster")?;
            // Both destinations leave the GPU as 10-bit YCbCr at the output
            // raster: the encoder only uploads it, it never converts colour.
            use plurx_core::transcode::dv_processing::DvExportPolicy;
            let export_policy = display_target
                .map_or(DvExportPolicy::PqBilinearYuv420p10V1 { raster }, |target| {
                    DvExportPolicy::DisplayTargetYuv420p10V1 { raster, target }
                });
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
                decode: self.decode,
                export_raster: Some(export_raster),
                display_target: export_policy.display_target(),
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
            let private = match run_private_window(request, registration_hook).await? {
                WindowOutcome::Completed(private) => private,
                WindowOutcome::Cancelled => return Ok(WindowOutcome::Cancelled),
            };
            let first_frame = entry.start_ticks / u64::from(grid.denominator);
            let observed = private.observe(
                &window,
                self.source_origin,
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
            // The graph's CPU debit assumed the GPU decodes. A renderer that
            // opened its decoders in software spent threads nobody reserved;
            // its window is not served.
            if self.decode == crate::dv_segment::RendererDecode::Gpu
                && observed.software_frame_workers
            {
                return Err(
                    "the renderer decoded in software, but this graph was admitted \
                     for GPU decode and holds no CPU for software decoders"
                        .into(),
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
                source_origin: self.source_origin,
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
            Ok(WindowOutcome::Completed((verified, selected)))
        }
        .await;
        // Return observations to the owner. Only its current generation or
        // admitted preparation can make a backend failure episode-wide.
        if cancellation.is_cancelled() {
            Ok(WindowOutcome::Cancelled)
        } else {
            result
        }
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
) -> Result<WindowOutcome<PrivateWindow>, String> {
    let slot = request.request.producer.clone();
    let cancel = request.request.cancel.child_token();
    let _cancel_on_drop = cancel.clone().drop_guard();
    request.request.cancel = cancel.clone();
    let deadline = request.renderer_deadline;
    let display_target = request.display_target;
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
    match capture_private_output(
        &slot,
        &registration,
        writers,
        stdout,
        stderr,
        helper,
        &cancel,
        deadline,
    )
    .await?
    {
        WindowOutcome::Completed(encoded) => Ok(WindowOutcome::Completed(PrivateWindow {
            encoded,
            evidence,
            registration,
            display_target,
        })),
        WindowOutcome::Cancelled => Ok(WindowOutcome::Cancelled),
    }
}

#[allow(clippy::too_many_arguments)]
async fn capture_private_output(
    slot: &crate::prodrun::ProducerSlot,
    registration: &crate::prodrun::ProducerRegistration,
    writers: crate::prodrun::ProducerWriters,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    helper: crate::dv_segment::HelperCompletion,
    cancel: &tokio_util::sync::CancellationToken,
    deadline: tokio::time::Instant,
) -> Result<WindowOutcome<Vec<u8>>, String> {
    use std::time::Duration;
    use tokio::io::AsyncReadExt;
    const OUTPUT_LIMIT: u64 = 64 * 1024 * 1024;
    let retirement = registration.retirement();
    let observed_helper = helper.clone();
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
                match helper.wait().await {
                    crate::dv_segment::HelperOutcome::Completed => Ok(()),
                    crate::dv_segment::HelperOutcome::Cancelled => {
                        Err("DV helper cancelled".into())
                    }
                    crate::dv_segment::HelperOutcome::Failed(cause) => Err(cause),
                }
            };
            let exit = async {
                loop {
                    if let Some(status) = slot
                        .try_wait_registered(registration)
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
            _ = retirement.cancelled() => Err("DV private window retired".into()),
            _ = tokio::time::sleep_until(deadline) => Err("DV private window deadline expired".into()),
        }
    };
    let result = private_window_result(result, cancel, &retirement, observed_helper.outcome());
    writers.settled();
    let cleanup = slot
        .request_registered_retirement(registration)
        .await
        .map_err(|e| e.to_string());
    // External retirement may have taken the exact child between capture and
    // cleanup. Keep the typed cancellation rather than reporting its missing
    // child as a fresh backend failure.
    if let Err(cause) = cleanup {
        if !matches!(&result, Ok(WindowOutcome::Cancelled)) {
            return Err(cause);
        }
    }
    registration.wait_confirmed_reap().await;
    let _ = observed_helper.wait().await;
    result
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
impl RenderRow {
    fn validate_fel_curves(&self, rpu: &[u8], raster: (u32, u32)) -> Result<(), String> {
        use plurx_core::transcode::dv_processing::DvFelMetadata;
        let parsed = DvFelMetadata::from_raw_rpu(rpu, raster).map_err(|e| e.to_string())?;
        if self.polynomial_segments != Some(parsed.polynomial_segments)
            || self.mmr_segments != Some(parsed.mmr_segments)
        {
            return Err("FEL curve counts differ from actual raw RPU".into());
        }
        Ok(())
    }
}
/// Exact requested policy and raw-RPU values, not membership in a supported set.
#[cfg(target_os = "linux")]
#[derive(Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
struct DisplayTargetRow {
    frame: u8,
    nits: u32,
    method: String,
    curve: String,
    max_pq: u16,
    avg_pq: u16,
    max0_fallback: bool,
    title_min_1e4: u32,
    title_peak: u32,
    trim_targets: usize,
    detail_weight: f64,
}
#[cfg(target_os = "linux")]
impl DisplayTargetRow {
    fn validate(
        &self,
        target: plurx_core::transcode::dv_processing::DvDisplayTarget,
        raw: &[u8],
    ) -> Result<(), String> {
        use plurx_core::transcode::dv_processing::DISPLAY_TARGET_METHOD;
        let (max, avg) = target.validate_rpu(raw).map_err(|e| e.to_string())?;
        // Family, trim selection and detail weight follow exactly from the raw RPU and bucket.
        let dm = target.display_management(raw).map_err(|e| e.to_string())?;
        let dm_ok = self.curve == dm.curve
            && self.trim_targets == dm.trim_targets
            && (self.detail_weight - dm.detail_weight).abs() <= 1e-4;
        if self.nits != target.nits()
            || self.method != DISPLAY_TARGET_METHOD
            || self.title_min_1e4 != target.mastering().min_1e4_nits
            || self.title_peak != target.mastering().max_nits
            || (self.max_pq, self.avg_pq) != (max, avg)
            || self.max0_fallback != (max == 0)
            || !dm_ok
        {
            return Err("display target trace differs from requested policy or raw L1".into());
        }
        Ok(())
    }
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
    /// Validated helper rows retained only for the ignored physical controls.
    #[cfg(test)]
    pub(crate) display_target_rows: Vec<serde_json::Value>,
    pub(crate) source_trace: DvDigest,
    pub(crate) source_tick: (u32, u32),
    pub(crate) runtime_device: DvDigest,
    /// The helper's own account of how it decoded the source pictures.
    pub(crate) decoder: plurx_core::transcode::DecodeBackend,
    /// The helper opened its decoders without a VA-API device, so each layer
    /// ran four software frame workers (`segment_decode_render.c`
    /// `open_layer`). A per-frame GPU fallback keeps one thread per layer and
    /// is not this.
    pub(crate) software_frame_workers: bool,
}
/// Read the helper's decoder report. A GPU decode that fell back (no device,
/// no HEVC Main10 decode, surfaces the Vulkan device could not import) is
/// still a valid window; the reason is logged so the fallback is visible.
#[cfg(target_os = "linux")]
pub(crate) fn helper_decoder(
    value: &serde_json::Value,
) -> Result<(plurx_core::transcode::DecodeBackend, bool), String> {
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
    Ok((backend, mode == "software"))
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
fn validate_window_origin(
    origin: &str,
    provenance: &str,
    expected: plurx_core::transcode::dv_processing::DvSourceOrigin,
) -> Result<(), String> {
    if provenance != "observed_held_format_and_selected_video_probe"
        || timestamp(origin)? != expected.timestamp()
    {
        return Err("source window origin differs from observed held common origin".into());
    }
    Ok(())
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
        source_origin: plurx_core::transcode::dv_processing::DvSourceOrigin,
        stream: u32,
        epoch: u64,
        grid: plurx_core::transcode::VodFrameGrid,
        first_frame: u64,
        frames: usize,
        mode: plurx_core::transcode::dv_processing::DvProcessingMode,
        export_raster: (u32, u32),
    ) -> Result<ObservedWindow, String> {
        use plurx_core::transcode::dv_processing::*;
        if window.source_origin != Some(source_origin) {
            return Err("window does not carry the frozen held common origin".into());
        }
        let events = self.evidence.renderer_events()?;
        let mut pairs = BTreeMap::new();
        let mut renders = BTreeMap::new();
        let mut targets = BTreeMap::new();
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
                Some("display_target_frame") => {
                    let row: DisplayTargetRow =
                        serde_json::from_value(value).map_err(|e| e.to_string())?;
                    if self.display_target.is_none() || targets.insert(row.frame, row).is_some() {
                        return Err("unexpected or duplicate display target trace".into());
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
        let (decoder, software_frame_workers) = decoder.ok_or("no decoder report")?;
        if mode == DvProcessingMode::BaseRpu {
            let base = base_complete
                .as_ref()
                .ok_or("no base processing completion")?;
            if base.get("frames").and_then(serde_json::Value::as_u64) != Some(frames as u64)
                || base
                    .get("decoded_layers")
                    .and_then(serde_json::Value::as_u64)
                    != Some(1)
                || ["el_bound", "fel_contributed"].iter().any(|field| {
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
        let requested_policy = self.display_target.map_or_else(
            || "bt2020-pq-master-clip".to_owned(),
            DvDisplayTarget::output_policy,
        );
        validate_window_origin(
            &complete.source_origin,
            &complete.origin_provenance,
            source_origin,
        )?;
        if tick.0 == 0
            || complete.frames != frames
            || pairs.len() != frames
            || renders.len() != frames
            || coded.len() != frames
            || targets.len()
                != if self.display_target.is_some() {
                    frames
                } else {
                    0
                }
            || frames == 0
            || frames > 64
            || complete.preroll_pairs > usize::from(window.max_preroll)
            || complete.paired_pictures > usize::from(window.max_preroll) + frames + 1
            || complete.source_bytes_read > 128 * 1024 * 1024
            || complete.membership != "frame_pts_in_half_open_window_durations_unclipped"
            || timestamp(&complete.requested_start)?
                != DvTimestamp::new(window.start.0, window.start.1).map_err(|e| e.to_string())?
            || timestamp(&complete.requested_end)?
                != DvTimestamp::new(window.end.0, window.end.1).map_err(|e| e.to_string())?
            || segment.get("frames").and_then(serde_json::Value::as_u64) != Some(frames as u64)
            || segment
                .get("output_policy")
                .and_then(serde_json::Value::as_str)
                != Some(requested_policy.as_str())
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
        #[cfg(test)]
        let display_target_rows = targets
            .values()
            .map(|row| serde_json::to_value(row).expect("integer target trace serializes"))
            .collect();
        let mut trace = serde_json::to_vec(&(
            "observed_held_common_stream_format_origin-v2",
            source_origin,
            complete.origin_provenance.as_str(),
            stream,
            epoch,
            window.start,
            window.end,
        ))
        .map_err(|e| e.to_string())?;
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
            if let Some(target) = self.display_target {
                targets
                    .remove(&index)
                    .ok_or("display target frame missing")?
                    .validate(target, &rpu)?;
            }
            let (profile, source_el_kind) = if mode == DvProcessingMode::Fel {
                rendered.validate_fel_curves(&rpu, (pair.bl_width, pair.bl_height))?;
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
                let mut trims = false;
                if let Some(target) = self.display_target {
                    actual.push(DvOperation::DisplayTargetMapping {
                        nits: target.nits(),
                    });
                    trims = target
                        .display_management(&rpu)
                        .map_err(|e| e.to_string())?
                        .creative_trims();
                    if trims {
                        actual.push(DvOperation::CreativeTrimApplication);
                    }
                }
                if pair.profile != Some(parsed.profile)
                    || rendered.profile != Some(parsed.profile)
                    || pair.fel_contributed != Some(false)
                    || rendered.fel_contributed != Some(false)
                    || rendered.creative_trims_applied != Some(trims)
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
        dv_map_source_to_output_grid(&source_keys, tick, source_origin, grid, first_frame, frames)
            .map_err(|e| e.to_string())?;
        Ok(ObservedWindow {
            pictures,
            #[cfg(test)]
            display_target_rows,
            source_trace: digest(&trace)?,
            source_tick: tick,
            runtime_device,
            decoder,
            software_frame_workers,
        })
    }
}

#[cfg(all(test, target_os = "linux"))]
mod fel_mapping_tests {
    #[test]
    fn fel_receipt_requires_curve_counts_from_the_original_raw_rpu() {
        let raw = include_bytes!("../../plurx-core/tests/fixtures/dv-runtime/frame-000.nal");
        let mut row: super::RenderRow = serde_json::from_value(serde_json::json!({
            "frame": 0, "pts": "0/24", "duration": "1/24",
            "width": 64, "height": 64, "el_bound": true,
            "nlq_active": true, "render_errors": 0,
            "polynomial_segments": 3, "mmr_segments": 0
        }))
        .expect("bounded render trace fixture");
        row.validate_fel_curves(raw, (64, 64))
            .expect("raw curves agree");
        row.polynomial_segments = None;
        assert!(row.validate_fel_curves(raw, (64, 64)).is_err());
        row.polynomial_segments = Some(3);
        row.mmr_segments = Some(1);
        assert!(row.validate_fel_curves(raw, (64, 64)).is_err());
        assert!(row.validate_fel_curves(&[], (64, 64)).is_err());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod source_origin_tests {
    use super::validate_window_origin;
    use plurx_core::transcode::{
        dv_processing::{DvSourceOrigin, DvTimestamp},
        VodSourceClock,
    };

    #[test]
    fn renderer_receipt_requires_exact_observed_held_common_origin() {
        for (origin, micros, equivalent) in [("0/1", 0, "0/1000"), ("1/8", 125000, "125/1000")] {
            let expected = DvSourceOrigin::observed_common(
                DvTimestamp::new(micros, 1000000).expect("valid expected held-format timestamp"),
                VodSourceClock::ReportedOrigin(micros),
            )
            .expect("valid observed common origin");
            for actual in [origin, equivalent] {
                validate_window_origin(
                    actual,
                    "observed_held_format_and_selected_video_probe",
                    expected,
                )
                .expect("matching held-origin receipt");
            }
            for wrong in ["1/7", "124/1000", "N/A", "1/0", "9223372036854775808/1"] {
                assert!(validate_window_origin(
                    wrong,
                    "observed_held_format_and_selected_video_probe",
                    expected
                )
                .is_err());
            }
            for provenance in [
                "",
                "unknown",
                "producer_default_zero",
                "first_window_picture",
                "observed_stream_start_time",
            ] {
                assert!(validate_window_origin(origin, provenance, expected).is_err());
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod helper_decoder_tests {
    use super::helper_decoder;
    use plurx_core::transcode::DecodeBackend;

    /// The retained P7 fixture's display-management selection, pinned so a parser or selection
    /// change cannot silently move it.
    fn fixture_dm(
        target: plurx_core::transcode::dv_processing::DvDisplayTarget,
        raw: &[u8],
    ) -> (&'static str, usize, f64) {
        let dm = target.display_management(raw).expect("fixture RPU parses");
        assert_eq!(
            (dm.cm4, dm.curve, dm.trim_targets, dm.detail_weight),
            FIXTURE_DM,
            "fixture selection moved"
        );
        (dm.curve, dm.trim_targets, dm.detail_weight)
    }
    /// The P7 fixture carries CM v4.0 metadata and no L8 trim, so its served mapping is untrimmed.
    const FIXTURE_DM: (bool, &str, usize, f64) = (true, "cm4-l1-only", 0, 0.0);
    fn other_family(curve: &str) -> &'static str {
        if curve.starts_with("cm4-") {
            "cm29-l1-only"
        } else {
            "cm4-l1-only"
        }
    }

    #[test]
    fn display_target_trace_requires_the_family_and_trims_of_the_raw_rpu() {
        use plurx_core::{scan::dv_title::*, transcode::dv_processing::*};
        let stats = DvTitleStats {
            schema: SCHEMA,
            complete: true,
            decoded_frames: 1,
            valid_l1_frames: 1,
            missing_l1_frames: 0,
            invalid_l1_frames: 0,
            max_l1_pq: Some(4095),
            mastering_luminance: TitleConstant::Unknown,
            mastering_primaries: TitleConstant::Unknown,
        };
        let DvDisplayTargetChoice::Target(target) =
            DvDisplayTarget::for_title(1000, Some(stats)).expect("title")
        else {
            panic!("target");
        };
        let raw = include_bytes!("../../../tools/dv_processing/controls/tags/p7-frame0.nal");
        let (max, avg) = target.validate_rpu(raw).expect("retained L1");
        let (curve, trims, weight) = fixture_dm(target, raw);
        let row = |curve: &str, trims: usize, weight: f64| {
            serde_json::from_value::<super::DisplayTargetRow>(serde_json::json!({
                "frame":0,"nits":1000,"method":DISPLAY_TARGET_METHOD,"curve":curve,
                "max_pq":max,"avg_pq":avg,"max0_fallback":max==0,"title_min_1e4":50,
                "title_peak":4000,"trim_targets":trims,"detail_weight":weight}))
            .expect("trace")
        };
        assert!(row(curve, trims, weight).validate(target, raw).is_ok());
        for (curve, trims, weight) in [
            // trims are not applied to served output, whatever the RPU carries
            ("cm4-l8-interpolated", 1, 0.0),
            ("cm4-l1-only", trims + 1, weight),
            ("cm4-l1-only", trims, 0.25),
            // the family follows the RPU
            ("cm29-l1-only", trims, weight),
            ("cm29-l2-exact", trims + 1, weight),
            ("cm4-unknown", trims, weight),
        ] {
            assert!(
                row(curve, trims, weight).validate(target, raw).is_err(),
                "{curve}"
            );
        }
    }

    #[test]
    fn display_target_trace_binds_exact_bucket_endpoints_and_raw_l1() {
        use plurx_core::{scan::dv_title::*, transcode::dv_processing::*};
        let stats = DvTitleStats {
            schema: SCHEMA,
            complete: true,
            decoded_frames: 1,
            valid_l1_frames: 1,
            missing_l1_frames: 0,
            invalid_l1_frames: 0,
            max_l1_pq: Some(4095),
            mastering_luminance: TitleConstant::Unknown,
            mastering_primaries: TitleConstant::Unknown,
        };
        let DvDisplayTargetChoice::Target(target) = DvDisplayTarget::for_title(1600, Some(stats))
            .expect("1600-nit bucket and complete high-peak title")
        else {
            panic!("target");
        };
        let raw = include_bytes!("../../../tools/dv_processing/controls/tags/p7-frame0.nal");
        let (max, avg) = target
            .validate_rpu(raw)
            .expect("retained P7 fixture has valid L1 for the target title");
        let (curve, trims, weight) = fixture_dm(target, raw);
        let valid = serde_json::json!({
            "frame":0, "nits":1600, "method":DISPLAY_TARGET_METHOD,
            "curve":curve, "max_pq":max, "avg_pq":avg,
            "max0_fallback":max == 0, "title_min_1e4":50, "title_peak":4000,
            "trim_targets":trims, "detail_weight":weight,
        });
        let parse = |row| {
            serde_json::from_value::<super::DisplayTargetRow>(row)
                .expect("synthetic target trace has the expected JSON shape")
        };
        assert!(parse(valid.clone()).validate(target, raw).is_ok());
        // Both 600 and 1600 are allowed. Only the requested one is admissible.
        for (field, value) in [
            ("nits", serde_json::json!(600)),
            ("title_min_1e4", serde_json::json!(51)),
            ("title_peak", serde_json::json!(1000)),
            ("max_pq", serde_json::json!(max ^ 1)),
            ("avg_pq", serde_json::json!(avg ^ 1)),
            ("max0_fallback", serde_json::json!(max != 0)),
            ("method", serde_json::json!("libplacebo-st2094-10")),
            ("curve", serde_json::json!("annex-b2-rgb")),
            ("curve", serde_json::json!(other_family(curve))),
            ("trim_targets", serde_json::json!(trims + 1)),
            ("detail_weight", serde_json::json!(1.5)),
        ] {
            let mut forged = valid.clone();
            forged[field] = value;
            assert!(parse(forged).validate(target, raw).is_err(), "{field}");
        }
        assert!(parse(valid).validate(target, b"not an RPU").is_err());
    }

    #[test]
    fn resolved_source_floor_is_required_by_the_helper_trace() {
        use plurx_core::{scan::dv_title::*, transcode::dv_processing::*};
        let stats = DvTitleStats {
            schema: SCHEMA,
            complete: true,
            decoded_frames: 1,
            valid_l1_frames: 1,
            missing_l1_frames: 0,
            invalid_l1_frames: 0,
            max_l1_pq: Some(4095),
            mastering_luminance: TitleConstant::Unknown,
            mastering_primaries: TitleConstant::Unknown,
        };
        let DvDisplayTargetChoice::Target(target) =
            DvDisplayTarget::for_title(1600, Some(stats)).expect("title")
        else {
            panic!("target");
        };
        let target = target
            .resolve_source(Some(DvSourceMastering {
                primaries_rgbw: [34000, 16000, 13250, 34500, 7500, 3000, 15635, 16450],
                min_1e4_nits: 17,
            }))
            .expect("source floor");
        assert_eq!(target.endpoint_args(), ["17", "4000"]);
        let raw = include_bytes!("../../../tools/dv_processing/controls/tags/p7-frame0.nal");
        let (max, avg) = target.validate_rpu(raw).expect("retained L1");
        let (curve, trims, weight) = fixture_dm(target, raw);
        let mut row = serde_json::json!({"frame":0,"nits":1600,"method":DISPLAY_TARGET_METHOD,
            "curve":curve,"max_pq":max,"avg_pq":avg,"max0_fallback":max==0,
            "title_min_1e4":17,"title_peak":4000,"trim_targets":trims,"detail_weight":weight});
        let parse = |row| serde_json::from_value::<super::DisplayTargetRow>(row).expect("trace");
        assert!(parse(row.clone()).validate(target, raw).is_ok());
        row["title_min_1e4"] = 50.into();
        assert!(
            parse(row).validate(target, raw).is_err(),
            "old default floor cannot describe source17 execution"
        );
    }

    #[test]
    fn the_helper_decoder_report_names_the_backend() {
        // Only a helper that opened without a device runs software frame
        // workers; that is what a GPU-decode admission did not debit.
        for (mode, backend, workers) in [
            ("vaapi", DecodeBackend::Vaapi, false),
            ("vaapi-host-copy", DecodeBackend::Vaapi, false),
            ("vaapi-software-fallback", DecodeBackend::Software, false),
            ("software", DecodeBackend::Software, true),
        ] {
            let row = serde_json::json!({"kind": "decoder", "mode": mode, "reason": ""});
            assert_eq!(helper_decoder(&row), Ok((backend, workers)), "{mode}");
        }
        assert!(helper_decoder(&serde_json::json!({"kind": "decoder"})).is_err());
        assert!(helper_decoder(&serde_json::json!({"kind": "decoder", "mode": "cuda"})).is_err());
    }
}

#[cfg(test)]
mod private_window_outcome_tests {
    use super::encoder_diagnostic_tail;

    async fn capture_owned_graph(
        encoder_fails: bool,
        helper_fails: bool,
        externally_retire: bool,
        timeout: bool,
        caller_cancel: bool,
    ) -> Result<super::WindowOutcome<Vec<u8>>, String> {
        use std::process::Stdio;
        use tokio_util::sync::CancellationToken;
        let slot = std::sync::Arc::new(crate::prodrun::ProducerSlot::new());
        let mut encoder = tokio::process::Command::new(if encoder_fails {
            "/bin/false"
        } else {
            "/bin/sleep"
        });
        if !encoder_fails {
            encoder.arg("30");
        }
        let mut child = encoder
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("actual encoder");
        let stdout = child.stdout.take().expect("encoder output");
        let stderr = child.stderr.take().expect("encoder diagnostic");
        let job = crate::process_control::ChildJob::attach(&child).expect("child job");
        let (registration, writers) = slot.attach_registered_job_owned(child, job, 0, None).await;
        let helper_cancel = CancellationToken::new();
        let helper_work_cancel = helper_cancel.clone();
        let cancel = CancellationToken::new();
        let encoder_retirement = registration.retirement();
        let mut helper_command = tokio::process::Command::new(if helper_fails {
            "/bin/false"
        } else if encoder_fails {
            "/bin/true"
        } else {
            "/bin/sleep"
        });
        if !helper_fails && !encoder_fails {
            helper_command.arg("30");
        }
        let mut helper_child = helper_command.spawn().expect("actual renderer helper");
        let helper = async move {
            let status = tokio::select! {
                status = helper_child.wait() => status.map_err(|error| error.to_string())?,
                _ = helper_work_cancel.cancelled() => {
                    if caller_cancel { encoder_retirement.cancelled().await; }
                    let _ = helper_child.start_kill();
                    helper_child.wait().await.map_err(|error| error.to_string())?;
                    return Err("renderer interrupted".into());
                }
            };
            if status.success() {
                Ok(())
            } else {
                Err(format!("renderer helper failed: {status}"))
            }
        };
        let completion = crate::dv_segment::supervise_streaming_helper(
            helper,
            helper_cancel,
            cancel.clone(),
            std::sync::Arc::downgrade(&slot),
            registration.clone(),
        );
        let observed = completion.clone();
        if caller_cancel {
            cancel.cancel();
        }
        let close = externally_retire.then(|| {
            let slot = slot.clone();
            let registration = registration.clone();
            tokio::spawn(async move {
                tokio::task::yield_now().await;
                slot.request_registered_retirement(&registration)
                    .await
                    .expect("explicit Close retires exact encoder");
            })
        });
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_millis(if timeout { 30 } else { 2000 });
        let result = super::capture_private_output(
            &slot,
            &registration,
            writers,
            stdout,
            stderr,
            completion,
            &cancel,
            deadline,
        )
        .await;
        if let Some(close) = close {
            close.await.expect("Close owner completes");
        }
        assert!(
            registration.confirmed_reap().is_some(),
            "capture settles the original process owner"
        );
        assert!(
            observed.outcome().is_some(),
            "helper supervisor settles before capture returns"
        );
        result
    }

    #[tokio::test]
    async fn explicit_close_is_normal_graph_cancellation() {
        assert!(matches!(
            capture_owned_graph(false, false, true, false, false).await,
            Ok(super::WindowOutcome::Cancelled)
        ));
    }

    #[tokio::test]
    async fn helper_failure_retirement_preserves_backend_cause() {
        let error = capture_owned_graph(false, true, false, false, false)
            .await
            .expect_err("failed helper graph");
        assert!(error.contains("renderer helper failed"), "{error}");
    }

    #[tokio::test]
    async fn encoder_failure_preserves_backend_cause() {
        let error = capture_owned_graph(true, false, false, false, false)
            .await
            .expect_err("failed encoder graph");
        assert!(error.starts_with("DV encoder failed:"), "{error}");
    }

    #[tokio::test]
    async fn graph_timeout_is_a_failure_and_reaps_both_children() {
        let error = capture_owned_graph(false, false, false, true, false)
            .await
            .expect_err("bounded graph");
        assert_eq!(error, "DV private window deadline expired");
    }

    #[tokio::test]
    async fn caller_cancel_retires_encoder_before_joining_blocked_helper() {
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            capture_owned_graph(false, false, false, false, true),
        )
        .await
        .expect("cancellation cannot deadlock helper join");
        assert!(matches!(outcome, Ok(super::WindowOutcome::Cancelled)));
    }

    #[tokio::test]
    async fn helper_supervisor_cancellation_does_not_wait_for_encoder_backpressure() {
        use tokio_util::sync::CancellationToken;
        let slot = std::sync::Arc::new(crate::prodrun::ProducerSlot::new());
        let child = tokio::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("encoder");
        let job = crate::process_control::ChildJob::attach(&child).expect("child job");
        let (registration, writers) = slot.attach_registered_job_owned(child, job, 0, None).await;
        let helper_cancel = CancellationToken::new();
        let work_cancel = helper_cancel.clone();
        let retirement = registration.retirement();
        let mut helper_child = tokio::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("blocked renderer");
        let helper = async move {
            work_cancel.cancelled().await;
            // Model a renderer whose write cannot settle while its encoder
            // still owns the pipe. Only supervisor retirement can unblock it.
            retirement.cancelled().await;
            let _ = helper_child.start_kill();
            helper_child
                .wait()
                .await
                .map_err(|error| error.to_string())?;
            Err("renderer interrupted".into())
        };
        let cancel = CancellationToken::new();
        let completed = crate::dv_segment::supervise_streaming_helper(
            helper,
            helper_cancel,
            cancel.clone(),
            std::sync::Arc::downgrade(&slot),
            registration.clone(),
        );
        let settled = completed.clone();
        cancel.cancel();
        let outcome =
            tokio::time::timeout(std::time::Duration::from_secs(2), completed.wait()).await;
        // Cleanup after the bounded assertion also prevents a failed
        // regression from retaining either actual child.
        slot.request_registered_retirement(&registration)
            .await
            .expect("retire encoder");
        writers.settled();
        registration.wait_confirmed_reap().await;
        let _ = settled.wait().await;
        assert!(matches!(
            outcome.expect("supervisor must retire before helper join"),
            crate::dv_segment::HelperOutcome::Cancelled
        ));
    }

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
        assert_eq!(
            encoder_diagnostic_tail(long.as_bytes())
                .expect("a long line is kept, bounded")
                .len(),
            300
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn empty_or_failed_encoder_output_names_its_cause() {
        use super::private_window_outcome;
        use std::os::unix::process::ExitStatusExt;
        let ok = std::process::ExitStatus::from_raw(0);
        let failed = std::process::ExitStatus::from_raw(1 << 8);
        let stderr = b"Impossible to convert between the formats\nprogress=end\n";
        let error =
            private_window_outcome(Vec::new(), stderr, ok).expect_err("an empty output is refused");
        assert!(error.contains("no output"), "{error}");
        assert!(error.contains("Impossible to convert"), "{error}");
        assert!(!error.contains("bound exceeded"), "{error}");
        let error = private_window_outcome(vec![1], stderr, failed)
            .expect_err("a failed encoder is refused");
        assert!(error.starts_with("DV encoder failed:"), "{error}");
        assert!(error.contains("Impossible to convert"), "{error}");
        assert_eq!(
            private_window_outcome(vec![7], b"", ok).expect("a successful output is kept"),
            vec![7]
        );
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
        // Every decline after the setting check, to the end of the entry
        // point, is reported, never silent.
        let after_setting = body
            .split_once("// The setting is on for a DV source")
            .expect("setting check")
            .1;
        assert!(!after_setting.contains("Ok(None)"), "{after_setting}");
    }

    /// A Dolby Vision display's converted Profile 7 copy never reached the
    /// reconstructed 8.1: the entry point returned for every DV copy before
    /// reading FEL re-encode. Only a native (unconverted) DV copy is silent.
    #[test]
    fn a_converted_dv_copy_is_offered_the_reconstructed_profile81() {
        let body = entry_body();
        let silent = body
            .split_once("let Some(original) = request.candidate_context")
            .expect("candidate context read")
            .0;
        assert!(silent.contains("convert_dolby_vision: false"), "{silent}");
        assert!(
            silent.contains("processed_qualification_destination(request)?"),
            "{silent}"
        );
        assert!(include_str!("transcode/manager/candidates.rs")
            .contains("request.dv_processing_route()?"));
        assert!(body.contains("preferences.fel_reencode"), "{body}");
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

#[cfg(test)]
mod hdr10e_export_throughput {
    /// One body from a C source: from `start` to the first top-level `}`.
    fn c_body<'a>(source: &'a str, start: &str) -> &'a str {
        source
            .split_once(start)
            .expect("C function")
            .1
            .split_once("\n}\n")
            .expect("C function end")
            .0
    }

    /// 4K HDR10-E fell behind real time on the GPU export, not the decode:
    /// the reconstruction wrote 32-bit float, three passes read it back, and
    /// every frame waited for its own download. The export now renders into
    /// 16 bits, queues the download, and only the writer waits for it — so
    /// a decoded surface stays held until its frame has been written.
    #[test]
    fn the_4k_export_renders_in_16_bits_and_queues_its_download() {
        let export = include_str!("../../../tools/dv_processing/yuv420p10.h");
        let main = include_str!("../../../tools/dv_processing/segment_decode_render.c");
        for renderer in [
            include_str!("../../../tools/dv_processing/fel_renderer.h"),
            include_str!("../../../tools/dv_processing/base_dv_renderer.h"),
        ] {
            assert!(renderer.contains("dv_yuv10_source_texture("));
        }
        let source = c_body(export, "static pl_tex dv_yuv10_source_texture(");
        assert!(source.contains("PL_FMT_UNORM, 4, 16, 16"), "{source}");
        let queued = c_body(export, "static bool dv_yuv10_export_color(");
        assert!(queued.contains(".buf = buf"), "{queued}");
        for wait in ["pl_buf_poll", "pl_gpu_finish", "dv_yuv10_wait"] {
            assert!(!queued.contains(wait), "the export waits: {wait}");
        }
        let writer = c_body(main, "static void *writer_main(");
        let waited = writer
            .find("dv_yuv10_wait_buffer(buf)")
            .expect("writer waits");
        assert!(waited < writer.find("write_frame(").expect("writer writes"));
        let rendering = c_body(main, "static void render_pairs(");
        let held = rendering
            .find("hold_rendered_pair()")
            .expect("surfaces held");
        assert!(held < rendering.rfind("drop_pair()").expect("pair dropped"));
        let handoff = c_body(main, "static void write_rgb(");
        let idle = handoff
            .find("writer_wait_idle()")
            .expect("previous frame written");
        assert!(idle < handoff.find("release_held()").expect("surfaces released"));
    }
}

#[cfg(test)]
mod output_metadata_tests {
    use super::source_mastering_metadata;
    fn mastering_record() -> serde_json::Value {
        serde_json::json!({"side_data_type":"Mastering display metadata",
            "red_x":"34000/50000","red_y":"16000/50000",
            "green_x":"13250/50000","green_y":"34500/50000",
            "blue_x":"7500/50000","blue_y":"3000/50000",
            "white_point_x":"15635/50000","white_point_y":"16450/50000",
            "max_luminance":"40000000/10000","min_luminance":"50/10000"})
    }

    #[test]
    fn selected_frame_only_mdcv_preserves_full_metadata_and_raw_probe_comparison() {
        let raw = serde_json::json!({"streams":[
            {"index":7,"codec_type":"video","codec_name":"hevc","width":3840,"height":2160,
             "side_data_list":[{"side_data_type":"DOVI configuration record"}]},
            {"index":9,"codec_type":"audio","codec_name":"aac"}],"format":{"format_name":"matroska"}});
        let before = raw.to_string();
        for minimum in ["1/10000", "50/10000"] {
            let mut record = mastering_record();
            record["min_luminance"] = minimum.into();
            let frame = serde_json::json!({"stream_index":7,"side_data_list":[record]});
            let frames = serde_json::json!({"frames":[frame.clone(),frame.clone(),frame]});
            let actual = super::source_mastering_from_frames(&raw["streams"][0], &frames, 7)
                .expect("bounded selected frames")
                .expect("frame-only MDCV");
            assert_eq!(
                actual.primaries_rgbw,
                [34000, 16000, 13250, 34500, 7500, 3000, 15635, 16450]
            );
            assert_eq!(
                actual.min_1e4_nits,
                if minimum == "1/10000" { 1 } else { 50 }
            );
        }
        assert_eq!(
            raw.to_string(),
            before,
            "frame facts never decorate raw source JSON"
        );
        assert!(
            crate::ffmpeg::compare_probe_documents(&before, &raw.to_string())
                .expect("compare")
                .same
        );
        let mut changed = raw.clone();
        changed["streams"][0]["width"] = 1920.into();
        assert!(
            !crate::ffmpeg::compare_probe_documents(&before, &changed.to_string())
                .expect("changed media")
                .same
        );
        assert_eq!(raw["streams"].as_array().expect("all streams").len(), 2);
    }

    #[test]
    fn frame_mdcv_refuses_wrong_stream_malformed_and_conflicting_records() {
        let record = mastering_record();
        let frame = serde_json::json!({"stream_index":7,"side_data_list":[record.clone()]});
        let document = serde_json::json!({"frames":[frame.clone()]});
        let stream = serde_json::json!({"side_data_list":[record.clone()]});
        assert!(super::source_mastering_from_frames(&stream, &document, 7)
            .expect("matching declarations")
            .is_some());
        assert!(
            super::source_mastering_from_frames(&serde_json::Value::Null, &document, 8)
                .expect_err("wrong absolute stream")
                .contains("selected stream differs")
        );
        for (field, value) in [
            ("red_x", "33000/50000"),
            ("min_luminance", "1/10000"),
            ("max_luminance", "10000000/10000"),
        ] {
            let mut changed = record.clone();
            changed[field] = value.into();
            let conflicting = serde_json::json!({"stream_index":7,"side_data_list":[changed]});
            let disagreement = serde_json::json!({"frames":[frame.clone(),conflicting.clone()]});
            assert!(
                super::source_mastering_from_frames(&serde_json::Value::Null, &disagreement, 7)
                    .is_err(),
                "frame conflict {field}"
            );
            assert!(
                super::source_mastering_from_frames(
                    &stream,
                    &serde_json::json!({"frames":[conflicting]}),
                    7
                )
                .is_err(),
                "stream/frame conflict {field}"
            );
        }
        for field in ["red_x", "white_point_y", "min_luminance", "max_luminance"] {
            let mut malformed = record.clone();
            malformed.as_object_mut().expect("record").remove(field);
            assert!(super::source_mastering_from_frames(&serde_json::Value::Null,&serde_json::json!({"frames":[{"stream_index":7,"side_data_list":[malformed.clone()]}]}),7).is_err());
            assert!(
                super::source_mastering_from_frames(
                    &serde_json::json!({"side_data_list":[malformed]}),
                    &document,
                    7
                )
                .is_err(),
                "malformed stream cannot be overwritten by frame"
            );
        }
        for malformed in [
            serde_json::json!({}),
            serde_json::json!({"frames":{}}),
            serde_json::json!({"frames":[frame.clone(),frame.clone(),frame.clone(),frame.clone()]}),
        ] {
            assert!(
                super::source_mastering_from_frames(&serde_json::Value::Null, &malformed, 7)
                    .is_err()
            );
        }
        assert_eq!(
            super::source_mastering_from_frames(
                &serde_json::json!({"color_primaries":"bt2020"}),
                &serde_json::json!({"frames":[{"stream_index":7}]}),
                7
            )
            .expect("absent"),
            None
        );
    }

    #[test]
    fn targeted_metadata_source_fallback_requires_complete_selected_stream_mdcv() {
        let record = serde_json::json!({"side_data_type":"Mastering display metadata",
            "red_x":"34000/50000","red_y":"16000/50000",
            "green_x":"13250/50000","green_y":"34500/50000",
            "blue_x":"7500/50000","blue_y":"3000/50000",
            "white_point_x":"15635/50000","white_point_y":"16450/50000",
            "max_luminance":"4000/1","min_luminance":"17/10000"});
        let stream = |records| serde_json::json!({"side_data_list":records});
        let actual = source_mastering_metadata(&stream(vec![record.clone()]))
            .expect("valid MDCV")
            .expect("present");
        assert_eq!(
            actual.primaries_rgbw,
            [34000, 16000, 13250, 34500, 7500, 3000, 15635, 16450]
        );
        assert_eq!(actual.min_1e4_nits, 17);
        assert_eq!(
            source_mastering_metadata(&stream(vec![record.clone(), record.clone()]))
                .expect("same duplicate"),
            Some(actual)
        );
        assert_eq!(
            source_mastering_metadata(&serde_json::json!({"color_primaries":"bt2020"}))
                .expect("missing"),
            None
        );
        for field in ["red_x", "white_point_y", "min_luminance", "max_luminance"] {
            let mut partial = record.clone();
            partial.as_object_mut().expect("record").remove(field);
            assert!(
                source_mastering_metadata(&stream(vec![partial])).is_err(),
                "partial {field}"
            );
        }
        for (field, value) in [
            ("green_x", "1/0"),
            ("red_y", "NaN"),
            ("blue_x", "70000/50000"),
            ("min_luminance", "5000/1"),
            ("max_luminance", "0/1"),
        ] {
            let mut invalid = record.clone();
            invalid[field] = value.into();
            assert!(
                source_mastering_metadata(&stream(vec![invalid])).is_err(),
                "invalid {field}"
            );
        }
        for (field, value) in [("red_x", "33000/50000"), ("max_luminance", "1000/1")] {
            let mut conflicting = record.clone();
            conflicting[field] = value.into();
            assert!(source_mastering_metadata(&stream(vec![record.clone(), conflicting])).is_err());
        }
    }
}

#[cfg(test)]
mod sdr_floor_tests {
    use super::*;
    #[test]
    fn sdr_frame_minimum_ignores_hdr_coordinates_but_refuses_bad_or_conflicting_minima() {
        let minimum = serde_json::json!({"side_data_type":"Mastering display metadata","min_luminance":"1/10000","red_x":"invalid and irrelevant for SDR"});
        let frame = serde_json::json!({"stream_index":7,"side_data_list":[minimum.clone()]});
        let document = serde_json::json!({"frames":[frame.clone()]});
        assert_eq!(
            source_mastering_minimum_from_frames(&serde_json::Value::Null, &document, 7)
                .expect("minimum-only source"),
            Some(1)
        );
        assert!(
            source_mastering_from_frames(&serde_json::Value::Null, &document, 7).is_err(),
            "HDR still requires complete mastering facts"
        );
        let duplicate = serde_json::json!({"frames":[frame.clone(),frame.clone()]});
        assert_eq!(
            source_mastering_minimum_from_frames(&serde_json::Value::Null, &duplicate, 7)
                .expect("consistent"),
            Some(1)
        );
        assert!(
            source_mastering_minimum_from_frames(&serde_json::Value::Null, &document, 8).is_err(),
            "same selected-stream fence"
        );
        for value in ["1/0", "NaN", "-1/10000", "10001/10000"] {
            let mut bad = minimum.clone();
            bad["min_luminance"] = value.into();
            assert!(source_mastering_minimum_from_frames(&serde_json::Value::Null,&serde_json::json!({"frames":[{"stream_index":7,"side_data_list":[bad.clone()]}]}),7).is_err(),"malformed frame minimum {value}");
            assert!(
                source_mastering_minimum_from_frames(
                    &serde_json::json!({"side_data_list":[bad]}),
                    &document,
                    7
                )
                .is_err(),
                "malformed stream minimum cannot be overwritten"
            );
        }
        let mut changed = minimum.clone();
        changed["min_luminance"] = "50/10000".into();
        let conflict = serde_json::json!({"frames":[frame,{"stream_index":7,"side_data_list":[changed.clone()]}]});
        assert!(
            source_mastering_minimum_from_frames(&serde_json::Value::Null, &conflict, 7).is_err()
        );
        assert!(
            source_mastering_minimum_from_frames(
                &serde_json::json!({"side_data_list":[changed]}),
                &document,
                7
            )
            .is_err(),
            "stream/frame minimum conflict"
        );
        for absent in [
            serde_json::json!({"frames":[]}),
            serde_json::json!({"frames":[{"stream_index":7,"side_data_list":[{"side_data_type":"Mastering display metadata","red_x":"unknown"}]}]}),
        ] {
            assert_eq!(
                source_mastering_minimum_from_frames(&serde_json::Value::Null, &absent, 7)
                    .expect("unknown minimum remains absent"),
                None
            );
        }
    }

    #[test]
    fn sdr_source_floor_does_not_require_hdr_primaries_but_refuses_conflicting_minima() {
        let stream = serde_json::json!({"side_data_list":[{"side_data_type":"Mastering display metadata","min_luminance":"17/10000"}]});
        assert_eq!(
            source_mastering_minimum(&stream).expect("minimum"),
            Some(17)
        );
        assert!(
            source_mastering_metadata(&stream).is_err(),
            "HDR declarations still require their complete facts"
        );
        let mut conflict = stream.clone();
        conflict["side_data_list"].as_array_mut().expect("list").push(serde_json::json!({"side_data_type":"Mastering display metadata","min_luminance":"50/10000"}));
        assert!(source_mastering_minimum(&conflict).is_err());
        for value in ["1/0", "-1/10000", "10001/10000"] {
            let mut malformed = stream.clone();
            malformed["side_data_list"][0]["min_luminance"] = value.into();
            assert!(source_mastering_minimum(&malformed).is_err());
        }
        assert_eq!(
            source_mastering_minimum(&serde_json::json!({})).expect("unknown"),
            None
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod title_scanner_identity_tests {
    use super::*;

    fn backend(renderer: &[u8]) -> DvBackendIdentity {
        let digest = |bytes: &[u8]| DvDigest::new(hash(bytes)).expect("digest");
        DvBackendIdentity::new(
            digest(renderer),
            digest(b"libraries"),
            digest(b"parser"),
            digest(b"sources"),
            1,
            digest(b"manifest"),
        )
        .expect("backend")
    }

    fn scanner(directory: &tempfile::TempDir, name: &str, bytes: &[u8]) -> PathBuf {
        let path = directory.path().join(name);
        std::fs::write(&path, bytes).expect("scanner");
        path
    }

    /// A renderer, shader or libplacebo change must not re-queue every title;
    /// only a different scanner may.
    #[test]
    fn title_demand_is_keyed_by_the_scanner_alone() {
        let directory = tempfile::tempdir().expect("scratch");
        let first = scanner(&directory, "first", b"scanner one");
        let second = scanner(&directory, "second", b"scanner two");
        let before =
            ToolSet::lifecycle_fixture_with_scanner(backend(b"renderer one"), first.clone())
                .expect("tools");
        let new_renderer = ToolSet::lifecycle_fixture_with_scanner(backend(b"renderer two"), first)
            .expect("tools");
        let new_scanner = ToolSet::lifecycle_fixture_with_scanner(backend(b"renderer one"), second)
            .expect("tools");
        assert_ne!(before.backend_digest(), new_renderer.backend_digest());
        assert_eq!(
            before.title_scanner_digest(),
            new_renderer.title_scanner_digest()
        );
        assert_eq!(before.backend_digest(), new_scanner.backend_digest());
        assert_ne!(
            before.title_scanner_digest(),
            new_scanner.title_scanner_digest()
        );
    }

    /// Every title-demand reader and writer uses the scanner key; FEL verdicts
    /// keep the backend key because they measure the renderer.
    #[test]
    fn title_demand_call_sites_use_the_scanner_key() {
        for (name, source) in [
            ("title_work.rs", include_str!("source_probe/title_work.rs")),
            ("dv_title.rs", include_str!("source_probe/dv_title.rs")),
        ] {
            assert!(source.contains("title_scanner_digest()"), "{name}");
            // The backend key appears only as the FEL identity it measures.
            assert_eq!(
                source.matches("backend_digest()").count(),
                source
                    .matches("fel_identity(&tools.backend_digest())")
                    .count(),
                "{name}"
            );
        }
        let create = include_str!("transcode/manager/create.rs");
        assert!(create.contains("self.fel_identity(&tools.backend_digest())"));
        assert!(create.contains(
            "planning.probe_json.as_deref(),\n            &tools.title_scanner_digest(),"
        ));
    }
}
