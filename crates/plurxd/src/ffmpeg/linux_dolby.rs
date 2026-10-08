//! Exact-package strict P5 observations, invoked by the existing serial video
//! compatibility owner. This module owns no timer, retry or media producer.

use plurx_core::transcode::{LinuxDolbyContext, LinuxDolbyIdentity};
use serde_json::{json, Value};
use std::sync::Arc;

const UHD_MANIFEST: &[u8] = include_bytes!("../../fixtures/linux-dolby/manifest.json");
const UHD_MEDIA: &[(&str, &[u8])] = &[
    (
        "p5_4k24_fresh.mp4",
        include_bytes!("../../fixtures/linux-dolby/p5_4k24_fresh.mp4"),
    ),
    (
        "p5_4k24_missing_midstream.mp4",
        include_bytes!("../../fixtures/linux-dolby/p5_4k24_missing_midstream.mp4"),
    ),
];

fn validate_uhd_corpus() -> Result<(), &'static str> {
    use sha2::{Digest, Sha256};
    let document: Value =
        serde_json::from_slice(UHD_MANIFEST).map_err(|_| "invalid_embedded_corpus")?;
    if document["schema_version"] != 1
        || document["generator_recipe_version"] != 1
        || document["license"] != "CC0-1.0"
        || document["source_shape"] != json!([3840, 2160])
        || document["frame_count"] != 24
        || document["frame_rate"] != json!([24, 1])
        || document["source_sample_aspect_ratio"] != "1:1"
        || document["dolby_vision_level"] != 6
        || document["source_authority_sha256"]
            != "d86402326c3ef3b88c455ac089c8b94aa27fcbade48eb349499043f54ba5d062"
        || document["cases"].as_array().map(Vec::len) != Some(UHD_MEDIA.len())
    {
        return Err("invalid_embedded_corpus");
    }
    for (index, (name, bytes)) in UHD_MEDIA.iter().enumerate() {
        let case = &document["cases"][index];
        if case["path"] != *name
            || case["sha256"] != hex::encode(Sha256::digest(bytes))
            || case["byte_length"].as_u64() != Some(bytes.len() as u64)
            || (index == 0
                && (!case["negative_max_frames"].is_null()
                    || !case["affected_decode_sample"].is_null()))
            || (index == 1
                && (case["negative_max_frames"] != 10 || case["affected_decode_sample"] != 12))
        {
            return Err("invalid_embedded_corpus");
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
struct PreparedMedia {
    directory: plurx_core::fs_secure::SecureDirectory,
}

#[cfg(target_os = "linux")]
struct MediaPreparationFence(Arc<std::sync::atomic::AtomicBool>);

#[cfg(target_os = "linux")]
impl Drop for MediaPreparationFence {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

#[cfg(target_os = "linux")]
async fn prepare_media(
    root: &std::path::Path,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<PreparedMedia, &'static str> {
    use sha2::{Digest, Sha256};
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let fence = MediaPreparationFence(Arc::new(std::sync::atomic::AtomicBool::new(true)));
    validate_uhd_corpus()?;
    let parent = plurx_core::fs_secure::SecureDirectory::open(root)
        .await
        .map_err(|_| "cache_unavailable")?;
    let owner = parent
        .create_child_directory("linux-dolby")
        .await
        .map_err(|_| "cache_unavailable")?;
    crate::macos_video::verify_private_directory(&owner)
        .await
        .map_err(|_| "cache_unavailable")?;
    let key = hex::encode(Sha256::digest(
        [UHD_MANIFEST, crate::macos_video::p5::MANIFEST].concat(),
    ));
    let directory = owner
        .create_child_directory(&key)
        .await
        .map_err(|_| "cache_unavailable")?;
    crate::macos_video::verify_private_directory(&directory)
        .await
        .map_err(|_| "cache_unavailable")?;
    for (_, bytes) in crate::macos_video::p5::MEDIA.iter().chain(UHD_MEDIA) {
        if cancel.is_cancelled() {
            return Err("cancelled");
        }
        if std::time::Instant::now() >= deadline {
            return Err("preparation_timed_out");
        }
        let name = format!("{}.mp4", hex::encode(Sha256::digest(bytes)));
        if directory
            .read_bounded_child(&name, bytes.len() as u64)
            .await
            .is_ok_and(|existing| existing == *bytes)
        {
            continue;
        }
        let token = cancel.clone();
        let active = Arc::clone(&fence.0);
        directory
            .atomic_write_child_cooperative(&name, bytes, move || {
                !token.is_cancelled()
                    && active.load(std::sync::atomic::Ordering::Acquire)
                    && std::time::Instant::now() < deadline
            })
            .await
            .map_err(|_| "cache_unavailable")?;
        if directory
            .read_bounded_child(&name, bytes.len() as u64)
            .await
            .map_err(|_| "cache_unavailable")?
            != *bytes
        {
            return Err("cache_unavailable");
        }
    }
    Ok(PreparedMedia { directory })
}

#[cfg(target_os = "linux")]
impl PreparedMedia {
    async fn held(&self, bytes: &[u8]) -> Result<std::fs::File, &'static str> {
        use sha2::{Digest, Sha256};
        let digest = hex::encode(Sha256::digest(bytes));
        self.held_named(&format!("{digest}.mp4"), bytes).await
    }

    async fn held_named(&self, name: &str, bytes: &[u8]) -> Result<std::fs::File, &'static str> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let mut file = self
            .directory
            .open_read_child(name)
            .await
            .map_err(|_| "cache_unavailable")?;
        let mut actual = Vec::with_capacity(bytes.len());
        (&mut file)
            .take(bytes.len() as u64 + 1)
            .read_to_end(&mut actual)
            .await
            .map_err(|_| "cache_unavailable")?;
        if actual != bytes {
            return Err("cache_unavailable");
        }
        file.rewind().await.map_err(|_| "cache_unavailable")?;
        Ok(file.into_std().await)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LinuxDolbyReport {
    pub(crate) generation: u64,
    pub(crate) reason: &'static str,
    context: Option<LinuxDolbyContext>,
    graphs: Vec<Value>,
}

impl LinuxDolbyReport {
    pub(crate) fn unavailable(generation: u64, reason: &'static str) -> Self {
        Self {
            generation,
            reason,
            context: None,
            graphs: Vec::new(),
        }
    }

    pub(crate) fn initial() -> Self {
        Self::unavailable(
            0,
            if cfg!(target_os = "linux") {
                "probe_pending"
            } else {
                "unsupported_platform"
            },
        )
    }

    pub(crate) fn context(&self) -> Option<LinuxDolbyContext> {
        self.context.clone()
    }

    pub(crate) fn diagnostics(&self) -> Value {
        // Descriptors, loader paths, kernel strings and physical identifiers
        // never cross this operator wire surface.
        json!({
            "generation": self.generation,
            "availability": if self.context.is_some() { "available" } else if self.reason == "probe_pending" { "pending" } else { "unavailable" },
            "reason": self.reason,
            "implementation": self.context.as_ref().map(|context| identity_diagnostics(context.identity())),
            "graphs": self.graphs,
            "qualification": "observed_exact_package_and_driver",
            "policy": "automatic_with_observed_envelope",
        })
    }
}

fn identity_diagnostics(identity: &LinuxDolbyIdentity) -> Value {
    json!({"digest": identity.digest(), "ffmpeg_sha256": identity.ffmpeg_sha256(),
           "ffprobe_sha256": identity.ffprobe_sha256()})
}

pub(crate) type ReportSnapshot = Arc<LinuxDolbyReport>;

#[cfg(target_os = "linux")]
struct PackageCapture {
    producer: super::EncodedExecutable,
    reporter: super::EncodedExecutable,
    source_digest: String,
    clock: plurx_core::transcode::LinuxSourceClockAssociation,
    objects: Vec<plurx_core::transcode::LinuxExecutionObject>,
    runtime_objects: std::collections::BTreeMap<String, String>,
}

#[cfg(target_os = "linux")]
async fn capture_package() -> Result<PackageCapture, &'static str> {
    use plurx_core::transcode::{LinuxExecutionObject, LinuxExecutionObjectKind};
    use sha2::{Digest, Sha256};
    let producer = super::EncodedExecutable::capture()
        .await
        .map_err(|_| "producer_unattested")?;
    let reporter = super::EncodedExecutable::capture_program(&super::ffprobe_bin())
        .await
        .map_err(|_| "reporter_unattested")?;
    let sealed_parser = super::EncodedExecutable::capture_program(&super::bound_ffprobe_bin())
        .await
        .map_err(|_| "sealed_parser_unattested")?;
    let root = producer.path.parent().ok_or("package_provenance_missing")?;
    let manifest_path = root.join("provenance/linux-dolby-package.json");
    use tokio::io::AsyncReadExt;
    let manifest_file = tokio::fs::File::open(&manifest_path)
        .await
        .map_err(|_| "package_provenance_missing")?;
    let mut bytes = Vec::new();
    manifest_file
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| "package_provenance_missing")?;
    if bytes.is_empty() || bytes.len() > 1024 * 1024 {
        return Err("package_provenance_invalid");
    }
    let manifest: Value =
        serde_json::from_slice(&bytes).map_err(|_| "package_provenance_invalid")?;
    if manifest["schema_version"] != 1
        || manifest["source"]["archive_sha256"]
            != "87bedcefc860cd234cdeddbebdcd3ce45aeedc81d0fbb258ed839ff778be3140"
        || manifest["source"]["commit"] != "253db2a7b0a8045c54ce68ce33d7f601229b1822"
        || manifest["source"]["local_patch_sha256"]
            != "467cecbbf0e70a406cb6bc47130ad49cdda2d81adcdf4d6d4e2d960c5e3b9fe8"
        || manifest["executables"]["ffmpeg_sha256"] != producer.digest
        || manifest["executables"]["ffprobe_sha256"] != reporter.digest
        || manifest["executables"]["sealed_parser_sha256"] != sealed_parser.digest
    {
        return Err("package_provenance_mismatch");
    }
    // A similarly versioned system probe is not the packaged reporter. Its
    // loader origin and clock-source relation must remain explicit.
    if reporter.path.parent() != Some(root) {
        return Err("reporter_package_mismatch");
    }
    if sealed_parser.path == reporter.path {
        return Err("sealed_parser_package_mismatch");
    }
    for key in ["quilt_digest", "sdk_digest", "source_offer_digest"] {
        let value = if key == "quilt_digest" {
            &manifest["source"][key]
        } else {
            &manifest[key]
        };
        if value.as_str().is_none_or(|digest| {
            digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        }) {
            return Err("package_provenance_invalid");
        }
    }
    let runtime_objects: std::collections::BTreeMap<String, String> =
        serde_json::from_value(manifest["runtime_objects"].clone())
            .map_err(|_| "package_runtime_inventory_missing")?;
    if runtime_objects.is_empty()
        || runtime_objects.len() > 512
        || runtime_objects.iter().any(|(role, digest)| {
            role.is_empty()
                || role.contains('/')
                || role.chars().any(char::is_control)
                || digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
        || [
            "libavcodec.so",
            "libavfilter.so",
            "libavformat.so",
            "libavutil.so",
        ]
        .iter()
        .any(|required| {
            !runtime_objects
                .keys()
                .any(|role| role.starts_with(required))
        })
    {
        return Err("package_runtime_inventory_invalid");
    }
    let source_digest = hex::encode(Sha256::digest(
        serde_json::to_vec(&manifest["source"]).map_err(|_| "package_provenance_invalid")?,
    ));
    let (captured_digest, version) = super::hash_engine_object(&manifest_path)
        .await
        .map_err(|_| "package_provenance_changed")?;
    if captured_digest.as_slice() != &Sha256::digest(&bytes)[..] {
        return Err("package_provenance_changed");
    }
    let clock = plurx_core::transcode::LinuxSourceClockAssociation::new(
        hex::encode(captured_digest),
        source_digest.clone(),
        reporter.digest.clone(),
        sealed_parser.digest.clone(),
    )
    .map_err(|_| "package_provenance_invalid")?;
    let objects = vec![
        LinuxExecutionObject {
            path: producer.path.clone(),
            version: Some(producer.object_version.clone()),
            kind: LinuxExecutionObjectKind::ProducerExecutable,
        },
        LinuxExecutionObject {
            path: reporter.path.clone(),
            version: Some(reporter.object_version.clone()),
            kind: LinuxExecutionObjectKind::NativeReporterExecutable,
        },
        LinuxExecutionObject {
            path: sealed_parser.path.clone(),
            version: Some(sealed_parser.object_version.clone()),
            kind: LinuxExecutionObjectKind::SealedParserExecutable,
        },
        LinuxExecutionObject {
            path: manifest_path,
            version: Some(version),
            kind: LinuxExecutionObjectKind::FollowedFile,
        },
    ];
    Ok(PackageCapture {
        producer,
        reporter,
        source_digest,
        clock,
        objects,
        runtime_objects,
    })
}

/// Read the actual dynamic loader's initialization trace from the selected
/// graph. An installed driver pathname or an `ldd` dependency alone does not
/// establish that the decoder used it. Caller supplies LD_DEBUG=libs only to
/// the bounded probe child; production children retain the captured closure.
fn loaded_elf_paths(stderr: &[u8]) -> Result<Vec<std::path::PathBuf>, &'static str> {
    let text = std::str::from_utf8(stderr).map_err(|_| "invalid_loader_trace")?;
    let mut paths = std::collections::BTreeSet::new();
    for line in text.lines() {
        let Some((_, value)) = line.split_once("calling init:") else {
            continue;
        };
        let value = value.trim();
        if value.is_empty()
            || !value.starts_with('/')
            || value.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err("invalid_loader_trace");
        }
        let path = std::path::PathBuf::from(value);
        if path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err("invalid_loader_trace");
        }
        paths.insert(path);
        if paths.len() > 256 {
            return Err("loader_closure_limit");
        }
    }
    if paths.is_empty() {
        return Err("loaded_driver_unobserved");
    }
    Ok(paths.into_iter().collect())
}

