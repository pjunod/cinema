//! Hardware-specific media support, separate from backend preference and FPS.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use plurx_core::transcode::{Encoder, EncoderCaps, Pipeline};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const CONTRACT: &str = include_str!("../../../scripts/ffmpeg-support-matrix.json");

#[derive(Deserialize)]
struct Contract {
    required_package: String,
    expected_version: String,
    source_commit: String,
    source_sha256: String,
    macos_patches: BTreeMap<String, String>,
    rows: Vec<Requirement>,
}

#[derive(Deserialize)]
struct Requirement {
    id: String,
    label: String,
    required_for: String,
    profile: String,
    groups: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    options: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    optional: bool,
    encoder: Option<String>,
    pipeline: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SupportRow {
    pub id: String,
    pub label: String,
    pub required_for: String,
    pub applicable: bool,
    pub required: bool,
    pub included: bool,
    pub missing: Vec<String>,
    pub runtime: String,
    pub runtime_detail: Option<String>,
    #[serde(skip)]
    encoder: Option<String>,
    #[serde(skip)]
    pipeline: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SupportReport {
    pub required_package: String,
    pub expected_version: String,
    pub executable_version: Option<String>,
    pub source_revision: String,
    pub package_verification: String,
    #[serde(skip)]
    pub additional_media_support: Vec<MediaSupport>,
    #[serde(skip)]
    pub hwaccels: BTreeSet<String>,
    #[serde(skip)]
    pub filters: BTreeSet<String>,
    pub executable: String,
    pub expected_executable: Option<String>,
    pub package_verified: bool,
    pub package_problems: Vec<String>,
    pub rows: Vec<SupportRow>,
    pub checked_at: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct MediaSupport {
    pub id: String,
    pub label: String,
    pub support_available: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct TranscodingFact {
    pub id: String,
    pub label: String,
    pub enabled: bool,
    pub hardware: bool,
    pub backend: String,
    pub device_ids: Vec<String>,
    pub detail: String,
}

fn package_verification_status(platform: &str, verified: bool) -> &'static str {
    if platform != "macos" {
        "unsupported_platform"
    } else if verified {
        "verified"
    } else {
        "unverified"
    }
}

fn executable_version(text: &str) -> Option<String> {
    text.lines()
        .next()?
        .strip_prefix("ffmpeg version ")?
        .split_whitespace()
        .next()
        .map(str::to_owned)
}

fn media_support(inventory: &Inventory) -> Vec<MediaSupport> {
    let has = |group: &str, name: &str| {
        inventory
            .groups
            .get(group)
            .is_some_and(|items| items.contains(name))
    };
    [
        (
            "ac4",
            "AC-4 audio",
            has("decoders", "ac4"),
            "Audio decoder; used when the source requires it",
        ),
        (
            "dolby_vision",
            "Dolby Vision",
            has("decoders", "hevc") && has("filters", "tonemapx") && has("bsfs", "dovi_rpu"),
            "Dolby Vision metadata and rendering support; profile-dependent",
        ),
        (
            "vulkan",
            "Vulkan / libplacebo",
            has("filters", "libplacebo") && has("hwaccels", "vulkan"),
            "Alternative GPU processing; device support depends on the installed driver",
        ),
        (
            "opencl",
            "OpenCL tone mapping",
            has("filters", "tonemap_opencl") && has("hwaccels", "opencl"),
            "Alternative GPU tone mapping; device support depends on the installed OpenCL runtime",
        ),
    ]
    .into_iter()
    .map(|(id, label, available, detail)| MediaSupport {
        id: id.to_owned(),
        label: label.to_owned(),
        support_available: available,
        detail: detail.to_owned(),
    })
    .collect()
}

/// Project configured policy and detected facilities without turning external
/// acceptance evidence into a feature switch. Per-source eligibility remains
/// the resolver's responsibility.
pub fn transcoding_facts(
    system: &crate::state::SystemInfo,
    support: &SupportReport,
    encoder: Encoder,
    mac_enabled: bool,
    hardware_sessions_enabled: bool,
) -> Vec<TranscodingFact> {
    let mut facts = transcoding_facts_for_platform(
        system,
        support,
        encoder,
        mac_enabled,
        std::env::consts::OS,
        hardware_sessions_enabled,
    );
    apply_decode_compatibility(&mut facts, std::env::var("PLURX_HWDECODE").ok().as_deref());
    facts
}

fn apply_decode_compatibility(facts: &mut [TranscodingFact], compatibility: Option<&str>) {
    use plurx_core::transcode::{DecodePlanPolicy, DecodePolicySnapshot};
    if DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, compatibility).force_software_decode() {
        if let Some(fact) = facts.iter_mut().find(|fact| fact.id == "hardware_decoding") {
            fact.enabled = false;
            fact.hardware = false;
            fact.backend = "software".to_owned();
            fact.device_ids.clear();
            fact.detail = "CPU decoding · Compatibility preference".to_owned();
        }
    }
}

fn transcoding_facts_for_platform(
    system: &crate::state::SystemInfo,
    support: &SupportReport,
    encoder: Encoder,
    mac_enabled: bool,
    platform: &str,
    hardware_sessions_enabled: bool,
) -> Vec<TranscodingFact> {
    let hardware_encoder = hardware_sessions_enabled
        && encoder != Encoder::Software
        && system.encoders.available(encoder);
    let backend = encoder.family_name();
    let binding = system
        .encoders
        .device_bindings
        .iter()
        .find(|binding| binding.backend == backend);
    let device_ids = binding
        .map(|binding| binding.device_ids.clone())
        .unwrap_or_default();
    let device = device_ids
        .iter()
        .filter_map(|id| {
            system
                .encoders
                .graphics_devices
                .iter()
                .find(|device| &device.id == id)
        })
        .map(|device| {
            format!(
                "{} ({})",
                device.model.as_deref().unwrap_or(&device.vendor),
                device.id
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let device = if !device.is_empty() {
        device
    } else {
        match encoder {
            Encoder::VideoToolbox => "Apple video hardware",
            Encoder::Nvenc => "NVIDIA video hardware (driver default)",
            Encoder::Qsv => "Intel video hardware (driver default)",
            Encoder::Vaapi => "Render node; physical device unresolved",
            Encoder::Software => "CPU",
        }
        .to_owned()
    };
    let decode_backend = match encoder {
        Encoder::Nvenc => "cuda",
        Encoder::Qsv => "qsv",
        Encoder::Vaapi => "vaapi",
        Encoder::VideoToolbox => "videotoolbox",
        Encoder::Software => "none",
    };
    let decode = hardware_encoder && support.hwaccels.contains(decode_backend);
    let native_mac =
        platform == "macos" && hardware_encoder && encoder == Encoder::VideoToolbox && mac_enabled;
    let native_scale = native_mac && support.filters.contains("scale_vt");
    let native_tone = native_scale && support.filters.contains("tonemap_videotoolbox");
    let measured_pipeline = system.tone_map.selected();
    let pipeline = match measured_pipeline {
        Pipeline::VtToneMapMetal if !native_tone => Pipeline::Cpu,
        Pipeline::VtScaleSdr | Pipeline::VtScaleHdr10 if !native_scale => Pipeline::Cpu,
        pipeline if !pipeline.pairs_with(encoder) => Pipeline::Cpu,
        pipeline => pipeline,
    };
    let hardware_tone = hardware_sessions_enabled && (native_tone || pipeline.on_gpu());
    let hardware_scale = hardware_sessions_enabled
        && (native_scale
            || matches!(
                pipeline,
                Pipeline::TonemapCuda
                    | Pipeline::VppQsv
                    | Pipeline::TonemapVaapi
                    | Pipeline::Libplacebo
                    | Pipeline::LibplaceboVaapi
                    | Pipeline::LibplaceboSoftware
                    | Pipeline::VtScaleSdr
                    | Pipeline::VtScaleHdr10
                    | Pipeline::VtToneMapMetal
            ));
    let subtitles = support.filters.contains("subtitles") || support.filters.contains("ass");
    let deinterlace = support.filters.contains("bwdif");
    let native_subtitles = native_scale
        && support.filters.contains("subtitles_vt_images")
        && support.filters.contains("overlay_videotoolbox");
    let native_deinterlace = native_scale && support.filters.contains("bwdif_videotoolbox");
    let processing_backend = if !hardware_sessions_enabled {
        "CPU"
    } else if native_tone {
        "Metal"
    } else {
        match pipeline {
            Pipeline::TonemapCuda => "CUDA",
            Pipeline::VppQsv => "Quick Sync",
            Pipeline::TonemapVaapi => "VA-API",
            Pipeline::Libplacebo | Pipeline::LibplaceboVaapi | Pipeline::LibplaceboSoftware => {
                "Vulkan / libplacebo"
            }
            Pipeline::TonemapOpencl => "OpenCL",
            Pipeline::VtToneMapMetal | Pipeline::VtDoviMetal | Pipeline::DoviMetal => "Metal",
            _ => "CPU",
        }
    };
    let encoder_label = match encoder {
        Encoder::VideoToolbox => "VideoToolbox",
        Encoder::Nvenc => "NVENC",
        Encoder::Qsv => "Quick Sync",
        Encoder::Vaapi => "VA-API",
        Encoder::Software => "CPU",
    };
    let decoder_label = if encoder == Encoder::Nvenc {
        "NVDEC"
    } else {
        encoder_label
    };
    let fact =
        |id: &str, label: &str, enabled, hardware, backend: &str, detail: String| TranscodingFact {
            id: id.to_owned(),
            label: label.to_owned(),
            enabled,
            hardware,
            backend: backend.to_owned(),
            // A processing adapter is not proved by its paired encoder's binding.
            device_ids: if hardware && matches!(id, "hardware_decoding" | "hardware_encoding") {
                device_ids.clone()
            } else {
                Vec::new()
            },
            detail,
        };
    vec![
        fact(
            "hardware_decoding",
            "Hardware decoding",
            decode,
            decode,
            decode_backend,
            if decode {
                format!(
                    "{decoder_label} · {device}\nSupported codecs use hardware; others use the CPU"
                )
            } else {
                "CPU decoding is selected".to_owned()
            },
        ),
        fact(
            "hardware_encoding",
            "Hardware encoding",
            hardware_encoder,
            hardware_encoder,
            backend,
            if hardware_sessions_enabled {
                format!("{encoder_label} · {device}")
            } else {
                "Hardware session limit is zero".to_owned()
            },
        ),
        fact(
            "tone_mapping",
            "HDR → SDR tone mapping",
            (support.filters.contains("tonemap") && support.filters.contains("zscale"))
                || hardware_tone,
            hardware_tone,
            processing_backend,
            if hardware_tone {
                format!("{processing_backend} · GPU acceleration")
            } else {
                "CPU processing".to_owned()
            },
        ),
        fact(
            "hardware_scaling",
            "Hardware scaling",
            hardware_scale,
            hardware_scale,
            if native_scale {
                "VideoToolbox"
            } else {
                processing_backend
            },
            if native_scale {
                "VideoToolbox · Hardware-accelerated resizing".to_owned()
            } else if hardware_scale {
                format!("{processing_backend} · Hardware-accelerated resizing\nSelected for compatible streams")
            } else {
                "CPU resizing is selected".to_owned()
            },
        ),
        fact(
            "subtitle_burn_in",
            "Subtitle burn-in",
            subtitles || native_subtitles,
            native_subtitles,
            if native_subtitles {
                "VideoToolbox / Metal"
            } else {
                "CPU"
            },
            if native_subtitles {
                "Mac GPU processing · Text and bitmap subtitles"
            } else {
                "CPU rendering · May require GPU frame transfers"
            }
            .to_owned(),
        ),
        fact(
            "deinterlacing",
            if native_deinterlace {
                "Hardware deinterlacing"
            } else {
                "Deinterlacing"
            },
            deinterlace || native_deinterlace,
            native_deinterlace,
            if native_deinterlace {
                "VideoToolbox"
            } else {
                "CPU"
            },
            if native_deinterlace {
                "VideoToolbox · Interlaced SDR video"
            } else {
                "CPU processing · Interlaced video"
            }
            .to_owned(),
        ),
    ]
}

#[derive(Default)]
struct Inventory {
    groups: BTreeMap<String, BTreeSet<String>>,
    options: BTreeMap<String, BTreeSet<String>>,
}

static REPORT: tokio::sync::OnceCell<SupportReport> = tokio::sync::OnceCell::const_new();

fn windows_gpu_vendors(text: &str) -> BTreeSet<String> {
    let text = text.to_ascii_uppercase();
    ["8086", "1002", "10DE"]
        .into_iter()
        .filter(|id| text.contains(&format!("VEN_{id}&")))
        .map(|id| format!("0x{}", id.to_ascii_lowercase()))
        .collect()
}

fn hardware_profiles(
    platform: &str,
    vendors: &BTreeSet<String>,
    caps: &EncoderCaps,
) -> BTreeSet<String> {
    let mut profiles = BTreeSet::from(["common".to_owned()]);
    if platform == "macos" {
        profiles.extend(["macos".to_owned(), "gpu".to_owned()]);
    } else {
        if vendors.contains("0x8086") || caps.qsv {
            profiles.extend(["intel".to_owned(), "gpu".to_owned()]);
            if platform == "linux" {
                profiles.insert("vaapi".to_owned());
            }
        }
        if vendors.contains("0x1002") || caps.vaapi {
            profiles.insert("gpu".to_owned());
            if platform == "linux" {
                profiles.insert("vaapi".to_owned());
            }
        }
        if vendors.contains("0x10de") || caps.nvenc {
            profiles.extend(["nvidia".to_owned(), "gpu".to_owned()]);
        }
    }
    profiles
}

fn rows(contract: Contract, profiles: &BTreeSet<String>, inventory: &Inventory) -> Vec<SupportRow> {
    contract
        .rows
        .into_iter()
        .map(|requirement| {
            let applicable = profiles.contains(&requirement.profile);
            let mut missing = Vec::new();
            for (group, names) in requirement.groups {
                for name in names {
                    if !inventory
                        .groups
                        .get(&group)
                        .is_some_and(|items| items.contains(&name))
                    {
                        missing.push(format!("{group}: {name}"));
                    }
                }
            }
            for (filter, names) in requirement.options {
                for name in names {
                    if !inventory
                        .options
                        .get(&filter)
                        .is_some_and(|items| items.contains(&name))
                    {
                        missing.push(format!("{filter}: {name}"));
                    }
                }
            }
            let included = missing.is_empty();
            SupportRow {
                id: requirement.id,
                label: requirement.label,
                required_for: requirement.required_for,
                applicable,
                required: applicable && !requirement.optional,
                included,
                missing,
                runtime: if !applicable {
                    "not_applicable"
                } else if !included {
                    "blocked"
                } else {
                    "not_tested"
                }
                .to_owned(),
                runtime_detail: None,
                encoder: requirement.encoder,
                pipeline: requirement.pipeline,
            }
        })
        .collect()
}

async fn probe(ffmpeg: &str, args: &[&str]) -> Result<String, String> {
    let mut command = tokio::process::Command::new(ffmpeg);
    command.args(["-hide_banner"]).args(args);
    let output = crate::ffmpeg::bounded_command_output_cancellable(
        command,
        Duration::from_secs(10),
        2 * 1024 * 1024,
        "media support inventory",
        None,
        crate::process_control::ChildWork::background("media support inventory"),
    )
    .await?;
    Ok(format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

fn declared_names(text: &str, group: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            if group == "hwaccels" || group == "bsfs" {
                (fields.len() == 1 && !fields[0].ends_with(':')).then(|| fields[0].to_owned())
            } else {
                (fields.len() >= 2
                    && fields[0].len() >= 2
                    && fields[0]
                        .chars()
                        .all(|c| c == '.' || c.is_ascii_uppercase() || c == '|'))
                .then(|| fields[1].to_owned())
            }
        })
        .collect()
}

fn declared_options(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            fields.next()?.starts_with('<').then(|| name.to_owned())
        })
        .collect()
}

fn verify_file(root: &Path, relative: &str, expected: Option<&str>) -> Result<(), String> {
    let path = root.join(relative);
    let mut file =
        std::fs::File::open(&path).map_err(|_| format!("Missing companion: {relative}"))?;
    if file.metadata().map_err(|error| error.to_string())?.len() > 256 * 1024 * 1024 {
        return Err(format!("Oversized companion: {relative}"));
    }
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    use std::io::Read;
    loop {
        let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    if expected != Some(hex::encode(digest.finalize()).as_str()) {
        return Err(format!("Companion does not match package: {relative}"));
    }
    Ok(())
}

fn verify_macos_package(root: &Path, contract: &Contract) -> Result<(), String> {
    let path = root.join("provenance/manifest.json");
    if std::fs::metadata(&path)
        .map_err(|_| "No verified media-package manifest".to_owned())?
        .len()
        > 2 * 1024 * 1024
    {
        return Err("Oversized media-package manifest".to_owned());
    }
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    if manifest["mode"] != "compiled-pinned-jellyfin"
        || manifest["source_commit"] != contract.source_commit
        || manifest["source_sha256"] != contract.source_sha256
    {
        return Err("FFmpeg is not the pinned, patched Jellyfin package".to_owned());
    }
    if manifest["compiled_source"]["local_patches"]
        != serde_json::to_value(&contract.macos_patches).map_err(|error| error.to_string())?
    {
        return Err("FFmpeg package lacks the current Plurx patch set".to_owned());
    }
    for name in ["ffmpeg", "ffprobe"] {
        verify_file(root, name, manifest["binaries"][name]["sha256"].as_str())?;
    }
    if manifest["source_parser"]["parser_abi"] != "plurx-source-wasi-p1-v1-wasmtime-49.0.2" {
        return Err("Source parser does not match the required ABI".to_owned());
    }
    verify_file(
        root,
        "plurx-source-parser.wasm",
        manifest["source_parser"]["sha256"].as_str(),
    )?;
    for name in ["fc-list", "fc-conflist"] {
        verify_file(
            root,
            name,
            manifest["font_tools"]["binaries"][name]["sha256"].as_str(),
        )?;
    }
    let fonts = manifest["font_tools"]["configuration"]
        .as_object()
        .ok_or("Missing font configuration identity")?;
    if !fonts.contains_key("fonts.conf") {
        return Err("Missing font configuration identity".to_owned());
    }
    for (name, digest) in fonts {
        let relative = Path::new(name);
        if relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err("Unsafe font configuration identity".to_owned());
        }
        verify_file(root, &format!("fontconfig/{name}"), digest.as_str())?;
    }
    Ok(())
}

async fn processing_smoke(ffmpeg: &str, pipeline: Pipeline) -> Result<(), String> {
    let mut command = tokio::process::Command::new(ffmpeg);
    command.args(["-hide_banner", "-loglevel", "error"]);
    if pipeline == Pipeline::VtToneMapMetal {
        command.args([
            "-init_hw_device",
            "videotoolbox=vt",
            "-filter_hw_device",
            "vt",
        ]);
    } else {
        command.args(pipeline.init_args());
    }
    command.args([
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=1280x720:rate=30:duration=0.5",
    ]);
    let source = "format=yuv420p10le,setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc";
    let graph = pipeline
        .filters_at_peak(Some(1280), 720, Some("hdr10"), Some(1000))
        .ok_or("No processing graph for the support probe")?;
    let graph = if pipeline == Pipeline::VtToneMapMetal {
        format!("{source},format=p010le,hwupload,{graph},hwdownload,format=nv12")
    } else {
        format!("{source},{graph}")
    };
    command.args([
        "-vf",
        &graph,
        "-c:v",
        "rawvideo",
        "-frames:v",
        "15",
        "-progress",
        "pipe:1",
        "-f",
        "null",
        "-",
    ]);
    let outcome = crate::ffmpeg::bounded_command_capture_cancellable(
        command,
        Duration::from_secs(15),
        1024 * 1024,
        "media processing smoke",
        None,
        crate::process_control::ChildWork::background("media processing smoke"),
    )
    .await?;
    let output = outcome.output?;
    if !outcome.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr)
            .lines()
            .take(4)
            .collect::<Vec<_>>()
            .join(" · "));
    }
    let progress = String::from_utf8_lossy(&output.stdout);
    if !progress.lines().any(|line| line == "progress=end")
        || progress
            .lines()
            .filter_map(|line| line.strip_prefix("frame="))
            .next_back()
            .map(str::trim)
            != Some("15")
    {
        return Err("Processing probe did not complete all 15 frames".to_owned());
    }
    Ok(())
}

