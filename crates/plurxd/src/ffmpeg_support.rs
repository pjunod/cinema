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

#[derive(Clone, Debug, Serialize)]
pub struct SupportReport {
    pub executable: String,
    pub expected_executable: Option<String>,
    pub package_verified: bool,
    pub package_problems: Vec<String>,
    pub rows: Vec<SupportRow>,
    pub checked_at: u64,
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
    let mut inventory = Inventory::default();
    match probe(ffmpeg, &["-version"]).await {
        Ok(text)
            if text
                .lines()
                .next()
                .is_some_and(|line| line.to_ascii_lowercase().contains("jellyfin")) => {}
        Ok(_) => report
            .package_problems
            .push("Selected FFmpeg is not a Jellyfin build".to_owned()),
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