fn selected_vaapi_driver(paths: &[std::path::PathBuf]) -> Result<&std::path::Path, &'static str> {
    let mut drivers = paths.iter().filter(|path| {
        path.file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with("_drv_video.so"))
    });
    let driver = drivers.next().ok_or("loaded_driver_unobserved")?;
    if drivers.next().is_some() {
        return Err("ambiguous_loaded_driver");
    }
    // These are candidate VAAPI families, not availability declarations.
    // Each physical/package tuple must pass its own complete observations;
    // today's independent diagnostic receipt covers Intel iHD only.
    if driver
        .file_name()
        .and_then(|name| name.to_str())
        .is_none_or(|name| !matches!(name, "iHD_drv_video.so" | "radeonsi_drv_video.so"))
    {
        return Err("unobserved_driver_class");
    }
    Ok(driver.as_path())
}

#[cfg(target_os = "linux")]
struct LoadedClosure {
    digest: String,
    objects: Vec<plurx_core::transcode::LinuxExecutionObject>,
    initialized: std::collections::BTreeSet<std::path::PathBuf>,
}

/// Retain every initialized ELF, plus the loader's attempted lookup objects.
/// This includes dlopened driver dependencies, which `ldd ffmpeg` omits. The
/// identity hashes content and object roles; private directory spelling is
/// retained only in the daemon's execution fence.
#[cfg(target_os = "linux")]
async fn capture_loaded_closure(
    stderr: &[u8],
    hardware: bool,
) -> Result<LoadedClosure, &'static str> {
    use plurx_core::transcode::{LinuxExecutionObject, LinuxExecutionObjectKind};
    use sha2::{Digest, Sha256};
    let paths = loaded_elf_paths(stderr)?;
    if hardware {
        selected_vaapi_driver(&paths)?;
    }
    let initialized = paths.iter().cloned().collect();
    let mut objects = Vec::new();
    let mut inventory = std::collections::BTreeMap::new();
    let mut retained = std::collections::BTreeSet::new();
    for path in paths {
        let (digest, version) = super::hash_engine_object(&path)
            .await
            .map_err(|_| "loaded_object_changed")?;
        let role = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("invalid_loader_trace")?
            .to_owned();
        if inventory.insert(role, hex::encode(digest)).is_some() {
            return Err("ambiguous_loaded_object");
        }
        retained.insert(path.clone());
        let lookup = tokio::fs::symlink_metadata(&path)
            .await
            .map_err(|_| "loaded_object_changed")?;
        objects.push(LinuxExecutionObject {
            path: path.clone(),
            version: Some(
                super::engine_object_version(&lookup).map_err(|_| "loaded_object_changed")?,
            ),
            kind: LinuxExecutionObjectKind::LookupObject,
        });
        objects.push(LinuxExecutionObject {
            path,
            version: Some(version),
            kind: LinuxExecutionObjectKind::FollowedFile,
        });
    }
    let trace = std::str::from_utf8(stderr).map_err(|_| "invalid_loader_trace")?;
    for line in trace.lines() {
        let Some((_, value)) = line.split_once("trying file=") else {
            continue;
        };
        let value = value.trim();
        let path = std::path::PathBuf::from(value);
        if !path.is_absolute()
            || path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err("invalid_loader_trace");
        }
        if !retained.insert(path.clone()) {
            continue;
        }
        if retained.len() > 4096 {
            return Err("loader_closure_limit");
        }
        let metadata = match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err("loader_lookup_unattested"),
        };
        #[cfg(unix)]
        let (kind, version) = match metadata {
            Some(metadata) => (
                LinuxExecutionObjectKind::LookupObject,
                Some(
                    super::engine_object_version(&metadata)
                        .map_err(|_| "loader_lookup_unattested")?,
                ),
            ),
            None => (LinuxExecutionObjectKind::AbsentLookup, None),
        };
        #[cfg(not(unix))]
        let (kind, version) = {
            let _ = metadata;
            return Err("unsupported_platform");
        };
        objects.push(LinuxExecutionObject {
            path,
            version,
            kind,
        });
    }
    // Loader policy can redirect a later launch even while every previously
    // initialized library remains unchanged. Retain its concrete lookup inputs
    // in the same execution fence; never treat a copied unused driver as proof.
    for path in [
        "/etc/ld.so.cache",
        "/etc/ld.so.conf",
        "/etc/ld.so.conf.d",
        "/etc/ld.so.preload",
    ] {
        let path = std::path::PathBuf::from(path);
        match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) => objects.push(LinuxExecutionObject {
                path,
                version: Some(
                    super::engine_object_version(&metadata)
                        .map_err(|_| "loader_lookup_unattested")?,
                ),
                kind: LinuxExecutionObjectKind::LookupObject,
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                objects.push(LinuxExecutionObject {
                    path,
                    version: None,
                    kind: LinuxExecutionObjectKind::AbsentLookup,
                })
            }
            Err(_) => return Err("loader_lookup_unattested"),
        }
    }
    if let Ok(mut entries) = tokio::fs::read_dir("/etc/ld.so.conf.d").await {
        let mut count = 0;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|_| "loader_lookup_unattested")?
        {
            count += 1;
            if count > 128 {
                return Err("loader_closure_limit");
            }
            let path = entry.path();
            let metadata = tokio::fs::symlink_metadata(&path)
                .await
                .map_err(|_| "loader_lookup_unattested")?;
            objects.push(LinuxExecutionObject {
                path: path.clone(),
                version: Some(
                    super::engine_object_version(&metadata)
                        .map_err(|_| "loader_lookup_unattested")?,
                ),
                kind: LinuxExecutionObjectKind::LookupObject,
            });
            let (_, version) = super::hash_engine_object(&path)
                .await
                .map_err(|_| "loader_lookup_unattested")?;
            objects.push(LinuxExecutionObject {
                path,
                version: Some(version),
                kind: LinuxExecutionObjectKind::FollowedFile,
            });
        }
    }
    let encoded = serde_json::to_vec(&inventory).map_err(|_| "invalid_loader_trace")?;
    Ok(LoadedClosure {
        digest: hex::encode(Sha256::digest(encoded)),
        objects,
        initialized,
    })
}