async fn inspect(ffmpeg: &str, ffprobe: &str, caps: &EncoderCaps) -> SupportReport {
    let mut report = SupportReport {
        required_package: String::new(),
        expected_version: String::new(),
        executable_version: None,
        source_revision: String::new(),
        package_verification: package_verification_status(std::env::consts::OS, false).to_owned(),
        additional_media_support: Vec::new(),
        hwaccels: BTreeSet::new(),
        filters: BTreeSet::new(),
        executable: ffmpeg.to_owned(),
        expected_executable: None,
        package_verified: false,
        package_problems: Vec::new(),
        rows: Vec::new(),
        checked_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };
    let contract: Contract = match serde_json::from_str(CONTRACT) {
        Ok(contract) => contract,
        Err(error) => {
            report
                .package_problems
                .push(format!("Invalid media support contract: {error}"));
            return report;
        }
    };
    if !cfg!(target_os = "macos") {
        // Resolving the selected object supplies its actual location; it is
        // deliberately not a source-package integrity attestation on Linux.
        match crate::ffmpeg::EncodedExecutable::capture_program(ffmpeg).await {
            Ok(selected) => report.executable = selected.path.display().to_string(),
            Err(error) => report.package_problems.push(error),
        }
    }
    report.required_package = contract.required_package.clone();
    report.expected_version = contract.expected_version.clone();
    report.source_revision = contract.source_commit.clone();
    let mut inventory = Inventory::default();
    match probe(ffmpeg, &["-version"]).await {
        Ok(text) => {
            report.executable_version = executable_version(&text);
            if report.executable_version.as_deref() != Some(&contract.expected_version) {
                report.package_problems.push(format!(
                    "Selected FFmpeg version does not match required {}",
                    contract.expected_version
                ));
            }
        }
        Err(error) => report.package_problems.push(error),
    }
    for group in ["filters", "encoders", "decoders", "hwaccels", "bsfs"] {
        match probe(ffmpeg, &[&format!("-{group}")]).await {
            Ok(text) => {
                inventory
                    .groups
                    .insert(group.to_owned(), declared_names(&text, group));
            }
            Err(error) => report.package_problems.push(error),
        }
    }
    for filter in contract
        .rows
        .iter()
        .flat_map(|row| row.options.keys())
        .collect::<BTreeSet<_>>()
    {
        if let Ok(text) = probe(ffmpeg, &["-h", &format!("filter={filter}")]).await {
            inventory
                .options
                .insert(filter.to_owned(), declared_options(&text));
        }
    }
    let mut vendors = BTreeSet::new();
    if cfg!(target_os = "linux") {
        if let Ok(entries) = std::fs::read_dir("/sys/class/drm") {
            for entry in entries.take(256).flatten() {
                if let Ok(vendor) = std::fs::read_to_string(entry.path().join("device/vendor")) {
                    vendors.insert(vendor.trim().to_owned());
                }
            }
        }
        // Discrete NVIDIA cards can exist without a DRM node when their driver
        // is absent. Requirements must survive that failure too.
        if let Ok(entries) = std::fs::read_dir("/sys/bus/pci/devices") {
            for entry in entries.take(4096).flatten() {
                let device = entry.path();
                if std::fs::read_to_string(device.join("class"))
                    .is_ok_and(|class| class.starts_with("0x03"))
                {
                    if let Ok(vendor) = std::fs::read_to_string(device.join("vendor")) {
                        vendors.insert(vendor.trim().to_owned());
                    }
                }
            }
        }
    }
    if cfg!(target_os = "windows") {
        let mut command = tokio::process::Command::new("powershell.exe");
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-CimInstance Win32_VideoController | ForEach-Object { $_.PNPDeviceID }",
        ]);
        match crate::ffmpeg::bounded_command_output_cancellable(
            command,
            Duration::from_secs(10),
            64 * 1024,
            "GPU inventory",
            None,
            crate::process_control::ChildWork::background("GPU inventory"),
        )
        .await
        {
            Ok(output) => vendors.extend(windows_gpu_vendors(&String::from_utf8_lossy(
                &output.stdout,
            ))),
            Err(error) => report
                .package_problems
                .push(format!("Cannot determine GPU requirements: {error}")),
        }
    }
    if cfg!(target_os = "macos") {
        let expected = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(|parent| parent.join("ffmpeg")));
        report.expected_executable = expected.as_ref().map(|path| path.display().to_string());
        for (name, configured) in [("ffmpeg", ffmpeg), ("ffprobe", ffprobe)] {
            match crate::ffmpeg::EncodedExecutable::capture_program(configured).await {
                Ok(selected) => {
                    if name == "ffmpeg" {
                        report.executable = selected.path.display().to_string();
                    }
                    if expected
                        .as_ref()
                        .map(|path| path.with_file_name(name))
                        .and_then(|path| std::fs::canonicalize(path).ok())
                        .as_ref()
                        != Some(&selected.path)
                    {
                        report.package_problems.push(format!(
                            "Configured {name} is outside the daemon's managed media package: {}",
                            selected.path.display()
                        ));
                    }
                }
                Err(error) => report.package_problems.push(error),
            }
        }
        if let Some(root) = expected.and_then(|path| path.parent().map(Path::to_path_buf)) {
            let checked = tokio::task::spawn_blocking(move || {
                let contract: Contract =
                    serde_json::from_str(CONTRACT).map_err(|error| error.to_string())?;
                verify_macos_package(&root, &contract)
            })
            .await;
            match checked {
                Ok(Ok(())) => report.package_verified = report.package_problems.is_empty(),
                Ok(Err(error)) => report.package_problems.push(error),
                Err(error) => report.package_problems.push(error.to_string()),
            }
        }
    }
    report.package_verification =
        package_verification_status(std::env::consts::OS, report.package_verified).to_owned();
    report.additional_media_support = media_support(&inventory);
    report.hwaccels = inventory
        .groups
        .get("hwaccels")
        .cloned()
        .unwrap_or_default();
    report.filters = inventory.groups.get("filters").cloned().unwrap_or_default();
    report.rows = rows(
        contract,
        &hardware_profiles(std::env::consts::OS, &vendors, caps),
        &inventory,
    );
    // Probe native processing independently of Auto's selected encoder. A CPU
    // preference must not turn absent Metal/Vulkan evidence into a pass.
    for (id, pipeline) in [
        ("metal", Pipeline::VtToneMapMetal),
        ("vulkan", Pipeline::Libplacebo),
    ] {
        if let Some(row) = report
            .rows
            .iter_mut()
            .find(|row| row.id == id && row.applicable && row.included)
        {
            match processing_smoke(ffmpeg, pipeline).await {
                Ok(()) => {
                    row.runtime = "passed".to_owned();
                    row.runtime_detail = Some("15-frame synthetic PQ-to-SDR processing test; picture quality and real-source qualification are separate".to_owned());
                }
                Err(error) => {
                    row.runtime = "failed".to_owned();
                    row.runtime_detail = Some(error);
                }
            }
        }
    }
    report
}

