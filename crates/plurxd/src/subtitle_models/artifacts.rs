//! Local immutable engine/model artifacts. Installed bytes are not a readiness
//! receipt: the durable owner must still run the production speech self-test.
use plurx_core::fs_secure::FileIdentity;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogModel {
    pub id: String,
    pub label: String,
    pub languages: Vec<String>,
    pub download_bytes: u64,
    pub sha256: String,
    pub url: String,
    pub source: String,
    pub license: String,
}

pub(crate) struct Engine {
    pub handle: File,
    pub path: PathBuf,
    pub binary_sha256: String,
    pub adapter_contract: String,
    pub platform: String,
    pub identity: FileIdentity,
    pub self_test_handle: File,
    pub self_test_identity: FileIdentity,
    pub self_test_sha256: String,
}

pub(crate) fn identity_key(identity: FileIdentity) -> String {
    #[cfg(windows)]
    let high = identity.inode_high;
    #[cfg(not(windows))]
    let high = 0_u64;
    format!(
        "v1:{}:{}:{high}:{}:{}:{}",
        identity.device,
        identity.inode,
        identity.size,
        identity.changed_seconds,
        identity.changed_nanoseconds
    )
}

pub(crate) struct ManagedModel {
    pub handle: File,
    pub path: PathBuf,
    pub sha256: String,
    pub identity: FileIdentity,
}

#[cfg(unix)]
mod local {
    use super::*;
    use plurx_core::fs_secure::{open_read_nofollow_blocking, std_file_identity, SecureDirectory};
    use serde_json::Value;
    use sha2::{Digest, Sha256};
    use std::{
        ffi::CString,
        io::{Read, Seek, SeekFrom},
        os::fd::{AsRawFd, FromRawFd},
        sync::{Mutex, OnceLock},
        time::Duration,
    };
    use tokio::io::AsyncWriteExt;

    const MAX_MODEL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
    const MAX_ENGINE_BYTES: u64 = 64 * 1024 * 1024;
    const DISK_MARGIN: u64 = 64 * 1024 * 1024;
    const REVISION: &str = "5359861c739e955e79d9a303bcbc70fb988958b1";
    const PIN: &str = include_str!("../../../../scripts/subtitle-engine-manifest.json");
    const COMPANIONS: [(&str, u64, &str); 3] = [
        (
            "self-test.wav",
            352078,
            "59dfb9a4acb36fe2a2affc14bacbee2920ff435cb13cc314a08c13f66ba7860e",
        ),
        (
            "whisper.cpp.LICENSE.txt",
            1078,
            "94f29bbed6a22c35b992c5c6ebf0e7c92f13b836b90f36f461c9cf2f0f1d010d",
        ),
        (
            "whisper-models.LICENSE.txt",
            1063,
            "b5d65a59060e68c4ff940e1eddfa6f94b2d68fdf58ed7f4dd57721c997e35e9d",
        ),
    ];
    static DIGESTS: OnceLock<Mutex<Vec<(FileIdentity, String)>>> = OnceLock::new();
    static INSTALL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn cancelled(cancel: &CancellationToken) -> Result<(), String> {
        if cancel.is_cancelled() {
            Err("model_install_cancelled".into())
        } else {
            Ok(())
        }
    }

    fn valid_digest(value: &str) -> bool {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }

    fn validate_spec(spec: &CatalogModel) -> Result<(), String> {
        if !valid_digest(&spec.sha256)
            || spec.download_bytes == 0
            || spec.download_bytes > MAX_MODEL_BYTES
        {
            return Err("invalid_model_catalog_identity".into());
        }
        Ok(())
    }

    fn hash_held(file: &mut File, expected: &str, size: u64) -> Result<FileIdentity, String> {
        hash_held_cancellable(file, expected, size, &CancellationToken::new())
    }
    fn hash_held_cancellable(
        file: &mut File,
        expected: &str,
        size: u64,
        cancel: &CancellationToken,
    ) -> Result<FileIdentity, String> {
        hash_held_observed(file, expected, size, cancel, || {})
    }
    fn hash_held_observed(
        file: &mut File,
        expected: &str,
        size: u64,
        cancel: &CancellationToken,
        mut after_chunk: impl FnMut(),
    ) -> Result<FileIdentity, String> {
        let check = || {
            if cancel.is_cancelled() {
                Err("worker_interrupted".to_owned())
            } else {
                Ok(())
            }
        };
        check()?;
        if !file
            .metadata()
            .map_err(|_| "artifact_metadata_unavailable")?
            .is_file()
        {
            return Err("artifact_not_regular".into());
        }
        let before = std_file_identity(file).map_err(|_| "artifact_identity_unavailable")?;
        if before.size != size {
            return Err("artifact_length_mismatch".into());
        }
        let cache = DIGESTS.get_or_init(|| Mutex::new(Vec::new()));
        let cached = cache
            .lock()
            .map_err(|_| "artifact_verification_unavailable")?
            .iter()
            .any(|(identity, digest)| *identity == before && digest == expected);
        if !cached {
            check()?;
            file.seek(SeekFrom::Start(0))
                .map_err(|_| "artifact_read_failed")?;
            let mut hash = Sha256::new();
            let mut buffer = [0_u8; 128 * 1024];
            let mut count = 0_u64;
            loop {
                check()?;
                let read = file.read(&mut buffer).map_err(|_| "artifact_read_failed")?;
                if read == 0 {
                    break;
                }
                count += read as u64;
                if count > size {
                    return Err("artifact_changed".into());
                }
                hash.update(&buffer[..read]);
                after_chunk();
            }
            if count != size || format!("{:x}", hash.finalize()) != expected {
                return Err("artifact_digest_mismatch".into());
            }
            if std_file_identity(file).map_err(|_| "artifact_identity_unavailable")? != before {
                return Err("artifact_changed".into());
            }
            check()?;
            let mut entries = cache
                .lock()
                .map_err(|_| "artifact_verification_unavailable")?;
            if entries.len() >= 64 {
                entries.remove(0);
            }
            entries.push((before, expected.into()));
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|_| "artifact_read_failed")?;
        if std_file_identity(file).map_err(|_| "artifact_identity_unavailable")? != before {
            return Err("artifact_changed".into());
        }
        check()?;
        Ok(before)
    }

    // Static executables keep the held-FD trust boundary and do not introduce
    // a runtime plugin search path. CPU detection includes OS AVX state support.
    fn supports_avx2_engine() -> bool {
        #[cfg(target_arch = "x86_64")]
        {
            std::is_x86_feature_detected!("sse4.2")
                && std::is_x86_feature_detected!("avx")
                && std::is_x86_feature_detected!("avx2")
                && std::is_x86_feature_detected!("f16c")
                && std::is_x86_feature_detected!("fma")
                && std::is_x86_feature_detected!("bmi2")
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            false
        }
    }

    fn engine_dependencies(facts: &Value, platform: &str) -> Result<(), String> {
        let dependencies = facts["runtime_dependencies"]
            .as_array()
            .filter(|items| !items.is_empty() && items.len() <= 16)
            .ok_or("packaged_engine_dependencies_invalid")?;
        for dependency in dependencies {
            let dependency = dependency
                .as_str()
                .filter(|value| value.len() <= 512)
                .ok_or("packaged_engine_dependencies_invalid")?;
            let lower = dependency.to_ascii_lowercase();
            if [
                "not found",
                "libggml",
                "libwhisper",
                "libomp",
                "libgomp",
                "homebrew",
                "@rpath",
            ]
            .iter()
            .any(|bad| lower.contains(bad))
            {
                return Err("packaged_engine_dependencies_unqualified".into());
            }
            let qualified = if platform.starts_with("macos-") {
                dependency.trim().starts_with("/usr/lib/")
                    || dependency.trim().starts_with("/System/Library/")
            } else {
                let name = dependency.split_whitespace().next().unwrap_or("");
                [
                    "linux-vdso.so.1",
                    "libstdc++.so.6",
                    "libm.so.6",
                    "libgcc_s.so.1",
                    "libc.so.6",
                ]
                .contains(&name)
                    || name.starts_with("/lib/ld-linux-")
                    || name.starts_with("/lib64/ld-linux-")
            };
            if !qualified {
                return Err("packaged_engine_dependencies_unqualified".into());
            }
        }
        Ok(())
    }