#[cfg(target_os = "linux")]
async fn capture_kernel_device(device: &std::path::Path) -> Result<LoadedClosure, &'static str> {
    use plurx_core::transcode::{LinuxExecutionObject, LinuxExecutionObjectKind};
    use sha2::{Digest, Sha256};
    let name = device
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("invalid_render_device")?;
    if device.parent() != Some(std::path::Path::new("/dev/dri"))
        || name
            .strip_prefix("renderD")
            .and_then(|index| index.parse::<u32>().ok())
            .is_none_or(|index| !(128..=1023).contains(&index))
    {
        return Err("invalid_render_device");
    }
    let metadata = tokio::fs::metadata(device)
        .await
        .map_err(|_| "render_device_unavailable")?;
    use std::os::unix::fs::FileTypeExt;
    if !metadata.file_type().is_char_device() {
        return Err("invalid_render_device");
    }
    let mut objects = vec![LinuxExecutionObject {
        path: device.to_owned(),
        version: Some(
            super::engine_object_version(&metadata).map_err(|_| "render_device_unavailable")?,
        ),
        kind: LinuxExecutionObjectKind::FollowedFile,
    }];
    let mut inventory = std::collections::BTreeMap::new();
    let sysfs = std::path::PathBuf::from("/sys/class/drm")
        .join(name)
        .join("device");
    use std::os::unix::fs::MetadataExt;
    let actual_sysfs = std::path::PathBuf::from(format!(
        "/sys/dev/char/{}:{}/device",
        libc::major(metadata.rdev()),
        libc::minor(metadata.rdev())
    ));
    if tokio::fs::canonicalize(&actual_sysfs)
        .await
        .map_err(|_| "kernel_device_unattested")?
        != tokio::fs::canonicalize(&sysfs)
            .await
            .map_err(|_| "kernel_device_unattested")?
    {
        return Err("kernel_device_mismatch");
    }
    for (role, path) in [
        (
            "kernel_release",
            std::path::PathBuf::from("/proc/sys/kernel/osrelease"),
        ),
        ("pci_vendor", sysfs.join("vendor")),
        ("pci_device", sysfs.join("device")),
        ("device_uevent", sysfs.join("uevent")),
    ] {
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|_| "kernel_device_unattested")?;
        if bytes.is_empty() || bytes.len() > 256 * 1024 {
            return Err("kernel_device_unattested");
        }
        let digest = hex::encode(Sha256::digest(bytes));
        inventory.insert(role, digest.clone());
        objects.push(LinuxExecutionObject {
            path,
            version: Some(digest),
            kind: LinuxExecutionObjectKind::ContentDigest,
        });
    }
    let driver = sysfs.join("driver");
    let driver_metadata = tokio::fs::symlink_metadata(&driver)
        .await
        .map_err(|_| "kernel_device_unattested")?;
    let driver_target = tokio::fs::read_link(&driver)
        .await
        .map_err(|_| "kernel_device_unattested")?;
    inventory.insert(
        "kernel_driver",
        hex::encode(Sha256::digest(driver_target.as_os_str().as_encoded_bytes())),
    );
    objects.push(LinuxExecutionObject {
        path: driver,
        version: Some(
            super::engine_object_version(&driver_metadata)
                .map_err(|_| "kernel_device_unattested")?,
        ),
        kind: LinuxExecutionObjectKind::LookupObject,
    });
    Ok(LoadedClosure {
        digest: hex::encode(Sha256::digest(
            serde_json::to_vec(&inventory).map_err(|_| "kernel_device_unattested")?,
        )),
        objects,
        initialized: Default::default(),
    })
}