/// Inventory is collected once at startup. Reads project the boot and current
/// compatibility results; no Settings poll launches another subprocess.
pub async fn report(system: &crate::state::SystemInfo) -> SupportReport {
    let mut report = REPORT
        .get_or_init(|| inspect(&system.ffmpeg, &system.ffprobe, &system.encoders))
        .await
        .clone();
    for row in &mut report.rows {
        if !row.applicable || !row.included {
            continue;
        }
        if let Some(family) = &row.encoder {
            let encoder = match family.as_str() {
                "videotoolbox" => Encoder::VideoToolbox,
                "qsv" => Encoder::Qsv,
                "vaapi" => Encoder::Vaapi,
                "nvenc" => Encoder::Nvenc,
                _ => Encoder::Software,
            };
            row.runtime = if system.encoders.available(encoder) {
                "passed"
            } else {
                "failed"
            }
            .to_owned();
            row.runtime_detail = Some(
                "Production H.264 encoder validation; HEVC qualification is separate".to_owned(),
            );
        }
        if row.id != "metal" && row.id != "vulkan" {
            if let Some(pipeline) = &row.pipeline {
                if let Some(verdict) = system
                    .tone_map
                    .verdicts
                    .iter()
                    .find(|verdict| verdict.pipeline == *pipeline)
                {
                    row.runtime = if verdict.passed {
                        "passed"
                    } else if verdict.unavailable {
                        "unavailable"
                    } else {
                        "failed"
                    }
                    .to_owned();
                    row.runtime_detail = verdict.rejected.clone();
                }
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_contract_separates_package_and_executable_versions() {
        let contract: Contract = serde_json::from_str(CONTRACT).expect("contract");
        assert_eq!(contract.required_package, "8.1.3-1");
        assert_eq!(contract.expected_version, "8.1.3-Jellyfin");
        assert_eq!(
            package_verification_status("linux", true),
            "unsupported_platform"
        );
        assert_eq!(
            package_verification_status("linux", false),
            "unsupported_platform"
        );
        assert_eq!(package_verification_status("macos", false), "unverified");
        assert_eq!(package_verification_status("macos", true), "verified");
        assert_eq!(
            executable_version("ffmpeg version 8.1.3-Jellyfin Copyright 2026\nconfiguration: ...")
                .as_deref(),
            Some(contract.expected_version.as_str())
        );
        assert!(executable_version("failed to run ffmpeg").is_none());
    }

    #[test]
    fn additional_support_distinguishes_build_facilities_from_runtime_qualification() {
        let mut inventory = Inventory::default();
        inventory.groups.insert(
            "decoders".to_owned(),
            BTreeSet::from(["ac4".to_owned(), "hevc".to_owned()]),
        );
        inventory.groups.insert(
            "filters".to_owned(),
            BTreeSet::from([
                "tonemapx".to_owned(),
                "libplacebo".to_owned(),
                "tonemap_opencl".to_owned(),
            ]),
        );
        inventory
            .groups
            .insert("bsfs".to_owned(), BTreeSet::from(["dovi_rpu".to_owned()]));
        let facts = media_support(&inventory);
        assert!(
            facts
                .iter()
                .find(|fact| fact.id == "ac4")
                .expect("AC4")
                .support_available
        );
        assert!(
            facts
                .iter()
                .find(|fact| fact.id == "dolby_vision")
                .expect("DV")
                .support_available
        );
        assert!(
            !facts
                .iter()
                .find(|fact| fact.id == "vulkan")
                .expect("Vulkan")
                .support_available
        );
        inventory.groups.insert(
            "hwaccels".to_owned(),
            BTreeSet::from(["vulkan".to_owned(), "opencl".to_owned()]),
        );
        assert!(media_support(&inventory)
            .iter()
            .all(|fact| fact.support_available));
    }

    #[test]
    fn six_facts_separate_cpu_fallback_from_saved_mac_enablement_and_acceptance() {
        let mut system = crate::state::SystemInfo::default();
        system.encoders.videotoolbox = true;
        let mut support = SupportReport {
            hwaccels: BTreeSet::from(["videotoolbox".to_owned()]),
            filters: [
                "subtitles",
                "bwdif",
                "tonemap",
                "zscale",
                "scale_vt",
                "tonemap_videotoolbox",
                "subtitles_vt_images",
                "overlay_videotoolbox",
                "bwdif_videotoolbox",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            ..SupportReport::default()
        };
        // No external acceptance or successful graph report is present. The
        // configured switch remains enabled, while stream compatibility is
        // still decided separately by the production resolver.
        let enabled = transcoding_facts_for_platform(
            &system,
            &support,
            Encoder::VideoToolbox,
            true,
            "macos",
            true,
        );
        assert_eq!(enabled.len(), 6);
        assert!(enabled.iter().all(|fact| fact.enabled));
        println!(
            "System facts example: {}",
            serde_json::to_string(&enabled).expect("wire facts")
        );
        let disabled = transcoding_facts_for_platform(
            &system,
            &support,
            Encoder::VideoToolbox,
            false,
            "macos",
            true,
        );
        assert!(
            !disabled
                .iter()
                .find(|fact| fact.id == "hardware_scaling")
                .expect("scale")
                .enabled
        );
        for id in ["tone_mapping", "subtitle_burn_in", "deinterlacing"] {
            let fact = disabled
                .iter()
                .find(|fact| fact.id == id)
                .expect("CPU fallback");
            assert!(fact.enabled);
            assert!(!fact.hardware);
        }
        let software = transcoding_facts_for_platform(
            &system,
            &support,
            Encoder::Software,
            true,
            "macos",
            true,
        );
        assert!(!software.iter().any(|fact| fact.hardware));
        system.tone_map.selected = Pipeline::VtToneMapMetal.name().to_owned();
        let saved_off = transcoding_facts_for_platform(
            &system,
            &support,
            Encoder::VideoToolbox,
            false,
            "macos",
            true,
        );
        assert!(!saved_off
            .iter()
            .filter(|fact| fact.id == "tone_mapping" || fact.id == "hardware_scaling")
            .any(|fact| fact.hardware));
        system.tone_map.selected = Pipeline::LibplaceboSoftware.name().to_owned();
        support.filters.insert("libplacebo".to_owned());
        support.hwaccels.insert("vulkan".to_owned());
        let software_gpu = transcoding_facts_for_platform(
            &system,
            &support,
            Encoder::Software,
            false,
            "linux",
            true,
        );
        assert!(
            software_gpu
                .iter()
                .find(|fact| fact.id == "tone_mapping")
                .expect("software-frame Vulkan route")
                .hardware
        );
        let mut forced_cpu_decode = enabled.clone();
        apply_decode_compatibility(&mut forced_cpu_decode, Some("off"));
        assert!(
            !forced_cpu_decode
                .iter()
                .find(|fact| fact.id == "hardware_decoding")
                .expect("forced CPU decode")
                .enabled
        );
        assert!(
            forced_cpu_decode
                .iter()
                .find(|fact| fact.id == "hardware_encoding")
                .expect("independent encoder")
                .enabled
        );

        let no_slots = transcoding_facts_for_platform(
            &system,
            &support,
            Encoder::VideoToolbox,
            true,
            "macos",
            false,
        );
        assert!(!no_slots.iter().any(|fact| fact.hardware));
        for id in ["tone_mapping", "subtitle_burn_in", "deinterlacing"] {
            assert!(
                no_slots
                    .iter()
                    .find(|fact| fact.id == id)
                    .expect("CPU fallback with no hardware slots")
                    .enabled
            );
        }
    }

    #[test]
    fn saved_mac_preference_requires_build_facilities_and_processing_never_borrows_encoder_binding()
    {
        let mut system = crate::state::SystemInfo::default();
        system.encoders.videotoolbox = true;
        system
            .encoders
            .device_bindings
            .push(plurx_core::transcode::EncoderDeviceBinding {
                backend: "videotoolbox".to_owned(),
                device_ids: vec!["encoder-device".to_owned()],
                binding: "configured".to_owned(),
                detail: "Encoder device".to_owned(),
            });
        let mut support = SupportReport {
            filters: [
                "tonemap",
                "subtitles_vt_images",
                "overlay_videotoolbox",
                "bwdif_videotoolbox",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            ..SupportReport::default()
        };
        let absent = transcoding_facts_for_platform(
            &system,
            &support,
            Encoder::VideoToolbox,
            true,
            "macos",
            true,
        );
        assert!(
            !absent
                .iter()
                .find(|fact| fact.id == "tone_mapping")
                .expect("tone map")
                .hardware
        );
        assert!(
            !absent
                .iter()
                .find(|fact| fact.id == "hardware_scaling")
                .expect("scale")
                .enabled
        );
        assert!(!absent
            .iter()
            .filter(|fact| !matches!(fact.id.as_str(), "hardware_encoding" | "hardware_decoding"))
            .any(|fact| fact.hardware));
        support
            .filters
            .extend(["scale_vt".to_owned(), "tonemap_videotoolbox".to_owned()]);
        let present = transcoding_facts_for_platform(
            &system,
            &support,
            Encoder::VideoToolbox,
            true,
            "macos",
            true,
        );
        let tone = present
            .iter()
            .find(|fact| fact.id == "tone_mapping")
            .expect("tone map");
        assert!(tone.hardware);
        assert!(tone.detail.contains("Metal · GPU acceleration"));
        for fact in present
            .iter()
            .filter(|fact| fact.id != "hardware_encoding" && fact.id != "hardware_decoding")
        {
            assert!(fact.device_ids.is_empty());
        }
    }

    #[test]
    fn system_fact_examples_share_production_projection() {
        let mut mac = crate::state::SystemInfo::default();
        mac.encoders.videotoolbox = true;
        let mac_support = SupportReport {
            hwaccels: BTreeSet::from(["videotoolbox".to_owned()]),
            filters: [
                "subtitles",
                "bwdif",
                "tonemap",
                "zscale",
                "scale_vt",
                "tonemap_videotoolbox",
                "subtitles_vt_images",
                "overlay_videotoolbox",
                "bwdif_videotoolbox",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            ..SupportReport::default()
        };
        let mut linux = crate::state::SystemInfo::default();
        linux.encoders.nvenc = true;
        linux.tone_map.selected = Pipeline::TonemapCuda.name().to_owned();
        let linux_support = SupportReport {
            hwaccels: BTreeSet::from(["cuda".to_owned()]),
            filters: [
                "subtitles",
                "bwdif",
                "tonemap",
                "zscale",
                "tonemap_cuda",
                "scale_cuda",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            ..SupportReport::default()
        };
        let empty = crate::state::SystemInfo::default();
        let empty_support = SupportReport::default();
        for (name, platform, system, support, encoder) in [
            ("macos", "macos", &mac, &mac_support, Encoder::VideoToolbox),
            ("linux", "linux", &linux, &linux_support, Encoder::Nvenc),
            (
                "unavailable",
                "linux",
                &empty,
                &empty_support,
                Encoder::Software,
            ),
        ] {
            let facts =
                transcoding_facts_for_platform(system, support, encoder, true, platform, true);
            assert_eq!(facts.len(), 6);
            if name == "unavailable" {
                assert!(!facts.iter().any(|fact| fact.enabled));
            } else {
                assert!(facts.iter().all(|fact| fact.enabled));
            }
            let value = serde_json::json!({"platform": platform, "transcoding_facts": facts});
            if let Ok(directory) = std::env::var("PLURX_SYSTEM_FACTS_EXAMPLES_DIR") {
                let directory = std::path::Path::new(&directory);
                std::fs::create_dir_all(directory).expect("example output directory");
                std::fs::write(
                    directory.join(format!("system-facts-{name}.json")),
                    serde_json::to_vec_pretty(&value).expect("example JSON"),
                )
                .expect("write exact wire example");
            }
        }
    }

    fn missing(platform: &str, vendors: &[&str], caps: EncoderCaps) -> Vec<String> {
        let contract = serde_json::from_str(CONTRACT).expect("checked-in support contract");
        let vendors = vendors.iter().map(|vendor| (*vendor).to_owned()).collect();
        rows(
            contract,
            &hardware_profiles(platform, &vendors, &caps),
            &Inventory::default(),
        )
        .into_iter()
        .filter(|row| row.required)
        .map(|row| row.id)
        .collect()
    }

    #[test]
    fn hardware_matrix_requires_only_the_node_platform_and_detected_gpu() {
        let mac = missing("macos", &[], EncoderCaps::default());
        for name in ["videotoolbox", "metal", "metal_subtitles", "vulkan"] {
            assert!(mac.contains(&name.to_owned()));
        }
        for name in ["qsv", "vaapi", "nvenc", "cuda"] {
            assert!(!mac.contains(&name.to_owned()));
        }
        let intel = missing("linux", &["0x8086"], EncoderCaps::default());
        assert!(intel.contains(&"qsv".to_owned()));
        assert!(intel.contains(&"vaapi".to_owned()));
        assert!(!intel.contains(&"metal".to_owned()));
        assert!(!intel.contains(&"nvenc".to_owned()));
        let amd = missing("linux", &["0x1002"], EncoderCaps::default());
        assert!(amd.contains(&"vaapi".to_owned()));
        assert!(!amd.contains(&"qsv".to_owned()));
        let nvidia = missing("linux", &["0x10de"], EncoderCaps::default());
        assert!(nvidia.contains(&"nvenc".to_owned()));
        assert!(nvidia.contains(&"cuda".to_owned()));
        assert!(!nvidia.contains(&"vaapi".to_owned()));
        assert_eq!(
            missing("linux", &[], EncoderCaps::default()),
            vec!["software", "media"]
        );
    }

    #[test]
    fn missing_filter_option_blocks_metal_even_when_the_filter_exists() {
        let contract: Contract =
            serde_json::from_str(CONTRACT).expect("checked-in support contract");
        let mut inventory = Inventory::default();
        inventory.groups.insert(
            "filters".to_owned(),
            BTreeSet::from(["scale_vt".to_owned(), "tonemap_videotoolbox".to_owned()]),
        );
        inventory.options.insert(
            "tonemap_videotoolbox".to_owned(),
            BTreeSet::from(["apply_dovi".to_owned(), "require_dovi".to_owned()]),
        );
        let rows = rows(contract, &BTreeSet::from(["macos".to_owned()]), &inventory);
        let metal = rows
            .iter()
            .find(|row| row.id == "metal")
            .expect("Metal row");
        assert!(!metal.included);
        assert_eq!(metal.runtime, "blocked");
        assert_eq!(metal.missing, vec!["tonemap_videotoolbox: reference_white"]);
    }

    #[test]
    fn inventory_parsing_requires_actual_declarations() {
        assert_eq!(
            declared_names(
                "Filters:\n .. scale_vt V->V Scale\n ... libplacebo V->V Tone-map\n",
                "filters"
            ),
            BTreeSet::from(["scale_vt".to_owned(), "libplacebo".to_owned()])
        );
        assert!(
            !declared_names("This build has no libplacebo filter", "filters")
                .contains("libplacebo")
        );
        assert!(
            declared_options(" reference_white <double> ..FV.. (default 203)")
                .contains("reference_white")
        );
        assert!(!declared_options("Unknown option reference_white").contains("reference_white"));
    }

    #[test]
    fn windows_gpu_inventory_does_not_depend_on_a_working_ffmpeg_encoder() {
        let vendors = windows_gpu_vendors("PCI\\VEN_10DE&DEV_1234\nPCI\\VEN_8086&DEV_5678");
        let required = missing(
            "windows",
            &vendors.iter().map(String::as_str).collect::<Vec<_>>(),
            EncoderCaps::default(),
        );
        assert!(required.contains(&"nvenc".to_owned()));
        assert!(required.contains(&"qsv".to_owned()));
        assert!(!required.contains(&"vaapi".to_owned()));
        assert!(!required.contains(&"metal".to_owned()));
        let amd = missing("windows", &["0x1002"], EncoderCaps::default());
        assert!(amd.contains(&"vulkan".to_owned()));
        assert!(!amd.contains(&"vaapi".to_owned()));
        assert!(!amd.contains(&"vaapi_processing".to_owned()));
    }

    #[test]
    fn package_companion_hash_detects_altered_media_bytes() {
        let temporary = tempfile::tempdir().expect("private package fixture");
        let file = temporary.path().join("ffmpeg");
        std::fs::write(&file, b"verified-media").expect("write verified package fixture");
        let expected = hex::encode(Sha256::digest(b"verified-media"));
        assert!(verify_file(temporary.path(), "ffmpeg", Some(&expected)).is_ok());
        std::fs::write(file, b"different-media").expect("alter package fixture");
        assert!(verify_file(temporary.path(), "ffmpeg", Some(&expected)).is_err());
        assert!(verify_file(temporary.path(), "ffprobe", None).is_err());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "requires PLURX_TEST_FFMPEG pointing at a complete native Mac package"]
    async fn complete_macos_package_runs_metal_and_vulkan() {
        let ffmpeg = std::env::var("PLURX_TEST_FFMPEG").expect("explicit native package");
        let contract = serde_json::from_str(CONTRACT).expect("support contract");
        verify_macos_package(
            Path::new(&ffmpeg).parent().expect("package directory"),
            &contract,
        )
        .expect("complete pinned package integrity");
        processing_smoke(&ffmpeg, Pipeline::VtToneMapMetal)
            .await
            .expect("actual Metal PQ-to-SDR graph");
        processing_smoke(&ffmpeg, Pipeline::Libplacebo)
            .await
            .expect("actual Vulkan/libplacebo PQ-to-SDR graph");
    }
}