    fn engine_cpu_variant<'a>(
        manifest: &'a Value,
        pin: &Value,
        platform: &str,
        avx2: bool,
    ) -> Result<Option<&'a Value>, String> {
        // Baseline-only packages remain valid across upgrades.
        let Some(variants) = manifest.get("cpu_variants") else {
            return Ok(None);
        };
        let variants = variants
            .as_object()
            .ok_or("packaged_engine_variants_invalid")?;
        if variants.is_empty() {
            return Ok(None);
        }
        if ![
            "linux-x86_64",
            "linux-amd64",
            "macos-x86_64",
            "macos-universal",
        ]
        .contains(&platform)
        {
            return Err("packaged_engine_variants_unqualified".into());
        }
        if variants.len() != 1 {
            return Err("packaged_engine_variants_invalid".into());
        }
        let fact = variants
            .get("x86_64-avx2")
            .ok_or("packaged_engine_variants_unqualified")?;
        if fact.as_object().is_none_or(|fields| fields.len() != 6) {
            return Err("packaged_engine_variants_invalid".into());
        }
        let expected = &pin["cpu_variants"]["x86_64-avx2"];
        for key in ["filename", "required_features", "build_options"] {
            if expected.get(key).is_none() || fact.get(key) != expected.get(key) {
                return Err("packaged_engine_variants_unqualified".into());
            }
        }
        if expected["filename"] != "whisper-cli-avx2" {
            return Err("packaged_engine_pin_invalid".into());
        }
        if !fact["binary_sha256"].as_str().is_some_and(valid_digest)
            || !fact["binary_bytes"]
                .as_u64()
                .is_some_and(|bytes| bytes > 0 && bytes <= MAX_ENGINE_BYTES)
        {
            return Err("packaged_engine_variants_invalid".into());
        }
        engine_dependencies(fact, platform)?;
        Ok(avx2.then_some(fact))
    }

    pub(super) async fn inspect_engine_at(path: PathBuf) -> Result<Engine, String> {
        inspect_engine_at_with_cpu(path, supports_avx2_engine()).await
    }

    async fn inspect_engine_at_with_cpu(path: PathBuf, avx2: bool) -> Result<Engine, String> {
        tokio::task::spawn_blocking(move || {
            if !path.is_absolute() {
                return Err("packaged_engine_path_not_absolute".into());
            }
            let parent = path.parent().ok_or("packaged_engine_unavailable")?;
            let mut manifest_file =
                open_read_nofollow_blocking(&parent.join("subtitle-engine.json"))
                    .map_err(|_| "packaged_engine_manifest_unavailable")?;
            let identity = std_file_identity(&manifest_file)
                .map_err(|_| "packaged_engine_manifest_unavailable")?;
            if !manifest_file
                .metadata()
                .map_err(|_| "packaged_engine_manifest_unavailable")?
                .is_file()
                || identity.size > 128 * 1024
            {
                return Err("packaged_engine_manifest_invalid".into());
            }
            let mut bytes = Vec::new();
            std::io::Read::by_ref(&mut manifest_file)
                .take(128 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| "packaged_engine_manifest_unavailable")?;
            if bytes.len() as u64 != identity.size
                || std_file_identity(&manifest_file)
                    .map_err(|_| "packaged_engine_manifest_unavailable")?
                    != identity
            {
                return Err("packaged_engine_manifest_changed".into());
            }
            let manifest: Value =
                serde_json::from_slice(&bytes).map_err(|_| "packaged_engine_manifest_invalid")?;
            let pin: Value =
                serde_json::from_str(PIN).map_err(|_| "packaged_engine_pin_invalid")?;
            for key in [
                "version",
                "adapter_contract",
                "source_commit",
                "source_archive_sha256",
                "build_options",
            ] {
                if manifest.get(key) != pin.get(key) {
                    return Err("packaged_engine_unqualified".into());
                }
            }
            let qualified_platform = match (std::env::consts::OS, std::env::consts::ARCH) {
                ("macos", "aarch64") => ["macos-arm64", "macos-universal"],
                ("macos", "x86_64") => ["macos-x86_64", "macos-universal"],
                ("linux", "aarch64") => ["linux-aarch64", "linux-arm64"],
                ("linux", "x86_64") => ["linux-x86_64", "linux-amd64"],
                _ => return Err("managed_engine_platform_unqualified".into()),
            };
            if !manifest["platform"]
                .as_str()
                .is_some_and(|platform| qualified_platform.contains(&platform))
            {
                return Err("packaged_engine_platform_mismatch".into());
            }
            let platform = manifest["platform"]
                .as_str()
                .ok_or("packaged_engine_platform_mismatch")?
                .to_owned();
            let companions = manifest["companions"]
                .as_object()
                .filter(|items| items.len() == COMPANIONS.len())
                .ok_or("packaged_engine_companions_invalid")?;
            let mut fixture = None;
            for (name, size, sha) in COMPANIONS {
                if companions.get(name).and_then(|fact| fact["bytes"].as_u64()) != Some(size)
                    || companions
                        .get(name)
                        .and_then(|fact| fact["sha256"].as_str())
                        != Some(sha)
                {
                    return Err("packaged_engine_companions_unqualified".into());
                }
                let mut file = open_read_nofollow_blocking(&parent.join(name))
                    .map_err(|_| "packaged_engine_companion_missing")?;
                let identity = hash_held(&mut file, sha, size)
                    .map_err(|_| "packaged_engine_companion_changed")?;
                if name == "self-test.wav" {
                    fixture = Some((file, identity, sha.to_owned()));
                }
            }
            engine_dependencies(&manifest, &platform)?;
            let variant = engine_cpu_variant(&manifest, &pin, &platform, avx2)?;
            let (facts, path) = match variant {
                Some(facts) => (facts, parent.join("whisper-cli-avx2")),
                None => (&manifest, path),
            };
            let sha = facts["binary_sha256"]
                .as_str()
                .filter(|value| valid_digest(value))
                .ok_or("packaged_engine_manifest_invalid")?
                .to_owned();
            let size = facts["binary_bytes"]
                .as_u64()
                .filter(|size| *size > 0 && *size <= MAX_ENGINE_BYTES)
                .ok_or("packaged_engine_manifest_invalid")?;
            let mut handle =
                open_read_nofollow_blocking(&path).map_err(|_| "packaged_engine_unavailable")?;
            use std::os::unix::fs::PermissionsExt;
            if handle
                .metadata()
                .map_err(|_| "packaged_engine_unavailable")?
                .permissions()
                .mode()
                & 0o111
                == 0
            {
                return Err("packaged_engine_not_executable".into());
            }
            let identity =
                hash_held(&mut handle, &sha, size).map_err(|_| "packaged_engine_changed")?;
            let (self_test_handle, self_test_identity, self_test_sha256) =
                fixture.ok_or("packaged_engine_companion_missing")?;
            Ok(Engine {
                handle,
                path,
                binary_sha256: sha,
                adapter_contract: pin["adapter_contract"]
                    .as_str()
                    .ok_or("packaged_engine_pin_invalid")?
                    .into(),
                platform,
                identity,
                self_test_handle,
                self_test_identity,
                self_test_sha256,
            })
        })
        .await
        .map_err(|_| "engine_verification_task_failed")?
    }

    pub(crate) async fn inspect_engine() -> Result<Engine, String> {
        let path = match std::env::var_os("PLURX_SUBTITLE_ENGINE") {
            Some(value) => PathBuf::from(value),
            None => std::env::current_exe()
                .map_err(|_| "packaged_engine_location_unavailable")?
                .parent()
                .ok_or("packaged_engine_location_unavailable")?
                .join("subtitle-engine/whisper-cli"),
        };
        inspect_engine_at(path).await
    }

    #[cfg(test)]
    async fn open_from_directory(
        root: &Path,
        directory: &SecureDirectory,
        spec: &CatalogModel,
    ) -> Result<ManagedModel, String> {
        open_from_directory_cancellable(root, directory, spec, &CancellationToken::new()).await
    }
    async fn open_from_directory_cancellable(
        root: &Path,
        directory: &SecureDirectory,
        spec: &CatalogModel,
        cancel: &CancellationToken,
    ) -> Result<ManagedModel, String> {
        if cancel.is_cancelled() {
            return Err("worker_interrupted".into());
        }
        let name = format!("{}.bin", spec.sha256);
        let mut handle = directory
            .open_read_child(&name)
            .await
            .map_err(|_| "model_not_installed")?
            .into_std()
            .await;
        let expected = spec.sha256.clone();
        let size = spec.download_bytes;
        let cancel = cancel.clone();
        let (handle, identity) = tokio::task::spawn_blocking(move || {
            check_model_owner(&handle)?;
            let identity = hash_held_cancellable(&mut handle, &expected, size, &cancel)?;
            Ok::<_, String>((handle, identity))
        })
        .await
        .map_err(|_| "model_verification_task_failed")??;
        Ok(ManagedModel {
            handle,
            path: root.join(name),
            sha256: spec.sha256.clone(),
            identity,
        })
    }

    fn check_model_owner(handle: &File) -> Result<(), String> {
        use std::os::unix::fs::MetadataExt;
        let metadata = handle
            .metadata()
            .map_err(|_| "model_metadata_unavailable")?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
            return Err("model_artifact_not_service_owned".into());
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) async fn open_model(
        root: &Path,
        spec: &CatalogModel,
    ) -> Result<ManagedModel, String> {
        open_model_cancellable(root, spec, &CancellationToken::new()).await
    }
    /// The owned worker signals cancellation and awaits this future through its
    /// blocking join; each bounded read observes the exact durable loss token.
    pub(crate) async fn open_model_cancellable(
        root: &Path,
        spec: &CatalogModel,
        cancel: &CancellationToken,
    ) -> Result<ManagedModel, String> {
        if cancel.is_cancelled() {
            return Err("worker_interrupted".into());
        }
        validate_spec(spec)?;
        if !root.is_absolute() {
            return Err("model_store_path_not_absolute".into());
        }
        let directory = SecureDirectory::open(root)
            .await
            .map_err(|_| "model_store_unavailable")?;
        check_directory_owner(&directory)?;
        open_from_directory_cancellable(root, &directory, spec, cancel).await
    }

    /// Cheap observation only. A matching current-boot self-test receipt may
    /// qualify this identity; presence/metadata alone never verifies bytes.
    pub(crate) async fn model_identity(
        root: &Path,
        spec: &CatalogModel,
    ) -> Result<FileIdentity, String> {
        validate_spec(spec)?;
        if !root.is_absolute() {
            return Err("model_store_path_not_absolute".into());
        }
        let directory = SecureDirectory::open(root)
            .await
            .map_err(|_| "model_store_unavailable")?;
        check_directory_owner(&directory)?;
        let handle = directory
            .open_read_child(&format!("{}.bin", spec.sha256))
            .await
            .map_err(|_| "model_not_installed")?
            .into_std()
            .await;
        check_model_owner(&handle)?;
        let metadata = handle
            .metadata()
            .map_err(|_| "model_metadata_unavailable")?;
        if !metadata.is_file() {
            return Err("model_artifact_not_regular".into());
        }
        if metadata.len() != spec.download_bytes {
            return Err("artifact_length_mismatch".into());
        }
        std_file_identity(&handle).map_err(|_| "model_metadata_unavailable".into())
    }

    async fn model_directory(root: &Path) -> Result<SecureDirectory, String> {
        if !root.is_absolute() {
            return Err("model_store_path_not_absolute".into());
        }
        let parent = SecureDirectory::open(root.parent().ok_or("model_store_invalid")?)
            .await
            .map_err(|_| "model_store_parent_unavailable")?;
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("model_store_invalid")?;
        let directory = parent
            .create_child_directory(name)
            .await
            .map_err(|_| "model_store_unsafe_or_unavailable")?;
        check_directory_owner(&directory)?;
        Ok(directory)
    }

    fn check_directory_owner(directory: &SecureDirectory) -> Result<(), String> {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the directory capability holds a live fd, and fstat writes
        // exactly one stat value on success.
        if unsafe { libc::fstat(directory.raw_fd(), stat.as_mut_ptr()) } != 0 {
            return Err("model_store_unavailable".into());
        }
        let stat = unsafe { stat.assume_init() };
        if stat.st_uid != unsafe { libc::geteuid() } || stat.st_mode & 0o022 != 0 {
            return Err("model_store_not_service_owned".into());
        }
        Ok(())
    }

    struct Temporary {
        directory: SecureDirectory,
        name: CString,
        handle: File,
    }

    impl Drop for Temporary {
        fn drop(&mut self) {
            // SAFETY: generated single-component name; the held directory
            // remains authority even if its pathname is renamed/replaced.
            unsafe {
                libc::unlinkat(self.directory.raw_fd(), self.name.as_ptr(), 0);
            }
        }
    }

    fn allocate(file: &File, size: u64) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        let result = {
            // SAFETY: live regular file, checked <=2GiB size fits off_t.
            let status = unsafe { libc::posix_fallocate(file.as_raw_fd(), 0, size as libc::off_t) };
            if status == 0 {
                Ok(())
            } else {
                Err(std::io::Error::from_raw_os_error(status))
            }
        };
        #[cfg(target_os = "macos")]
        let result = {
            let mut request = libc::fstore_t {
                fst_flags: libc::F_ALLOCATEALL,
                fst_posmode: libc::F_PEOFPOSMODE,
                fst_offset: 0,
                fst_length: size as libc::off_t,
                fst_bytesalloc: 0,
            };
            // SAFETY: F_PREALLOCATE consumes this initialized fstore_t on the
            // held file. F_ALLOCATEALL forbids accepting partial reservation.
            if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PREALLOCATE, &mut request) } == 0
                && request.fst_bytesalloc >= size as libc::off_t
            {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        };
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let result: std::io::Result<()> =
            Err(std::io::Error::other("unsupported reservation target"));
        result.map_err(|error| {
            if error.raw_os_error() == Some(libc::ENOSPC) {
                "model_disk_full"
            } else {
                "model_disk_reservation_unavailable"
            }
        })?;
        file.set_len(size)
            .map_err(|_| "model_disk_reservation_failed".into())
    }

    async fn temporary(directory: SecureDirectory, size: u64) -> Result<Temporary, String> {
        tokio::task::spawn_blocking(move || {
            let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
            // SAFETY: valid held directory and output storage for statvfs.
            if unsafe { libc::fstatvfs(directory.raw_fd(), stats.as_mut_ptr()) } != 0 {
                return Err("model_disk_space_unavailable".into());
            }
            let stats = unsafe { stats.assume_init() };
            #[allow(clippy::unnecessary_cast)]
            let free = (stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64);
            if free < size.saturating_add(DISK_MARGIN) {
                return Err("model_disk_full".into());
            }
            let name = CString::new(format!(".{}.part", uuid::Uuid::new_v4().simple()))
                .map_err(|_| "model_temporary_unavailable")?;
            // SAFETY: generated name, descriptor-relative exclusive creation
            // refuses symlinks and gives only this service read/write access.
            let raw = unsafe {
                libc::openat(
                    directory.raw_fd(),
                    name.as_ptr(),
                    libc::O_RDWR
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if raw < 0 {
                return Err("model_temporary_unavailable".into());
            }
            let temporary = Temporary {
                directory,
                name,
                handle: unsafe { File::from_raw_fd(raw) },
            };
            allocate(&temporary.handle, size)?;
            Ok(temporary)
        })
        .await
        .map_err(|_| "model_reservation_task_failed")?
    }

    fn approved_url(url: &reqwest::Url) -> bool {
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port_or_known_default() == Some(443)
            && url.fragment().is_none()
            && matches!(
                url.host_str(),
                Some("huggingface.co" | "us.aws.cdn.hf.co" | "cas-bridge.xethub.hf.co")
            )
    }

    fn reviewed_initial_url(spec: &CatalogModel, url: &reqwest::Url) -> bool {
        approved_url(url)
            && url.host_str() == Some("huggingface.co")
            && url.query().is_none()
            && url.as_str() == spec.url
            && (url.path()
                == format!(
                    "/ggerganov/whisper.cpp/resolve/{REVISION}/ggml-{}.bin",
                    spec.id
                )
                || crate::subtitle_text_translation::artifacts::registered_file(spec))
    }

    async fn response(
        spec: &CatalogModel,
        cancel: &CancellationToken,
    ) -> Result<reqwest::Response, String> {
        let mut url = reqwest::Url::parse(&spec.url).map_err(|_| "invalid_model_catalog_url")?;
        if !reviewed_initial_url(spec, &url) {
            return Err("invalid_model_catalog_url".into());
        }
        let client = reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .user_agent("plurx-subtitle-models/1")
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20 * 60))
            .build()
            .map_err(|_| "model_download_client_unavailable")?;
        for redirects in 0..=3 {
            cancelled(cancel)?;
            let result = tokio::select! {
                _ = cancel.cancelled() => return Err("model_install_cancelled".into()),
                result = client.get(url.clone()).header(reqwest::header::ACCEPT_ENCODING, "identity").send() => result,
            }.map_err(|_| "model_download_failed")?;
            if result.status().is_redirection() {
                if redirects == 3 {
                    return Err("model_redirect_limit".into());
                }
                let location = result
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or("model_redirect_invalid")?;
                url = url.join(location).map_err(|_| "model_redirect_invalid")?;
                if !approved_url(&url) {
                    return Err("model_redirect_refused".into());
                }
                continue;
            }
            if result.status() != reqwest::StatusCode::OK {
                return Err("model_download_http_failure".into());
            }
            if result
                .content_length()
                .is_some_and(|bytes| bytes != spec.download_bytes)
                || result
                    .headers()
                    .get(reqwest::header::CONTENT_ENCODING)
                    .is_some_and(|value| value != "identity")
            {
                return Err("model_download_length_or_encoding_refused".into());
            }
            return Ok(result);
        }
        Err("model_redirect_limit".into())
    }

    async fn publish(
        root: &Path,
        temporary: Temporary,
        spec: &CatalogModel,
        cancel: &CancellationToken,
    ) -> Result<ManagedModel, String> {
        cancelled(cancel)?;
        root_matches(root, &temporary.directory).await?;
        let name = temporary
            .name
            .to_str()
            .map_err(|_| "model_temporary_unavailable")?;
        let mut verify = temporary
            .handle
            .try_clone()
            .map_err(|_| "model_verification_failed")?;
        let sha = spec.sha256.clone();
        let bytes = spec.download_bytes;
        let hash_cancel = cancel.clone();
        tokio::task::spawn_blocking(move || {
            hash_held_cancellable(&mut verify, &sha, bytes, &hash_cancel)?;
            verify.sync_all().map_err(|_| "model_sync_failed")?;
            use std::os::unix::fs::PermissionsExt;
            verify
                .set_permissions(std::fs::Permissions::from_mode(0o400))
                .map_err(|_| "model_permissions_failed")?;
            Ok::<(), String>(())
        })
        .await
        .map_err(|_| "model_verification_task_failed")??;
        cancelled(cancel)?;
        temporary
            .directory
            .rename_child_noreplace(name, &format!("{}.bin", spec.sha256))
            .await
            .map_err(|_| "model_publication_failed")?;
        let model =
            open_from_directory_cancellable(root, &temporary.directory, spec, cancel).await?;
        root_matches(root, &temporary.directory).await?;
        cancelled(cancel)?;
        Ok(model)
    }

    async fn root_matches(root: &Path, held: &SecureDirectory) -> Result<(), String> {
        let current = SecureDirectory::open(root)
            .await
            .map_err(|_| "model_store_replaced")?;
        let current = current
            .identity()
            .await
            .map_err(|_| "model_store_replaced")?;
        let held = held.identity().await.map_err(|_| "model_store_replaced")?;
        if current.same_inode(held) {
            Ok(())
        } else {
            Err("model_store_replaced".into())
        }
    }

    async fn retire_interrupted(directory: &SecureDirectory) -> Result<(), String> {
        // The process slot and held directory flock exclude live installers,
        // including a blocking allocator whose async owner was interrupted.
        // Inference holds digest artifacts, never exclusive temporary names.
        for name in directory
            .child_names(256)
            .await
            .map_err(|_| "model_store_cleanup_unavailable")?
        {
            let Some(id) = name
                .strip_prefix('.')
                .and_then(|name| name.strip_suffix(".part"))
            else {
                continue;
            };
            if id.len() != 32
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                continue;
            }
            if !directory
                .child_metadata(&name)
                .await
                .map_err(|_| "model_store_cleanup_unavailable")?
                .is_file
            {
                return Err("model_store_cleanup_unsafe".into());
            }
            directory
                .unlink_child(&name)
                .await
                .map_err(|_| "model_store_cleanup_unavailable")?;
        }
        Ok(())
    }

    fn lock_directory(directory: &SecureDirectory) -> Result<(), String> {
        // SAFETY: a held directory descriptor. The lock belongs to this open
        // file description and remains live in cloned SecureDirectory handles
        // until every installer/allocator owner has actually retired.
        if unsafe { libc::flock(directory.raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("model_install_store_busy_or_unavailable".into());
        }
        Ok(())
    }

    async fn existing_or_retire_corrupt(
        root: &Path,
        directory: &SecureDirectory,
        spec: &CatalogModel,
        cancel: &CancellationToken,
    ) -> Result<Option<ManagedModel>, String> {
        cancelled(cancel)?;
        match open_from_directory_cancellable(root, directory, spec, cancel).await {
            Ok(model) => return Ok(Some(model)),
            Err(error) if error == "model_not_installed" => {
                return match directory
                    .child_metadata(&format!("{}.bin", spec.sha256))
                    .await
                {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    _ => Err("model_existing_artifact_unsafe_or_unavailable".into()),
                };
            }
            Err(error)
                if error == "artifact_length_mismatch" || error == "artifact_digest_mismatch" => {}
            Err(error) => return Err(error),
        }
        let name = format!("{}.bin", spec.sha256);
        let mut held = directory
            .open_read_child(&name)
            .await
            .map_err(|_| "model_repair_identity_changed")?
            .into_std()
            .await;
        let before = std_file_identity(&held).map_err(|_| "model_repair_identity_changed")?;
        let expected = spec.sha256.clone();
        let size = spec.download_bytes;
        let hash_cancel = cancel.clone();
        let checked = tokio::task::spawn_blocking(move || {
            let result = check_model_owner(&held)
                .and_then(|_| hash_held_cancellable(&mut held, &expected, size, &hash_cancel));
            (held, result)
        })
        .await
        .map_err(|_| "model_verification_task_failed")?;
        let (_held, result) = checked;
        match result {
            Ok(_) => {
                return open_from_directory_cancellable(root, directory, spec, cancel)
                    .await
                    .map(Some)
            }
            Err(error)
                if error == "artifact_length_mismatch" || error == "artifact_digest_mismatch" => {}
            Err(error) => return Err(error),
        }
        if directory
            .child_metadata(&name)
            .await
            .map_err(|_| "model_repair_identity_changed")?
            .identity
            != before
        {
            return Err("model_repair_identity_changed".into());
        }
        cancelled(cancel)?;
        let retired = format!(".{}.corrupt.{}", spec.sha256, uuid::Uuid::new_v4().simple());
        if !directory
            .rename_child_noreplace(&name, &retired)
            .await
            .map_err(|_| "model_repair_retirement_failed")?
        {
            return Err("model_repair_retirement_failed".into());
        }
        let moved = directory
            .child_metadata(&retired)
            .await
            .map_err(|_| "model_repair_identity_changed")?;
        if !moved.identity.same_inode(before) {
            let _ = directory.rename_child_noreplace(&retired, &name).await;
            return Err("model_repair_identity_changed".into());
        }
        // Retire only the proved corrupt inode. Accepted/running attempts keep
        // their already-held handles and detect its changed identity; another
        // digest's valid bytes and a concurrently valid replacement are kept.
        directory
            .unlink_child(&retired)
            .await
            .map_err(|_| "model_repair_retirement_failed")?;
        Ok(None)
    }

    async fn write_chunk(
        output: &mut tokio::fs::File,
        received: &mut u64,
        limit: u64,
        bytes: &[u8],
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        cancelled(cancel)?;
        let next = received
            .checked_add(bytes.len() as u64)
            .ok_or("model_download_oversized")?;
        if next > limit {
            return Err("model_download_oversized".into());
        }
        // Join each bounded filesystem write before observing cancellation;
        // dropping a selected-away tokio File write can leave work in flight.
        output
            .write_all(bytes)
            .await
            .map_err(|_| "model_download_write_failed")?;
        output
            .flush()
            .await
            .map_err(|_| "model_download_write_failed")?;
        *received = next;
        Ok(())
    }

    async fn finish_download(
        root: &Path,
        temporary: Temporary,
        spec: &CatalogModel,
        received: u64,
        cancel: &CancellationToken,
    ) -> Result<ManagedModel, String> {
        if received != spec.download_bytes {
            return Err("model_download_truncated".into());
        }
        publish(root, temporary, spec, cancel).await
    }

    pub(crate) async fn install_model(
        root: &Path,
        spec: &CatalogModel,
        cancel: &CancellationToken,
    ) -> Result<ManagedModel, String> {
        validate_spec(spec)?;
        let _guard = tokio::select! { _ = cancel.cancelled() => return Err("model_install_cancelled".into()), guard = INSTALL.lock() => guard };
        cancelled(cancel)?;
        let directory = model_directory(root).await?;
        lock_directory(&directory)?;
        retire_interrupted(&directory).await?;
        if let Some(model) = existing_or_retire_corrupt(root, &directory, spec, cancel).await? {
            cancelled(cancel)?;
            return Ok(model);
        }
        let temporary = temporary(directory, spec.download_bytes).await?;
        cancelled(cancel)?;
        let mut response = response(spec, cancel).await?;
        let mut output = tokio::fs::File::from_std(
            temporary
                .handle
                .try_clone()
                .map_err(|_| "model_temporary_unavailable")?,
        );
        let mut received = 0_u64;
        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => return Err("model_install_cancelled".into()),
                chunk = response.chunk() => chunk,
            }
            .map_err(|_| "model_download_interrupted")?;
            let Some(chunk) = chunk else {
                break;
            };
            write_chunk(
                &mut output,
                &mut received,
                spec.download_bytes,
                &chunk,
                cancel,
            )
            .await?;
        }
        output
            .flush()
            .await
            .map_err(|_| "model_download_write_failed")?;
        drop(output);
        finish_download(root, temporary, spec, received, cancel).await
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::unix::fs::symlink;

        fn spec(bytes: &[u8]) -> CatalogModel {
            CatalogModel { id: "base.en".into(), label: "test".into(), languages: vec!["en".into()], download_bytes: bytes.len() as u64,
                sha256: format!("{:x}", Sha256::digest(bytes)), url: format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/{REVISION}/ggml-base.en.bin"), source: "test".into(), license: "MIT".into() }
        }

        fn engine_companions(root: &Path, manifest: &mut Value) {
            manifest["companions"] = serde_json::json!({});
            for (name, size, sha) in COMPANIONS {
                let bytes: &[u8] = match name {
                    "self-test.wav" => include_bytes!("self-test.wav"),
                    "whisper.cpp.LICENSE.txt" => {
                        include_bytes!("../../../../licenses/whisper.cpp.LICENSE.txt")
                    }
                    "whisper-models.LICENSE.txt" => {
                        include_bytes!("../../../../licenses/whisper-models.LICENSE.txt")
                    }
                    _ => unreachable!(),
                };
                assert_eq!(bytes.len() as u64, size);
                assert_eq!(format!("{:x}", Sha256::digest(bytes)), sha);
                std::fs::write(root.join(name), bytes).expect("companion");
                manifest["companions"][name] = serde_json::json!({"bytes":size,"sha256":sha});
            }
            manifest["runtime_dependencies"] = if cfg!(target_os = "macos") {
                serde_json::json!(["/usr/lib/libSystem.B.dylib", "/usr/lib/libc++.1.dylib"])
            } else {
                serde_json::json!(["linux-vdso.so.1", "libc.so.6 => /lib/libc.so.6"])
            };
        }

        #[test]
        fn model_downloads_accept_only_reviewed_family_catalogs() {
            let catalog: serde_json::Value =
                serde_json::from_str(include_str!("../subtitle_text_translation/catalog.json"))
                    .expect("pinned text catalog");
            for entry in catalog["files"].as_array().expect("pinned parts") {
                let mut entry = entry.clone();
                entry.as_object_mut().expect("part object").remove("role");
                let spec: CatalogModel =
                    serde_json::from_value(entry).expect("pinned artifact identity");
                let url = reqwest::Url::parse(&spec.url).expect("pinned artifact URL");
                assert!(reviewed_initial_url(&spec, &url), "{}", spec.id);
                let mut changed = spec.clone();
                changed.sha256 = "0".repeat(64);
                assert!(!reviewed_initial_url(&changed, &url));
                changed = spec.clone();
                changed.download_bytes += 1;
                assert!(!reviewed_initial_url(&changed, &url));
                changed = spec.clone();
                changed.id.push_str("-other");
                assert!(!reviewed_initial_url(&changed, &url));
                changed = spec.clone();
                changed.url =
                    "https://huggingface.co/unreviewed/model/resolve/main/weights.bin".into();
                assert!(!reviewed_initial_url(
                    &changed,
                    &reqwest::Url::parse(&changed.url).expect("other URL")
                ));
                assert!(!reviewed_initial_url(
                    &spec,
                    &reqwest::Url::parse("https://huggingface.co/other").expect("different URL")
                ));
            }
            let legacy = crate::subtitle_translation::profile::model_spec();
            assert!(reviewed_initial_url(
                &legacy,
                &reqwest::Url::parse(&legacy.url).expect("speech URL")
            ));
        }
        #[tokio::test]
        async fn model_root_and_artifacts_refuse_symlinks_and_world_writes() {
            let temporary = tempfile::tempdir().expect("test root");
            let root = temporary.path().canonicalize().expect("canonical root");
            let real = root.join("real");
            std::fs::create_dir(&real).expect("real directory");
            symlink(&real, root.join("alias")).expect("alias");
            assert!(model_directory(&root.join("alias")).await.is_err());
            let model_root = root.join("models");
            let directory = model_directory(&model_root)
                .await
                .expect("owned model directory");
            let model = spec(b"model bytes");
            std::fs::write(root.join("external"), b"model bytes").expect("external");
            symlink(
                root.join("external"),
                model_root.join(format!("{}.bin", model.sha256)),
            )
            .expect("model link");
            assert!(open_from_directory(&model_root, &directory, &model)
                .await
                .is_err());
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&model_root, std::fs::Permissions::from_mode(0o777))
                .expect("world writable");
            assert!(model_directory(&model_root).await.is_err());
            assert_eq!(
                open_model(&model_root, &model).await.err().as_deref(),
                Some("model_store_not_service_owned")
            );
        }

        #[tokio::test]
        async fn cancelled_or_wrong_hash_never_publishes_and_temporary_is_retired() {
            let temporary_root = tempfile::tempdir().expect("root");
            let root = temporary_root
                .path()
                .canonicalize()
                .expect("canonical")
                .join("models");
            let directory = model_directory(&root).await.expect("directory");
            let model = spec(b"model bytes");
            let staged = temporary(directory.clone(), model.download_bytes)
                .await
                .expect("reserved temp");
            let cancel = CancellationToken::new();
            assert!(publish(&root, staged, &model, &cancel).await.is_err());
            assert!(directory
                .child_names(10)
                .await
                .expect("children")
                .is_empty());
            let staged = temporary(directory.clone(), model.download_bytes)
                .await
                .expect("reserved temp");
            cancel.cancel();
            assert!(publish(&root, staged, &model, &cancel).await.is_err());
            assert!(directory
                .child_names(10)
                .await
                .expect("children")
                .is_empty());
        }

        #[tokio::test]
        async fn interrupted_and_oversized_downloads_never_publish_reserved_zero_fill() {
            let temporary_root = tempfile::tempdir().expect("root");
            let root = temporary_root
                .path()
                .canonicalize()
                .expect("canonical")
                .join("models");
            let directory = model_directory(&root).await.expect("directory");
            let model = spec(b"model bytes");
            let cancel = CancellationToken::new();
            let staged = temporary(directory.clone(), model.download_bytes)
                .await
                .expect("reserved");
            assert!(finish_download(&root, staged, &model, 3, &cancel)
                .await
                .is_err());
            assert!(directory
                .child_names(10)
                .await
                .expect("children")
                .is_empty());
            let staged = temporary(directory.clone(), model.download_bytes)
                .await
                .expect("reserved");
            let mut output = tokio::fs::File::from_std(staged.handle.try_clone().expect("writer"));
            let mut received = 0;
            assert!(write_chunk(
                &mut output,
                &mut received,
                model.download_bytes,
                b"larger than model bytes",
                &cancel
            )
            .await
            .is_err());
            assert_eq!(received, 0);
            drop(output);
            drop(staged);
            assert!(directory
                .child_names(10)
                .await
                .expect("children")
                .is_empty());
        }

        #[tokio::test]
        async fn successful_install_is_content_addressed_and_does_not_overwrite_prior_artifact() {
            let temporary_root = tempfile::tempdir().expect("root");
            let root = temporary_root
                .path()
                .canonicalize()
                .expect("canonical")
                .join("models");
            let directory = model_directory(&root).await.expect("directory");
            let bytes = b"model bytes";
            let model = spec(bytes);
            let staged = temporary(directory.clone(), model.download_bytes)
                .await
                .expect("reserved");
            let mut output = tokio::fs::File::from_std(staged.handle.try_clone().expect("writer"));
            let cancel = CancellationToken::new();
            let mut received = 0;
            write_chunk(
                &mut output,
                &mut received,
                model.download_bytes,
                bytes,
                &cancel,
            )
            .await
            .expect("write");
            output.flush().await.expect("flush");
            drop(output);
            let installed = finish_download(&root, staged, &model, received, &cancel)
                .await
                .expect("installed");
            assert_eq!(installed.path, root.join(format!("{}.bin", model.sha256)));
            assert_eq!(std::fs::read(&installed.path).expect("bytes"), bytes);
            let opened = open_model(&root, &model).await.expect("verified on reopen");
            assert_eq!(opened.identity, installed.identity);
            let cancelled = CancellationToken::new();
            cancelled.cancel();
            assert!(install_model(&root, &model, &cancelled).await.is_err());
            assert_eq!(std::fs::read(&installed.path).expect("prior bytes"), bytes);
        }

        fn variant_fact(pin: &Value, bytes: &[u8]) -> Value {
            let mut fact = pin["cpu_variants"]["x86_64-avx2"].clone();
            fact["binary_bytes"] = bytes.len().into();
            fact["binary_sha256"] = format!("{:x}", Sha256::digest(bytes)).into();
            fact["runtime_dependencies"] = serde_json::json!(["libc.so.6"]);
            fact
        }

        #[test]
        fn engine_cpu_variant_requires_exact_static_recipe_and_full_host_support() {
            let pin: Value = serde_json::from_str(PIN).expect("pin");
            let fact = variant_fact(&pin, b"optimized");
            assert!(engine_cpu_variant(
                &serde_json::json!({"cpu_variants":{}}),
                &pin,
                "linux-aarch64",
                false
            )
            .expect("empty optional inventory")
            .is_none());
            assert!(engine_cpu_variant(
                &serde_json::json!({"cpu_variants":[]}),
                &pin,
                "linux-x86_64",
                false
            )
            .is_err());
            let mut manifest = serde_json::json!({"cpu_variants":{"x86_64-avx2":fact}});
            assert!(engine_cpu_variant(&manifest, &pin, "linux-x86_64", false)
                .expect("baseline")
                .is_none());
            assert_eq!(
                engine_cpu_variant(&manifest, &pin, "linux-x86_64", true).expect("optimized"),
                Some(&manifest["cpu_variants"]["x86_64-avx2"])
            );
            let mut universal = manifest.clone();
            universal["cpu_variants"]["x86_64-avx2"]["runtime_dependencies"] =
                serde_json::json!(["/usr/lib/libSystem.B.dylib"]);
            assert!(
                engine_cpu_variant(&universal, &pin, "macos-universal", false)
                    .expect("universal ARM")
                    .is_none()
            );
            assert!(engine_cpu_variant(&manifest, &pin, "linux-aarch64", true).is_err());
            assert!(
                engine_cpu_variant(&serde_json::json!({}), &pin, "linux-x86_64", true)
                    .expect("old package")
                    .is_none()
            );
            for (key, value) in [
                ("filename", serde_json::json!("../external")),
                ("required_features", serde_json::json!(["avx2"])),
                ("build_options", serde_json::json!({"GGML_NATIVE":"ON"})),
                ("binary_sha256", serde_json::json!("not-a-digest")),
                ("binary_bytes", serde_json::json!(0)),
                ("runtime_dependencies", serde_json::json!(["libggml.so"])),
            ] {
                let mut changed = manifest.clone();
                changed["cpu_variants"]["x86_64-avx2"][key] = value;
                assert!(
                    engine_cpu_variant(&changed, &pin, "linux-x86_64", true).is_err(),
                    "{key}"
                );
                assert!(
                    engine_cpu_variant(&changed, &pin, "linux-x86_64", false).is_err(),
                    "unsupported hardware still checks {key}"
                );
            }
            manifest["cpu_variants"]["unknown"] = serde_json::json!({});
            assert!(engine_cpu_variant(&manifest, &pin, "linux-x86_64", true).is_err());
        }

        #[cfg(target_arch = "x86_64")]
        #[tokio::test]
        async fn engine_cpu_variant_holds_selected_bytes_and_refuses_digest_or_symlink_replacement()
        {
            use std::os::unix::fs::PermissionsExt;
            let temporary_root = tempfile::tempdir().expect("root");
            let root = temporary_root.path().canonicalize().expect("canonical");
            let baseline = root.join("whisper-cli");
            let variant = root.join("whisper-cli-avx2");
            let baseline_bytes = b"baseline executable";
            let variant_bytes = b"optimized executable";
            for (path, bytes) in [
                (&baseline, baseline_bytes.as_slice()),
                (&variant, variant_bytes.as_slice()),
            ] {
                std::fs::write(path, bytes).expect("engine bytes");
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                    .expect("executable");
            }
            let pin: Value = serde_json::from_str(PIN).expect("pin");
            let mut manifest = pin.clone();
            manifest["binary_bytes"] = baseline_bytes.len().into();
            manifest["binary_sha256"] = format!("{:x}", Sha256::digest(baseline_bytes)).into();
            manifest["platform"] = if cfg!(target_os = "macos") {
                "macos-x86_64"
            } else {
                "linux-x86_64"
            }
            .into();
            engine_companions(&root, &mut manifest);
            let mut fact = variant_fact(&pin, variant_bytes);
            fact["runtime_dependencies"] = manifest["runtime_dependencies"].clone();
            manifest["cpu_variants"]["x86_64-avx2"] = fact;
            std::fs::write(
                root.join("subtitle-engine.json"),
                serde_json::to_vec(&manifest).expect("JSON"),
            )
            .expect("manifest");
            let baseline_engine = inspect_engine_at_with_cpu(baseline.clone(), false)
                .await
                .expect("legacy CPU");
            assert_eq!(baseline_engine.path, baseline);
            let mut selected = inspect_engine_at_with_cpu(baseline.clone(), true)
                .await
                .expect("AVX2 CPU");
            assert_eq!(selected.path, variant);
            assert_eq!(
                Some(selected.binary_sha256.as_str()),
                manifest["cpu_variants"]["x86_64-avx2"]["binary_sha256"].as_str()
            );
            let moved = root.join("held-avx2");
            std::fs::rename(&variant, &moved).expect("move original");
            std::fs::write(&variant, b"unqualified changed executable").expect("replace");
            std::fs::set_permissions(&variant, std::fs::Permissions::from_mode(0o700))
                .expect("mode");
            assert_eq!(
                inspect_engine_at_with_cpu(baseline.clone(), true)
                    .await
                    .err()
                    .as_deref(),
                Some("packaged_engine_changed")
            );
            let mut held = Vec::new();
            selected.handle.read_to_end(&mut held).expect("held bytes");
            assert_eq!(held, variant_bytes);
            std::fs::remove_file(&variant).expect("remove replacement");
            symlink(&moved, &variant).expect("substituted symlink");
            assert_eq!(
                inspect_engine_at_with_cpu(baseline.clone(), true)
                    .await
                    .err()
                    .as_deref(),
                Some("packaged_engine_unavailable")
            );
            assert!(
                inspect_engine_at_with_cpu(baseline, false).await.is_ok(),
                "unsupported host never opens optimized inode"
            );
        }

        #[tokio::test]
        async fn engine_manifest_digest_options_and_symlink_are_enforced() {
            use std::os::unix::fs::PermissionsExt;
            let temporary_root = tempfile::tempdir().expect("root");
            let root = temporary_root.path().canonicalize().expect("canonical");
            let path = root.join("whisper-cli");
            let bytes = b"synthetic executable bytes";
            std::fs::write(&path, bytes).expect("engine");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .expect("executable");
            let mut manifest: Value = serde_json::from_str(PIN).expect("pin");
            manifest
                .as_object_mut()
                .expect("object")
                .remove("cpu_variants");
            manifest["binary_bytes"] = bytes.len().into();
            manifest["binary_sha256"] = format!("{:x}", Sha256::digest(bytes)).into();
            manifest["platform"] = match (std::env::consts::OS, std::env::consts::ARCH) {
                ("macos", "aarch64") => "macos-arm64",
                ("macos", "x86_64") => "macos-x86_64",
                ("linux", "aarch64") => "linux-aarch64",
                ("linux", "x86_64") => "linux-x86_64",
                _ => panic!("test requires qualified host"),
            }
            .into();
            engine_companions(&root, &mut manifest);
            let manifest_path = root.join("subtitle-engine.json");
            std::fs::write(&manifest_path, serde_json::to_vec(&manifest).expect("JSON"))
                .expect("manifest");
            let engine = inspect_engine_at(path.clone())
                .await
                .expect("qualified local artifact");
            assert_eq!(
                std_file_identity(&engine.self_test_handle).expect("held fixture"),
                engine.self_test_identity
            );
            assert_eq!(engine.self_test_sha256, COMPANIONS[0].2);
            let saved_fixture = root.join("saved-fixture.wav");
            std::fs::rename(root.join("self-test.wav"), &saved_fixture).expect("move fixture");
            std::fs::write(root.join("self-test.wav"), b"unqualified replacement")
                .expect("replacement fixture");
            let mut held_fixture = engine.self_test_handle;
            let mut held_bytes = Vec::new();
            held_fixture
                .read_to_end(&mut held_bytes)
                .expect("held fixture bytes");
            assert_eq!(
                format!("{:x}", Sha256::digest(&held_bytes)),
                COMPANIONS[0].2
            );
            assert_eq!(
                inspect_engine_at(path.clone()).await.err().as_deref(),
                Some("packaged_engine_companion_changed")
            );
            std::fs::remove_file(root.join("self-test.wav")).expect("remove replacement");
            std::fs::rename(saved_fixture, root.join("self-test.wav")).expect("restore fixture");
            for (name, _, _) in COMPANIONS {
                let original = std::fs::read(root.join(name)).expect("companion bytes");
                let mut altered = original.clone();
                altered[0] ^= 1;
                std::fs::write(root.join(name), altered).expect("changed companion");
                assert_eq!(
                    inspect_engine_at(path.clone()).await.err().as_deref(),
                    Some("packaged_engine_companion_changed")
                );
                std::fs::write(root.join(name), original).expect("restored companion");
            }
            manifest["build_options"]["GGML_NATIVE"] = "ON".into();
            std::fs::write(&manifest_path, serde_json::to_vec(&manifest).expect("JSON"))
                .expect("changed manifest");
            assert!(inspect_engine_at(path.clone()).await.is_err());
            manifest["build_options"]["GGML_NATIVE"] = "OFF".into();
            std::fs::write(&manifest_path, serde_json::to_vec(&manifest).expect("JSON"))
                .expect("restored manifest");
            let mut altered = bytes.to_vec();
            altered[0] ^= 1;
            std::fs::write(&path, altered).expect("changed engine");
            assert_eq!(
                inspect_engine_at(path.clone()).await.err().as_deref(),
                Some("packaged_engine_changed")
            );
            std::fs::write(&path, bytes).expect("restored engine");
            std::fs::rename(&path, root.join("other-engine")).expect("move");
            symlink(root.join("other-engine"), &path).expect("link");
            assert_eq!(
                inspect_engine_at(path).await.err().as_deref(),
                Some("packaged_engine_unavailable")
            );
        }

        #[tokio::test]
        async fn reinstall_retires_only_owned_corrupt_inode_and_preserves_held_handles() {
            use std::os::unix::fs::PermissionsExt;
            let temporary_root = tempfile::tempdir().expect("root");
            let root = temporary_root
                .path()
                .canonicalize()
                .expect("canonical")
                .join("models");
            let directory = model_directory(&root).await.expect("directory");
            lock_directory(&directory).expect("exclusive installation");
            let model = spec(b"model bytes");
            let name = root.join(format!("{}.bin", model.sha256));
            std::fs::write(&name, b"model bytes").expect("existing model");
            let cancel = CancellationToken::new();
            let verified = existing_or_retire_corrupt(&root, &directory, &model, &cancel)
                .await
                .expect("existing")
                .expect("valid bytes kept");
            let old_identity = verified.identity;
            std::fs::set_permissions(&name, std::fs::Permissions::from_mode(0o666))
                .expect("unsafe mode");
            assert_eq!(
                open_model(&root, &model).await.err().as_deref(),
                Some("model_artifact_not_service_owned")
            );
            std::fs::set_permissions(&name, std::fs::Permissions::from_mode(0o600))
                .expect("private mode");
            std::fs::write(&name, b"bad payload").expect("same-length corruption");
            let mut held = verified.handle;
            std::fs::write(root.join("other.bin"), b"other accepted bytes").expect("other model");
            assert!(
                existing_or_retire_corrupt(&root, &directory, &model, &cancel)
                    .await
                    .expect("retirement")
                    .is_none()
            );
            assert!(!name.exists());
            assert!(std_file_identity(&held)
                .expect("held identity")
                .same_inode(old_identity));
            let mut old_bytes = Vec::new();
            held.read_to_end(&mut old_bytes)
                .expect("held bytes remain readable");
            assert_eq!(old_bytes, b"bad payload");
            assert_eq!(
                std::fs::read(root.join("other.bin")).expect("other retained"),
                b"other accepted bytes"
            );
            let staged = temporary(directory.clone(), model.download_bytes)
                .await
                .expect("reserved");
            let mut output = tokio::fs::File::from_std(staged.handle.try_clone().expect("writer"));
            let mut received = 0;
            write_chunk(
                &mut output,
                &mut received,
                model.download_bytes,
                b"model bytes",
                &cancel,
            )
            .await
            .expect("replacement");
            drop(output);
            let replacement = finish_download(&root, staged, &model, received, &cancel)
                .await
                .expect("reinstalled");
            assert!(!replacement.identity.same_inode(old_identity));
            assert_eq!(
                std::fs::read(&replacement.path).expect("replacement bytes"),
                b"model bytes"
            );
        }

        #[tokio::test]
        async fn low_disk_reservation_creates_no_partial_file() {
            let temporary_root = tempfile::tempdir().expect("root");
            let root = temporary_root
                .path()
                .canonicalize()
                .expect("canonical")
                .join("models");
            let directory = model_directory(&root).await.expect("directory");
            assert_eq!(
                temporary(directory.clone(), u64::MAX)
                    .await
                    .err()
                    .as_deref(),
                Some("model_disk_full")
            );
            assert!(directory
                .child_names(10)
                .await
                .expect("children")
                .is_empty());
        }

        #[tokio::test]
        async fn restart_retires_only_orphan_temporaries_after_exclusive_directory_lock() {
            let temporary_root = tempfile::tempdir().expect("root");
            let root = temporary_root
                .path()
                .canonicalize()
                .expect("canonical")
                .join("models");
            let first = model_directory(&root).await.expect("directory");
            lock_directory(&first).expect("lock");
            let other = SecureDirectory::open(&root).await.expect("second open");
            assert!(lock_directory(&other).is_err());
            std::fs::write(
                root.join(".0123456789abcdef0123456789abcdef.part"),
                b"interrupted",
            )
            .expect("orphan");
            std::fs::write(root.join("keep.bin"), b"accepted model").expect("accepted");
            retire_interrupted(&first).await.expect("retire");
            assert_eq!(
                first.child_names(10).await.expect("children"),
                vec!["keep.bin"]
            );
            drop(first);
            lock_directory(&other).expect("prior owner gone");
        }

        #[test]
        fn redirect_authority_refuses_downgrade_credentials_ports_and_unknown_hosts() {
            for url in [
                "http://huggingface.co/file",
                "https://user:secret@huggingface.co/file",
                "https://huggingface.co:444/file",
                "https://huggingface.co.attacker.test/file",
                "https://127.0.0.1/file",
                "https://us.aws.cdn.hf.co/file#fragment",
            ] {
                assert!(
                    !approved_url(&reqwest::Url::parse(url).expect("URL")),
                    "{url}"
                );
            }
            assert!(approved_url(
                &reqwest::Url::parse("https://us.aws.cdn.hf.co/file?signature=never-log")
                    .expect("approved URL")
            ));
        }

        #[tokio::test]
        async fn cancelled_model_hash_reads_at_most_one_chunk_and_never_qualifies() {
            let temporary = tempfile::tempdir().expect("root");
            let root = temporary
                .path()
                .canonicalize()
                .expect("canonical")
                .join("models");
            model_directory(&root).await.expect("owned root");
            let bytes = vec![37_u8; 512 * 1024];
            let spec = spec(&bytes);
            let path = root.join(format!("{}.bin", spec.sha256));
            std::fs::write(&path, bytes).expect("large regular model");
            let mut handle = File::open(&path).expect("model");
            let identity = std_file_identity(&handle).expect("identity");
            let before = CancellationToken::new();
            before.cancel();
            assert_eq!(
                open_model_cancellable(&root, &spec, &before)
                    .await
                    .err()
                    .as_deref(),
                Some("worker_interrupted")
            );
            assert_eq!(
                hash_held_cancellable(&mut handle, &spec.sha256, spec.download_bytes, &before)
                    .err()
                    .as_deref(),
                Some("worker_interrupted")
            );
            assert_eq!(handle.stream_position().expect("no reads"), 0);
            let during = CancellationToken::new();
            let signal = during.clone();
            assert_eq!(
                hash_held_observed(
                    &mut handle,
                    &spec.sha256,
                    spec.download_bytes,
                    &during,
                    || signal.cancel()
                )
                .err()
                .as_deref(),
                Some("worker_interrupted")
            );
            assert_eq!(handle.stream_position().expect("bounded read"), 128 * 1024);
            assert!(!DIGESTS
                .get_or_init(|| Mutex::new(Vec::new()))
                .lock()
                .expect("digest cache")
                .iter()
                .any(|(cached, digest)| *cached == identity && digest == &spec.sha256));
            let accepted = open_model_cancellable(&root, &spec, &CancellationToken::new())
                .await
                .expect("fresh uncancelled verification");
            assert_eq!(accepted.identity, identity);
        }

        #[tokio::test]
        async fn metadata_identity_never_qualifies_changed_or_unsafe_model_bytes() {
            use std::os::unix::fs::PermissionsExt;
            let temporary = tempfile::tempdir().expect("root");
            let root = temporary
                .path()
                .canonicalize()
                .expect("canonical")
                .join("models");
            model_directory(&root).await.expect("owned store");
            let spec = spec(b"model bytes");
            let path = root.join(format!("{}.bin", spec.sha256));
            std::fs::write(&path, b"model bytes").expect("model");
            let first = model_identity(&root, &spec).await.expect("metadata only");
            assert_eq!(
                open_model(&root, &spec)
                    .await
                    .expect("verified bytes")
                    .identity,
                first
            );
            std::fs::write(&path, b"other bytes").expect("same-length change");
            let changed = model_identity(&root, &spec)
                .await
                .expect("presence does not hash");
            assert_ne!(first, changed);
            assert_eq!(
                open_model(&root, &spec).await.err().as_deref(),
                Some("artifact_digest_mismatch")
            );
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))
                .expect("unsafe mode");
            assert_eq!(
                model_identity(&root, &spec).await.err().as_deref(),
                Some("model_artifact_not_service_owned")
            );
            std::fs::remove_file(&path).expect("remove unsafe model");
            let external = temporary.path().join("external");
            std::fs::write(&external, b"model bytes").expect("external");
            symlink(&external, &path).expect("symlink model");
            assert!(model_identity(&root, &spec).await.is_err());
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o777))
                .expect("unsafe store");
            assert_eq!(
                model_identity(&root, &spec).await.err().as_deref(),
                Some("model_store_not_service_owned")
            );
        }

        #[test]
        fn identity_cache_cannot_hide_model_replacement() {
            let root = tempfile::tempdir().expect("root");
            let path = root.path().join("model");
            std::fs::write(&path, b"model bytes").expect("model");
            let expected = spec(b"model bytes");
            let mut handle = File::open(&path).expect("handle");
            hash_held(&mut handle, &expected.sha256, expected.download_bytes).expect("verified");
            std::fs::write(&path, b"other bytes").expect("changed");
            assert!(hash_held(&mut handle, &expected.sha256, expected.download_bytes).is_err());
        }
    }
}

#[cfg(unix)]
pub(crate) use local::{inspect_engine, install_model, model_identity, open_model_cancellable};

#[cfg(all(unix, test))]
pub(crate) use local::open_model;

#[cfg(not(unix))]
pub(crate) async fn inspect_engine() -> Result<Engine, String> {
    Err("managed_engine_platform_unqualified".into())
}
#[cfg(all(not(unix), test))]
pub(crate) async fn open_model(_: &Path, _: &CatalogModel) -> Result<ManagedModel, String> {
    Err("managed_model_platform_unqualified".into())
}
#[cfg(not(unix))]
pub(crate) async fn open_model_cancellable(
    _: &Path,
    _: &CatalogModel,
    _: &CancellationToken,
) -> Result<ManagedModel, String> {
    Err("managed_model_platform_unqualified".into())
}

#[cfg(not(unix))]
pub(crate) async fn model_identity(_: &Path, _: &CatalogModel) -> Result<FileIdentity, String> {
    Err("managed_model_platform_unqualified".into())
}

#[cfg(not(unix))]
pub(crate) async fn install_model(
    _: &Path,
    _: &CatalogModel,
    _: &CancellationToken,
) -> Result<ManagedModel, String> {
    Err("managed_model_platform_unqualified".into())
}