fn observe_frozen_loader_paths(
    stderr: &[u8],
    captured: &std::collections::BTreeSet<std::path::PathBuf>,
    positive: bool,
) -> Result<(), &'static str> {
    let observed: std::collections::BTreeSet<_> = loaded_elf_paths(stderr)?.into_iter().collect();
    if !observed.is_subset(captured) || (positive && observed != *captured) {
        return Err("loaded_closure_changed");
    }
    if positive
        && captured.iter().any(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with("_drv_video.so"))
        })
    {
        selected_vaapi_driver(&observed.into_iter().collect::<Vec<_>>())?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct GraphCase<'a> {
    bytes: &'a [u8],
    decoder: plurx_core::transcode::DecodeBackend,
    encoder: plurx_core::transcode::Encoder,
    uhd24: bool,
    negative: bool,
    seek_seconds: u32,
}

#[cfg(target_os = "linux")]
async fn graph_command(
    package: &PackageCapture,
    media: &PreparedMedia,
    case: GraphCase<'_>,
    device: &str,
    child_env: &[(String, String)],
) -> Result<(tokio::process::Command, std::fs::File), &'static str> {
    use plurx_core::transcode::{
        DecodeBackend, EffectiveRateControl, OutputGrade, Pipeline, StrictDolbyPolicy, VideoCodec,
    };
    let file = media.held(case.bytes).await?;
    let mut command = tokio::process::Command::new(&package.producer.path);
    super::inherit_file_descriptors(&mut command, &[(&file, 3)]);
    command
        .envs(child_env.iter().map(|(key, value)| (key, value)))
        .env("LD_DEBUG", "libs");
    command.args([
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "info",
        "-xerror",
        "-threads",
        "2",
        "-filter_threads",
        "2",
        "-protocol_whitelist",
        "file,pipe",
    ]);
    command.args(
        StrictDolbyPolicy::linux_encoder_args_for_device(device, case.encoder)
            .ok_or("unsupported_encoder")?,
    );
    command.args(
        Pipeline::DoviStrictTonemapx
            .strict_dolby_input_args_for_backend(case.decoder)
            .ok_or("unsupported_decoder")?,
    );
    if case.decoder == DecodeBackend::Vaapi {
        command.args(["-hwaccel_device", device]);
    }
    if case.seek_seconds != 0 {
        command.args(["-ss", &case.seek_seconds.to_string()]);
    }
    command.args([
        "-i",
        "/dev/fd/3",
        "-map",
        "0:v:0",
        "-an",
        "-sn",
        "-dn",
        "-vf",
    ]);
    let (width, height) = if case.uhd24 && !case.negative {
        (1920, 1080)
    } else {
        (160, 90)
    };
    let mut filter = Pipeline::DoviStrictTonemapx
        .filters(Some(width), height, Some("dovi"))
        .ok_or("unsupported_renderer")?;
    let mapper = filter.find("tonemapx=").ok_or("unsupported_renderer")?;
    filter.insert_str(mapper, "showinfo@plurx_p5_contract=checksum=0,");
    if case.decoder == DecodeBackend::Vaapi {
        filter = format!("showinfo@plurx_p5_hardware=checksum=0,hwdownload,format=p010le,setparams=colorspace=unknown,{filter}");
    }
    if !case.negative {
        if let Some(upload) = Pipeline::DoviStrictTonemapx.encoder_upload(case.encoder) {
            filter.push(',');
            filter.push_str(upload);
        }
    }
    command.arg(filter).args([
        "-frames:v",
        "24",
        "-fps_mode",
        "passthrough",
        "-threads",
        "2",
    ]);
    if case.negative {
        command.args(["-pix_fmt", "yuv420p", "-f", "rawvideo", "pipe:1"]);
    } else {
        command.args(case.encoder.encode_args_for_codec(
            VideoCodec::H264,
            OutputGrade::Sdr,
            if case.uhd24 { 6000 } else { 500 },
            EffectiveRateControl::Vbr,
            false,
            None,
        ));
        // Tiny controls isolate selected-frame metadata/color association.
        // Full 4K encoder qualification retains the ordinary VOD B-frame
        // policy supplied by the shared encoder projection above.
        if !case.uhd24 {
            command.args(["-bf", "0"]);
        }
        command.args([
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-colorspace",
            "bt709",
            "-color_range",
            "tv",
            "-f",
            "mp4",
            "-movflags",
            "frag_keyframe+empty_moov+default_base_moof",
            "pipe:1",
        ]);
    }
    Ok((command, file))
}

#[cfg(target_os = "linux")]
async fn run_graph(
    package: &PackageCapture,
    media: &PreparedMedia,
    case: GraphCase<'_>,
    device: &str,
    child_env: &[(String, String)],
    deadline: tokio::time::Instant,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<super::BoundedCommandOutcome, &'static str> {
    let (command, _held_file) = graph_command(package, media, case, device, child_env).await?;
    let budget = deadline
        .saturating_duration_since(tokio::time::Instant::now())
        .min(std::time::Duration::from_secs(if case.uhd24 {
            30
        } else {
            10
        }));
    if budget.is_zero() {
        return Err("probe_deadline");
    }
    super::bounded_command_capture_cancellable(
        command,
        budget,
        if case.negative {
            24 * 160 * 90 * 3 / 2
        } else {
            4 * 1024 * 1024
        },
        "Linux strict Dolby graph",
        Some(cancel),
        crate::process_control::ChildWork::background("video processing compatibility probe"),
    )
    .await
    .map_err(|_| {
        if cancel.is_cancelled() {
            "cancelled"
        } else {
            "graph_failed"
        }
    })
}

#[cfg(target_os = "linux")]
fn observe_selected_frames(stderr: &[u8], case: GraphCase<'_>) -> Result<(), &'static str> {
    let metadata = |format, observer| {
        if case.uhd24 {
            crate::macos_video::p5::observe_selected_4k_metadata(stderr, format, observer)
        } else {
            crate::macos_video::p5::observe_selected_layout_metadata(
                stderr,
                // Only the existing variable fixture carries changing signatures.
                case.bytes
                    == crate::macos_video::p5::MEDIA
                        .iter()
                        .find(|(id, _)| *id == "variable")
                        .map(|(_, bytes)| *bytes)
                        .unwrap_or(&[]),
                format,
                observer,
            )
        }
    };
    metadata("yuv420p10le", "plurx_p5_contract").map_err(|_| "selected_metadata_failed")?;
    if case.decoder == plurx_core::transcode::DecodeBackend::Vaapi {
        metadata("vaapi", "plurx_p5_hardware").map_err(|_| "opaque_hardware_failed")?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
async fn inspect_output(
    package: &PackageCapture,
    media: &PreparedMedia,
    bytes: &[u8],
    case: GraphCase<'_>,
    child_env: &[(String, String)],
    deadline: tokio::time::Instant,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<(), &'static str> {
    use sha2::{Digest, Sha256};
    let name = format!("output-{}.mp4", hex::encode(Sha256::digest(bytes)));
    let token = cancel.clone();
    let preparation_deadline =
        std::time::Instant::now() + deadline.saturating_duration_since(tokio::time::Instant::now());
    media
        .directory
        .atomic_write_child_cooperative(&name, bytes, move || {
            !token.is_cancelled() && std::time::Instant::now() < preparation_deadline
        })
        .await
        .map_err(|_| "cache_unavailable")?;
    let file = media.held_named(&name, bytes).await?;
    let mut query = tokio::process::Command::new(&package.reporter.path);
    super::inherit_file_descriptors(&mut query, &[(&file, 3)]);
    query
        .envs(child_env.iter().map(|(key, value)| (key, value)))
        .args([
            "-v",
            "error",
            "-threads",
            "2",
            "-protocol_whitelist",
            "file,pipe",
            "-show_frames",
            "-show_streams",
            "-of",
            "json",
            "/dev/fd/3",
        ]);
    let budget = deadline
        .saturating_duration_since(tokio::time::Instant::now())
        .min(std::time::Duration::from_secs(10));
    if budget.is_zero() {
        return Err("probe_deadline");
    }
    let document = super::bounded_command_output_cancellable(
        query,
        budget,
        1024 * 1024,
        "Linux Dolby output metadata",
        Some(cancel),
        crate::process_control::ChildWork::background("video processing compatibility probe"),
    )
    .await
    .map_err(|_| "output_probe_failed")?;
    let document: Value =
        serde_json::from_slice(&document.stdout).map_err(|_| "output_contract_failed")?;
    // The inspection decoder uses the same retained producer image. Sampling
    // its actual delivered pixels never substitutes a declared stream tag.
    let file = media.held_named(&name, bytes).await?;
    let mut decode = tokio::process::Command::new(&package.producer.path);
    super::inherit_file_descriptors(&mut decode, &[(&file, 3)]);
    decode
        .envs(child_env.iter().map(|(key, value)| (key, value)))
        .args([
            "-nostdin",
            "-v",
            "error",
            "-threads",
            "2",
            "-filter_threads",
            "2",
            "-hwaccel",
            "none",
            "-protocol_whitelist",
            "file,pipe",
            "-i",
            "/dev/fd/3",
            "-map",
            "0:v:0",
            "-an",
            "-sn",
            "-dn",
            "-vf",
            "scale=160:90,format=yuv420p",
            "-frames:v",
            "24",
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "pipe:1",
        ]);
    let budget = deadline
        .saturating_duration_since(tokio::time::Instant::now())
        .min(std::time::Duration::from_secs(10));
    if budget.is_zero() {
        return Err("probe_deadline");
    }
    let decoded = super::bounded_command_output_cancellable(
        decode,
        budget,
        24 * 160 * 90 * 3 / 2,
        "Linux Dolby delivered pixels",
        Some(cancel),
        crate::process_control::ChildWork::background("video processing compatibility probe"),
    )
    .await
    .map_err(|_| "output_probe_failed")?;
    if case.uhd24 {
        observe_uhd_output(&document).map_err(|_| "output_contract_failed")?;
        crate::macos_video::p5::observe_colors(&decoded.stdout)
            .map_err(|_| "output_colors_failed")?;
    } else {
        crate::macos_video::p5::observe(&document, &decoded.stdout)
            .map_err(|_| "output_contract_failed")?;
        crate::macos_video::p5::observe_colors(&decoded.stdout)
            .map_err(|_| "output_colors_failed")?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn observe_uhd_output(document: &Value) -> Result<(), ()> {
    let streams = document["streams"].as_array().ok_or(())?;
    let frames = document["frames"].as_array().ok_or(())?;
    if streams.len() != 1 || frames.len() != 24 {
        return Err(());
    }
    let stream = &streams[0];
    if stream["codec_name"] != "h264"
        || stream["avg_frame_rate"] != "24/1"
        || stream["pix_fmt"] != "yuv420p"
        || stream["sample_aspect_ratio"] != "1:1"
        || stream["field_order"] != "progressive"
    {
        return Err(());
    }
    for value in std::iter::once(stream).chain(frames) {
        if value["width"] != 1920 || value["height"] != 1080 {
            return Err(());
        }
        for (key, expected) in [
            ("color_primaries", "bt709"),
            ("color_transfer", "bt709"),
            ("color_space", "bt709"),
            ("color_range", "tv"),
        ] {
            if value[key] != expected {
                return Err(());
            }
        }
        if value["side_data_list"].as_array().is_some_and(|sides| {
            sides.iter().any(|side| {
                let name = side["side_data_type"]
                    .as_str()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                name.contains("dovi")
                    || name.contains("dolby")
                    || name.contains("mastering")
                    || name.contains("content light")
            })
        }) {
            return Err(());
        }
    }
    let first = frames[0]["best_effort_timestamp_time"]
        .as_str()
        .and_then(|value| value.parse::<f64>().ok())
        .ok_or(())?;
    // Ordinary VOD B-frames can give the fragmented MP4 a small presentation
    // offset. They may not change frame cadence or drop/repeat observations.
    if !first.is_finite() || !(0.0..=2.0 / 24.0 + 0.001).contains(&first) {
        return Err(());
    }
    for (index, frame) in frames.iter().enumerate() {
        let pts = frame["best_effort_timestamp_time"]
            .as_str()
            .and_then(|value| value.parse::<f64>().ok())
            .ok_or(())?;
        if !pts.is_finite() || (pts - first - index as f64 / 24.0).abs() > 0.001 {
            return Err(());
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn merge_execution_objects(
    objects: Vec<plurx_core::transcode::LinuxExecutionObject>,
) -> Result<Vec<plurx_core::transcode::LinuxExecutionObject>, &'static str> {
    let mut captured = std::collections::BTreeMap::new();
    for object in objects {
        let key = (object.path.clone(), format!("{:?}", object.kind));
        if let Some(previous) = captured.insert(key, object.clone()) {
            if previous.version != object.version {
                return Err("implementation_changed_during_discovery");
            }
        }
        if captured.len() > 4096 {
            return Err("loader_closure_limit");
        }
    }
    Ok(captured.into_values().collect())
}

/// Discovery is charged to the existing serial compatibility generation. It
/// establishes lookup paths only. A second execution under the captured fence
/// must prove every admitted graph before a context can be published.
#[cfg(target_os = "linux")]
pub(crate) async fn run_generation(
    generation: u64,
    root: &std::path::Path,
    cancel: &tokio_util::sync::CancellationToken,
) -> LinuxDolbyReport {
    if cancel.is_cancelled() {
        return LinuxDolbyReport::unavailable(generation, "cancelled");
    }
    match qualify_generation(generation, root, cancel).await {
        Ok(report) => report,
        Err(reason) => LinuxDolbyReport::unavailable(generation, reason),
    }
}

#[cfg(target_os = "linux")]
async fn qualify_generation(
    generation: u64,
    root: &std::path::Path,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<LinuxDolbyReport, &'static str> {
    use plurx_core::transcode::Rational;
    use plurx_core::transcode::{
        DecodeBackend, Encoder, LinuxDolbyExecutionBinding, LinuxDolbyObservation,
    };
    use sha2::{Digest, Sha256};
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(210);
    let media = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        prepare_media(root, cancel),
    )
    .await
    .map_err(|_| "preparation_timed_out")??;
    let mut package = capture_package().await?;
    let device = std::env::var("PLURX_VAAPI_DEVICE")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/dev/dri/renderD128".to_owned());
    let kernel = capture_kernel_device(std::path::Path::new(&device)).await?;
    let node = std::path::Path::new(&device)
        .file_name()
        .ok_or("invalid_render_device")?;
    let vendor = tokio::fs::read_to_string(
        std::path::Path::new("/sys/class/drm")
            .join(node)
            .join("device/vendor"),
    )
    .await
    .map_err(|_| "kernel_device_unattested")?;
    let driver = match vendor.trim() {
        "0x8086" => "iHD",
        "0x1002" => "radeonsi",
        _ => return Err("unobserved_driver_class"),
    };
    let library = package
        .producer
        .path
        .parent()
        .ok_or("package_provenance_missing")?
        .join("lib");
    let environment = vec![
        (
            "LD_LIBRARY_PATH".to_owned(),
            library.to_string_lossy().into_owned(),
        ),
        ("LIBVA_DRIVER_NAME_JELLYFIN".to_owned(), driver.to_owned()),
        ("LIBVA_MESSAGING_LEVEL".to_owned(), "2".to_owned()),
    ];
    let mut objects = package.objects.clone();
    objects.extend(kernel.objects);
    let mut discovered = Vec::new();
    let mut closure_digests = Vec::new();
    for decoder in [DecodeBackend::Software, DecodeBackend::Vaapi] {
        for encoder in [Encoder::Software, Encoder::Vaapi, Encoder::Qsv] {
            if cancel.is_cancelled() {
                return Err("cancelled");
            }
            let case = GraphCase {
                bytes: UHD_MEDIA[0].1,
                decoder,
                encoder,
                uhd24: true,
                negative: false,
                seek_seconds: 0,
            };
            // An unsupported encoder contributes no capability. Discovery
            // itself never authorizes its output or metadata semantics.
            let outcome = match run_graph(
                &package,
                &media,
                case,
                &device,
                &environment,
                deadline,
                cancel,
            )
            .await
            {
                Ok(outcome) if outcome.status.success() => outcome,
                Err(reason) if cancel.is_cancelled() || tokio::time::Instant::now() >= deadline => {
                    return Err(reason)
                }
                _ => continue,
            };
            let output = match outcome.output {
                Ok(output) => output,
                Err(_) => continue,
            };
            let closure = match capture_loaded_closure(
                &output.stderr,
                decoder == DecodeBackend::Vaapi || encoder != Encoder::Software,
            )
            .await
            {
                Ok(closure) => closure,
                Err(_) => continue,
            };
            if decoder == DecodeBackend::Vaapi || encoder != Encoder::Software {
                let paths: Vec<_> = closure.initialized.iter().cloned().collect();
                let selected = selected_vaapi_driver(&paths)?;
                if selected.file_name().and_then(|name| name.to_str())
                    != Some(format!("{driver}_drv_video.so").as_str())
                {
                    return Err("loaded_driver_mismatch");
                }
            }
            for object in &closure.objects {
                if object.kind != plurx_core::transcode::LinuxExecutionObjectKind::FollowedFile {
                    continue;
                }
                let role = object
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or("invalid_loader_trace")?;
                if role.starts_with("libav")
                    || role.ends_with("_drv_video.so")
                    || package.runtime_objects.contains_key(role)
                {
                    let expected = package
                        .runtime_objects
                        .get(role)
                        .ok_or("loaded_package_object_unbound")?;
                    let (actual, _) = super::hash_engine_object(&object.path)
                        .await
                        .map_err(|_| "loaded_object_changed")?;
                    if hex::encode(actual) != *expected {
                        return Err("loaded_package_object_mismatch");
                    }
                }
            }
            closure_digests.push(closure.digest);
            objects.extend(closure.objects);
            discovered.push((case, closure.initialized));
        }
    }
    if !discovered
        .iter()
        .any(|(case, _)| case.decoder == DecodeBackend::Vaapi)
    {
        return Err("hardware_graph_unobserved");
    }
    closure_digests.sort();
    let binding =
        LinuxDolbyExecutionBinding::new(merge_execution_objects(objects)?, environment.clone())
            .map_err(|_| "invalid_execution_binding")?
            .with_source_clock(package.clock.clone())
            .map_err(|_| "invalid_execution_binding")?;
    package.producer = package
        .producer
        .clone()
        .bind_linux_execution(binding.clone())
        .map_err(|_| "implementation_changed")?;
    if !package.producer.is_current().await {
        return Err("implementation_changed_during_discovery");
    }
    let linked_digest = hex::encode(Sha256::digest(
        serde_json::to_vec(&closure_digests).map_err(|_| "invalid_execution_binding")?,
    ));
    let environment_digest = hex::encode(Sha256::digest(format!(
        "jellyfin-vaapi:{driver}:derived-qsv-v1"
    )));
    let identity = LinuxDolbyIdentity::new(
        package.producer.digest.clone(),
        package.reporter.digest.clone(),
        package.source_digest.clone(),
        linked_digest,
        environment_digest,
        kernel.digest,
        device.clone(),
    )
    .map_err(|_| "invalid_execution_binding")?;
    let mut observations = Vec::new();
    let mut hardware_accepted = false;
    let mut graphs = Vec::new();
    for (case, initialized) in discovered {
        if !package.producer.is_current().await {
            return Err("implementation_changed");
        }
        let outcome = run_graph(
            &package,
            &media,
            case,
            &device,
            &environment,
            deadline,
            cancel,
        )
        .await?;
        let output = outcome.output.map_err(|_| "graph_failed")?;
        let accepted = outcome.status.success()
            && observe_frozen_loader_paths(&output.stderr, &initialized, true).is_ok()
            && observe_selected_frames(&output.stderr, case).is_ok();
        let result = if accepted {
            inspect_output(
                &package,
                &media,
                &output.stdout,
                case,
                &environment,
                deadline,
                cancel,
            )
            .await
        } else {
            Err("output_contract_failed")
        };
        if !package.producer.is_current().await {
            return Err("implementation_changed");
        }
        if result.is_err() {
            graphs.push(json!({"decoder": format!("{:?}", case.decoder), "encoder": format!("{:?}", case.encoder), "reason": "output_contract_failed"}));
            continue;
        }
        // Effective metadata loss must stop this exact decoder/renderer graph;
        // it cannot skip a failed AU and publish generic PQ output instead.
        let negative = GraphCase {
            bytes: UHD_MEDIA[1].1,
            negative: true,
            ..case
        };
        let rejected = run_graph(
            &package,
            &media,
            negative,
            &device,
            &environment,
            deadline,
            cancel,
        )
        .await?;
        let rejected_output = rejected.output.map_err(|_| "negative_control_failed")?;
        crate::macos_video::p5::observe_negative(
            rejected.status,
            &rejected_output.stdout,
            &rejected_output.stderr,
            10,
        )
        .map_err(|_| "negative_control_failed")?;
        observe_frozen_loader_paths(&rejected_output.stderr, &initialized, false)?;
        if !package.producer.is_current().await {
            return Err("implementation_changed");
        }
        let controls: Value = serde_json::from_slice(crate::macos_video::p5::MANIFEST)
            .map_err(|_| "invalid_embedded_corpus")?;
        for (control, (id, bytes)) in controls["cases"]
            .as_array()
            .ok_or("invalid_embedded_corpus")?
            .iter()
            .zip(crate::macos_video::p5::MEDIA)
        {
            if control["id"] != *id
                || control["sha256"] != hex::encode(Sha256::digest(bytes))
                || control["byte_length"].as_u64() != Some(bytes.len() as u64)
            {
                return Err("invalid_embedded_corpus");
            }
            let negative = !control["negative_max_frames"].is_null();
            let tiny = GraphCase {
                bytes,
                uhd24: false,
                negative,
                seek_seconds: control["seek_seconds"]
                    .as_u64()
                    .ok_or("invalid_embedded_corpus")?
                    .try_into()
                    .map_err(|_| "invalid_embedded_corpus")?,
                ..case
            };
            if !package.producer.is_current().await {
                return Err("implementation_changed");
            }
            let outcome = run_graph(
                &package,
                &media,
                tiny,
                &device,
                &environment,
                deadline,
                cancel,
            )
            .await?;
            let output = outcome.output.map_err(|_| "control_failed")?;
            // Control graphs may initialize fewer objects than the full
            // encoder graph, but may never introduce an uncaptured object.
            observe_frozen_loader_paths(&output.stderr, &initialized, false)?;
            if negative {
                let maximum = control["negative_max_frames"]
                    .as_u64()
                    .ok_or("invalid_embedded_corpus")?
                    .try_into()
                    .map_err(|_| "invalid_embedded_corpus")?;
                crate::macos_video::p5::observe_negative(
                    outcome.status,
                    &output.stdout,
                    &output.stderr,
                    maximum,
                )
                .map_err(|_| "negative_control_failed")?;
            } else {
                if !outcome.status.success() {
                    return Err("positive_control_failed");
                }
                observe_selected_frames(&output.stderr, tiny)?;
                inspect_output(
                    &package,
                    &media,
                    &output.stdout,
                    tiny,
                    &environment,
                    deadline,
                    cancel,
                )
                .await?;
            }
            if !package.producer.is_current().await {
                return Err("implementation_changed");
            }
        }
        hardware_accepted |= case.decoder == DecodeBackend::Vaapi;
        observations.push(
            LinuxDolbyObservation::new(
                case.decoder,
                case.encoder,
                3840,
                2160,
                1920,
                1080,
                Rational::new(24, 1).ok_or("invalid_observed_envelope")?,
            )
            .map_err(|_| "invalid_observed_envelope")?,
        );
        graphs.push(json!({"decoder": format!("{:?}", case.decoder), "encoder": format!("{:?}", case.encoder), "reason": "observed", "source_max": [3840,2160,24], "output_max": [1920,1080]}));
    }
    if !hardware_accepted {
        return Err("hardware_graph_unobserved");
    }
    Ok(LinuxDolbyReport {
        generation,
        reason: "observed",
        context: Some(
            LinuxDolbyContext::new(identity, observations).with_execution_binding(binding),
        ),
        graphs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_neutral_generation_does_not_open_package_or_device() {
        let cancelled = tokio_util::sync::CancellationToken::new();
        cancelled.cancel();
        let report = run_generation(
            9,
            std::path::Path::new("/nonexistent-private-probe-root"),
            &cancelled,
        )
        .await;
        assert_eq!(report.reason, "cancelled");
        assert_eq!(report.generation, 9);
        assert!(report.context().is_none());
    }

    #[test]
    fn operator_report_omits_loader_paths_and_physical_device_identifiers() {
        use plurx_core::transcode::{
            LinuxDolbyExecutionBinding, LinuxExecutionObject, LinuxExecutionObjectKind,
        };
        let identity = LinuxDolbyIdentity::new(
            "1".repeat(64),
            "2".repeat(64),
            "3".repeat(64),
            "4".repeat(64),
            "5".repeat(64),
            "6".repeat(64),
            "/dev/dri/renderD129".to_owned(),
        )
        .expect("hash-only implementation facts");
        let binding = LinuxDolbyExecutionBinding::new(
            vec![LinuxExecutionObject {
                path: "/private/operator/ffmpeg".into(),
                version: Some("private inode token".to_owned()),
                kind: LinuxExecutionObjectKind::ProducerExecutable,
            }],
            vec![(
                "LD_LIBRARY_PATH".to_owned(),
                "/private/operator/lib".to_owned(),
            )],
        )
        .expect("private execution fence");
        let report = LinuxDolbyReport {
            generation: 2,
            reason: "observed",
            context: Some(LinuxDolbyContext::new(identity, vec![]).with_execution_binding(binding)),
            graphs: vec![],
        };
        let document = report.diagnostics();
        let encoded = document.to_string();
        assert_eq!(document["implementation"]["ffmpeg_sha256"], "1".repeat(64));
        for private in [
            "/private/operator",
            "/dev/dri",
            "inode token",
            "LD_LIBRARY_PATH",
        ] {
            assert!(!encoded.contains(private), "operator DTO leaked {private}");
        }
    }

    #[test]
    fn discovery_union_refuses_changed_versions_instead_of_rebinding() {
        use plurx_core::transcode::{LinuxExecutionObject, LinuxExecutionObjectKind};
        let original = LinuxExecutionObject {
            path: "/candidate/libavcodec.so".into(),
            version: Some("inode-a".to_owned()),
            kind: LinuxExecutionObjectKind::FollowedFile,
        };
        assert_eq!(
            merge_execution_objects(vec![original.clone(), original.clone()])
                .expect("unchanged duplicate role")
                .len(),
            1
        );
        let changed = LinuxExecutionObject {
            version: Some("inode-b".to_owned()),
            ..original.clone()
        };
        assert_eq!(
            merge_execution_objects(vec![original, changed])
                .expect_err("changed discovery role cannot be recaptured"),
            "implementation_changed_during_discovery"
        );
    }

    #[tokio::test]
    async fn replacement_after_discovery_cannot_authorize_graph_publication() {
        use plurx_core::transcode::{
            LinuxDolbyExecutionBinding, LinuxExecutionObject, LinuxExecutionObjectKind,
        };
        let root = crate::test_tempdir().expect("discovery fence directory");
        let executable = root.path().join("ffmpeg");
        let library = root.path().join("libavcodec.so");
        std::fs::write(&executable, b"captured producer").expect("producer bytes");
        std::fs::write(&library, b"captured decoder").expect("decoder bytes");
        let producer = super::super::EncodedExecutable::capture_at(executable)
            .await
            .expect("captured producer");
        let (_, version) = super::super::hash_engine_object(&library)
            .await
            .expect("discovered decoder object");
        let binding = LinuxDolbyExecutionBinding::new(
            vec![
                LinuxExecutionObject {
                    path: producer.path.clone(),
                    version: Some(producer.object_version.clone()),
                    kind: LinuxExecutionObjectKind::ProducerExecutable,
                },
                LinuxExecutionObject {
                    path: library.clone(),
                    version: Some(version),
                    kind: LinuxExecutionObjectKind::FollowedFile,
                },
            ],
            vec![],
        )
        .expect("captured discovery binding");
        let producer = producer
            .bind_linux_execution(binding)
            .expect("same producer origin");
        assert!(
            producer.is_current().await,
            "unchanged discovery can proceed to bounded qualification"
        );
        let replacement = root.path().join("replacement.so");
        std::fs::write(&replacement, b"different decoder").expect("replacement bytes");
        std::fs::rename(replacement, library).expect("atomic replacement during discovery");
        assert!(
            !producer.is_current().await,
            "publication must refuse an implementation replaced after discovery"
        );
    }

    #[test]
    fn loaded_driver_requires_selected_graph_trace_not_search_candidates() {
        let search_only = b"1: trying file=/usr/lib/jellyfin-ffmpeg/lib/dri/iHD_drv_video.so\n";
        assert_eq!(
            loaded_elf_paths(search_only).expect_err("lookup is not a loaded object"),
            "loaded_driver_unobserved"
        );
        let trace = b"1: calling init: /lib/libva.so.2\n1: calling init: /usr/lib/jellyfin-ffmpeg/lib/dri/iHD_drv_video.so\n";
        let paths = loaded_elf_paths(trace).expect("actual loaded closure");
        assert_eq!(
            selected_vaapi_driver(&paths)
                .expect("one actual iHD driver")
                .file_name()
                .expect("driver filename"),
            "iHD_drv_video.so"
        );
        let ambiguous = loaded_elf_paths(b"1: calling init: /dri/iHD_drv_video.so\n1: calling init: /dri/radeonsi_drv_video.so\n").expect("parse trace");
        assert_eq!(
            selected_vaapi_driver(&ambiguous).expect_err("multiple physical drivers are ambiguous"),
            "ambiguous_loaded_driver"
        );
        assert_eq!(
            loaded_elf_paths(b"1: calling init: /dri/../iHD_drv_video.so\n")
                .expect_err("traversal rejected"),
            "invalid_loader_trace"
        );
    }

    #[test]
    fn graph_acceptance_rejects_new_loaded_objects_after_discovery() {
        let initial = b"1: calling init: /lib/libva.so.2\n1: calling init: /dri/iHD_drv_video.so\n";
        let frozen = loaded_elf_paths(initial)
            .expect("discovery paths")
            .into_iter()
            .collect();
        assert_eq!(observe_frozen_loader_paths(initial, &frozen, true), Ok(()));
        let expanded = b"1: calling init: /lib/libva.so.2\n1: calling init: /dri/iHD_drv_video.so\n1: calling init: /lib/another_encoder.so\n";
        assert_eq!(
            observe_frozen_loader_paths(expanded, &frozen, true),
            Err("loaded_closure_changed")
        );
        let missing = b"1: calling init: /lib/libva.so.2\n";
        assert_eq!(
            observe_frozen_loader_paths(missing, &frozen, true),
            Err("loaded_closure_changed")
        );
        assert_eq!(observe_frozen_loader_paths(missing, &frozen, false), Ok(()));
    }
}
