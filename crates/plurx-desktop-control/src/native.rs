//! Native bootstrap and per-user immutable runtime promotion. Never writes protected legacy prefixes.
use crate::{
    installation::*,
    protocol::{ErrorCode, SupervisorKind},
    secure_fs, supervisor,
    transaction::{JournalV1, TransactionStore},
    update, Error, Result,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    time::Duration,
};

pub const CONTROL_BINARIES: &[&str] = &[
    "plurx-control",
    "plurx-install-helper",
    "plurx-update-worker",
    "plurx-recovery",
    "plurx-service-launch",
];
pub const CLOSURE_NAME: &str = "runtime-closure-v1.json";
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClosureFileV1 {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub executable: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeClosureV1 {
    pub schema_version: u32,
    pub version: String,
    pub files: Vec<ClosureFileV1>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseIdentityV1 {
    pub schema_version: u32,
    pub repository: String,
    pub version: String,
    pub source_commit: String,
    pub architecture: Architecture,
}
impl ReleaseIdentityV1 {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1
            || self.repository != REPOSITORY
            || semver::Version::parse(&self.version).is_err()
            || self.source_commit.len() != 40
            || !self.source_commit.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(invalid("invalid installed release identity"));
        }
        Ok(())
    }
}
/// Fresh installs may start by default; an identified legacy or recorded
/// installation preserves its prior stopped state unless explicitly started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapDisposition {
    Fresh,
    AdoptedStopped,
    ExistingRecord,
}
impl BootstrapDisposition {
    pub fn starts_initial_daemon(self, no_start: bool) -> bool {
        self == Self::Fresh && !no_start
    }
}

pub fn bootstrap_package(root: &Path, no_start: bool) -> Result<InstallationV1> {
    if !cfg!(target_os = "macos") || native_uid() == 0 || !root.is_absolute() {
        return Err(Error::new(ErrorCode::AuthorizationDenied,"macOS package bootstrap requires ordinary native owning UID and absolute extracted package directory"));
    }
    let release: ReleaseIdentityV1 =
        secure_fs::open_dir(root, native_uid())?.json("release.json")?;
    release.validate()?;
    if release.architecture != Architecture::Universal {
        return Err(invalid("native macOS release must be universal"));
    }
    verify_closure(root, &release.version)?;
    let record = default_record()?;
    if record.exists() {
        let old = InstallationV1::load(&record)?;
        let store = TransactionStore::locked(&old)?;
        let mut j = store.create(&old, crate::protocol::Operation::Upgrade, None)?;
        j.target_version = Some(release.version.clone());
        j.rollback = update::Rollback::ManualRecovery;
        store.save(&j)?;
        crate::transaction::stop_clean(&old, &store, &mut j)?;
        let install_lock = crate::launch_lock::acquire(&old, true)?;
        if !supervisor::observe(&old)?.stopped() {
            return Err(invalid(
                "external service launch raced bootstrap; installation refused",
            ));
        }
        j.transition(crate::transaction::Phase::Installing)?;
        store.save(&j)?;
        promote(&old, &store, &j, root)?;
        j.transition(crate::transaction::Phase::CandidateInstalled)?;
        store.save(&j)?;
        let mut installed = old.clone();
        installed.current_version = release.version;
        installed.current_tag = format!("v{}", installed.current_version);
        installed.current_commit = release.source_commit;
        installed.trust_key_ids = update::production_keys()?
            .into_iter()
            .map(|k| k.id)
            .collect();
        installed.last_transaction_id = Some(j.transaction_id.clone());
        crate::controller::save_installation(&installed)?;
        j.transition(crate::transaction::Phase::Restoring)?;
        store.save(&j)?;
        drop(install_lock);
        if j.initial_running && !no_start {
            crate::controller::authorize_start(&store, &mut j)?;
            supervisor::start(&installed)?;
            crate::controller::wait_ready(&installed, &j, Some(&installed.current_commit))?;
        }
        j.transition(crate::transaction::Phase::Committed)?;
        j.start_permit_expires = None;
        store.save(&j)?;
        return Ok(installed);
    }
    let (installed, disposition) = bootstrap_with_disposition(
        InstallMethod::MacosNative,
        &release.version,
        &release.source_commit,
        true,
        "http://127.0.0.1:32400",
        Some(root),
    )?;
    if disposition.starts_initial_daemon(no_start) {
        supervisor::start(&installed)?;
    }
    Ok(installed)
}
fn invalid(m: &str) -> Error {
    Error::new(ErrorCode::InvalidInstallation, m)
}
pub fn relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path.components().all(|c| matches!(c, Component::Normal(_)))
}
fn regular_files(root: &Path, base: &Path, out: &mut BTreeSet<String>) -> Result<()> {
    for entry in std::fs::read_dir(base)? {
        let entry = entry?;
        let m = entry.file_type()?;
        if m.is_symlink() || (!m.is_file() && !m.is_dir()) {
            return Err(invalid("native closure contains a link or special file"));
        }
        if m.is_dir() {
            regular_files(root, &entry.path(), out)?;
        } else {
            out.insert(
                entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|_| invalid("closure escaped root"))?
                    .to_str()
                    .ok_or_else(|| invalid("non-UTF8 closure member"))?
                    .to_string(),
            );
        }
        if out.len() > 100000 {
            return Err(invalid("closure member count exceeded"));
        }
    }
    Ok(())
}
/// Resolve the one canonical wrapper of a verified macOS release archive.
/// The artifact and version must come from the same verified manifest. This
/// rejects flat archives and every sibling; closure verification still follows.
/// Manual bootstrap accepts a direct package root and does not use this helper.
pub fn archive_package_root(
    extracted: &Path,
    artifact: &update::ArtifactV1,
    version: &str,
) -> Result<PathBuf> {
    let expected_name = format!("plurxd-{version}-macos-universal.tar.gz");
    if semver::Version::parse(version).is_err()
        || !update::asset_name(&expected_name)
        || artifact.name != expected_name
        || artifact.platform != update::Platform::Macos
        || artifact.architecture != Architecture::Universal
        || artifact.format != update::Format::TarGz
        || artifact.component != update::Component::NativeRuntime
        || artifact.package_name != "plurx-native"
    {
        return Err(invalid("noncanonical native archive artifact/version"));
    }
    let wrapper = expected_name
        .strip_suffix(".tar.gz")
        .ok_or_else(|| invalid("native archive suffix"))?;
    // Validate every ancestor without following links, then open the sole
    // wrapper relative to that held directory with O_NOFOLLOW as well.
    let dir = secure_fs::open_dir(extracted, native_uid())?;
    let mut entries = std::fs::read_dir(extracted)?;
    let entry = entries
        .next()
        .transpose()?
        .ok_or_else(|| invalid("native archive has no canonical package wrapper"))?;
    if entry.file_name() != std::ffi::OsStr::new(wrapper)
        || !entry.file_type()?.is_dir()
        || entries.next().transpose()?.is_some()
    {
        return Err(invalid(
            "native archive must contain exactly its canonical package directory",
        ));
    }
    dir.child(wrapper, false)?;
    Ok(extracted.join(wrapper))
}
pub fn verify_closure(root: &Path, version: &str) -> Result<RuntimeClosureV1> {
    let dir = secure_fs::open_dir(root, native_uid())?;
    let closure: RuntimeClosureV1 = dir.json(CLOSURE_NAME)?;
    if closure.schema_version != 1 || closure.version != version || closure.files.len() > 100000 {
        return Err(invalid("native closure schema/version/count mismatch"));
    }
    let mut expected = BTreeSet::from([CLOSURE_NAME.to_string()]);
    for file in &closure.files {
        let path = Path::new(&file.path);
        if !relative(path)
            || !expected.insert(file.path.clone())
            || file.size > update::MAX_ARTIFACT
        {
            return Err(invalid("invalid/duplicate native closure member"));
        }
        let full = root.join(path);
        let parent = secure_fs::open_dir(
            full.parent().ok_or_else(|| invalid("member parent"))?,
            native_uid(),
        )?;
        let name = full
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| invalid("member name"))?;
        let mut source = parent.open(name)?;
        if source.metadata()?.len() != file.size
            || (source.metadata()?.mode() & 0o111 != 0) != file.executable
        {
            return Err(invalid("native closure size/mode mismatch"));
        }
        let mut digest = Sha256::new();
        std::io::copy(&mut source, &mut DigestWriter(&mut digest))?;
        if hex::encode(digest.finalize()) != file.sha256 {
            return Err(invalid("native closure hash mismatch"));
        }
    }
    let mut actual = BTreeSet::new();
    regular_files(root, root, &mut actual)?;
    if actual != expected {
        return Err(invalid("native closure has missing or unlisted files"));
    }
    for required in [
        "runtime/plurxd",
        "runtime/ffmpeg",
        "runtime/ffprobe",
        "Plurx Server.app/Contents/Info.plist",
    ] {
        if !expected.contains(required) {
            return Err(invalid(
                "native closure missing a required runtime/app component",
            ));
        }
    }
    for binary in CONTROL_BINARIES {
        if !expected.contains(&format!("control/{binary}")) {
            return Err(invalid(
                "native closure missing controller/recovery entrypoint",
            ));
        }
    }
    Ok(closure)
}
struct DigestWriter<'a>(&'a mut Sha256);
impl std::io::Write for DigestWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn private_dir(path: &Path, uid: u32) -> Result<()> {
    if path.exists() {
        secure_fs::open_dir(path, uid)?;
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("directory has no parent"))?;
    private_dir(parent, uid)?;
    secure_fs::open_dir(parent, uid)?.child(
        path.file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| invalid("directory name"))?,
        true,
    )?;
    Ok(())
}
pub fn copy_tree(source: &Path, target: &Path, uid: u32) -> Result<()> {
    private_dir(target, uid)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let m = entry.file_type()?;
        if m.is_dir() {
            copy_tree(&source_path, &target_path, uid)?;
        } else if m.is_file() {
            let source_dir = secure_fs::open_dir(source, uid)?;
            let target_dir = secure_fs::open_dir(target, uid)?;
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| invalid("copy name"))?;
            let mode = if entry.metadata()?.mode() & 0o111 != 0 {
                0o500
            } else {
                0o400
            };
            target_dir.write(name, &source_dir.read(name, update::MAX_ARTIFACT)?, mode)?;
        } else {
            return Err(invalid("native copy refuses links and special files"));
        }
    }
    std::fs::File::open(target)?.sync_all()?;
    Ok(())
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromotionV1 {
    pub schema_version: u32,
    pub transaction_id: String,
    pub version_id: String,
    pub previous_current: Option<PathBuf>,
    pub app_backup: Option<PathBuf>,
    pub app_promoted: bool,
    pub controls_promoted: Vec<String>,
    pub pointer_promoted: bool,
}
fn current_target(runtime: &Path) -> Result<Option<PathBuf>> {
    match std::fs::read_link(runtime.join("current")) {
        Ok(target) => {
            let c: Vec<_> = target.components().collect();
            if c.len() != 2
                || c[0] != Component::Normal(std::ffi::OsStr::new("versions"))
                || !matches!(c[1], Component::Normal(_))
            {
                return Err(invalid(
                    "runtime/current is not a confined relative versions pointer",
                ));
            }
            Ok(Some(target))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn pointer(runtime: &Path, target: &Path) -> Result<()> {
    if !relative(target) {
        return Err(invalid("unsafe current pointer"));
    }
    let tmp = runtime.join(format!(".current-{}", uuid::Uuid::new_v4()));
    std::os::unix::fs::symlink(target, &tmp)?;
    std::fs::rename(&tmp, runtime.join("current"))?;
    std::fs::File::open(runtime)?.sync_all()?;
    Ok(())
}
pub fn promote(
    i: &InstallationV1,
    store: &TransactionStore,
    j: &JournalV1,
    extracted: &Path,
) -> Result<()> {
    let version = j
        .target_version
        .as_deref()
        .ok_or_else(|| invalid("missing promotion target"))?;
    verify_closure(extracted, version)?;
    let version_id = format!("{}-{}", version, j.transaction_id);
    let version_path = i.paths.runtime.join("versions").join(&version_id);
    private_dir(&i.paths.runtime.join("versions"), i.owner_uid)?;
    if version_path.exists() {
        return Err(invalid("immutable runtime version already exists"));
    }
    copy_tree(extracted, &version_path, i.owner_uid)?;
    verify_closure(&version_path, version)?;
    let dir = store.dir(&j.transaction_id)?;
    let mut p = PromotionV1 {
        schema_version: 1,
        transaction_id: j.transaction_id.clone(),
        version_id: version_id.clone(),
        previous_current: current_target(&i.paths.runtime)?,
        app_backup: None,
        app_promoted: false,
        controls_promoted: Vec::new(),
        pointer_promoted: false,
    };
    dir.write_json("promotion.json", &p)?;
    let home = home_for(i.owner_uid)?;
    let apps = home.join("Applications");
    private_dir(&apps, i.owner_uid)?;
    let app = apps.join("Plurx Server.app");
    let incoming = apps.join(format!(".Plurx-incoming-{}", j.transaction_id));
    copy_tree(
        &version_path.join("Plurx Server.app"),
        &incoming,
        i.owner_uid,
    )?;
    if app.exists() {
        secure_fs::open_dir(&app, i.owner_uid)?;
        let backup = apps.join(format!(".Plurx-retained-{}", j.transaction_id));
        p.app_backup = Some(backup.clone());
        dir.write_json("promotion.json", &p)?;
        std::fs::rename(&app, backup)?;
        std::fs::File::open(&apps)?.sync_all()?;
    }
    // Write-ahead records intent; recovery handles either side of each rename.
    p.app_promoted = true;
    dir.write_json("promotion.json", &p)?;
    std::fs::rename(&incoming, &app)?;
    std::fs::File::open(&apps)?.sync_all()?;
    let backup = dir.child("old-control", true)?;
    let control = i.state_dir()?;
    let source = secure_fs::open_dir(&version_path.join("control"), i.owner_uid)?;
    for binary in CONTROL_BINARIES {
        backup.write(binary, &control.read(binary, 256 * 1024 * 1024)?, 0o500)?;
        p.controls_promoted.push((*binary).into());
        dir.write_json("promotion.json", &p)?;
        control.write(binary, &source.read(binary, 256 * 1024 * 1024)?, 0o500)?;
    }
    p.pointer_promoted = true;
    dir.write_json("promotion.json", &p)?;
    pointer(
        &i.paths.runtime,
        &PathBuf::from("versions").join(&version_id),
    )?;
    Ok(())
}
pub fn rollback(i: &InstallationV1, store: &TransactionStore, j: &JournalV1) -> Result<()> {
    if j.rollback != update::Rollback::BinaryCompatible {
        return Err(Error::new(ErrorCode::RecoveryRequired, "candidate may have written incompatible data; binary rollback and DB/Raft restore are not automatic"));
    }
    let dir = store.dir(&j.transaction_id)?;
    let p: PromotionV1 = dir.json("promotion.json")?;
    if p.schema_version != 1 || p.transaction_id != j.transaction_id {
        return Err(invalid("promotion journal identity mismatch"));
    }
    if let Some(old) = p.previous_current {
        if !relative(&old)
            || old.components().count() != 2
            || old.components().next() != Some(Component::Normal(std::ffi::OsStr::new("versions")))
        {
            return Err(invalid("retained runtime escaped immutable versions root"));
        }
        verify_closure(&i.paths.runtime.join(&old), &j.old_version)?;
        pointer(&i.paths.runtime, &old)?;
    }
    let old_control = dir.child("old-control", false)?;
    for binary in p.controls_promoted {
        if !CONTROL_BINARIES.contains(&binary.as_str()) {
            return Err(invalid("unknown promoted control binary"));
        }
        i.state_dir()?.write(
            &binary,
            &old_control.read(&binary, 256 * 1024 * 1024)?,
            0o500,
        )?;
    }
    if let Some(backup) = p.app_backup {
        let apps = home_for(i.owner_uid)?.join("Applications");
        if backup != apps.join(format!(".Plurx-retained-{}", j.transaction_id)) {
            return Err(invalid("app rollback path escaped managed namespace"));
        }
        if backup.exists() {
            let current = apps.join("Plurx Server.app");
            if current.exists() {
                std::fs::rename(
                    &current,
                    apps.join(format!(".Plurx-failed-{}", j.transaction_id)),
                )?;
            }
            std::fs::rename(&backup, &current)?;
            std::fs::File::open(&apps)?.sync_all()?;
        }
    }
    Ok(())
}
pub fn daemon_path(i: &InstallationV1) -> Result<PathBuf> {
    if i.method == InstallMethod::MacosNative {
        let target = current_target(&i.paths.runtime)?
            .ok_or_else(|| invalid("no selected immutable runtime"))?;
        let version_root = i.paths.runtime.join(target);
        let closure: RuntimeClosureV1 =
            secure_fs::open_dir(&version_root, i.owner_uid)?.json(CLOSURE_NAME)?;
        verify_closure(&version_root, &closure.version)?;
        Ok(version_root.join("runtime/plurxd"))
    } else {
        // /usr/bin/plurxd is the package convenience symlink. The managed
        // launcher validates the real immutable package leaf with O_NOFOLLOW.
        if matches!(i.method, InstallMethod::LinuxDeb | InstallMethod::LinuxRpm) {
            Ok(PathBuf::from("/usr/lib/plurx/plurxd"))
        } else {
            Err(invalid(
                "daemon path requires a managed native installation",
            ))
        }
    }
}
/// The replacement's compiled roots, not downloaded metadata, define its next bridge.
pub fn installed_trust_keys(i: &InstallationV1, authenticated_signer: &str) -> Result<Vec<String>> {
    let path = if i.method == InstallMethod::MacosNative {
        i.paths.state.join("plurx-control")
    } else {
        PathBuf::from("/usr/lib/plurx-control/plurx-control")
    };
    let parent = path
        .parent()
        .ok_or_else(|| invalid("installed controller parent"))?;
    secure_fs::open_dir(parent, i.owner_uid)?.open("plurx-control")?;
    let (code, text) = supervisor::output(
        path.to_str()
            .ok_or_else(|| invalid("controller path encoding"))?,
        &["trust-info".into()],
        Duration::from_secs(5),
    )?;
    if code != 0 {
        return Err(invalid(
            "replacement controller cannot prove compiled public roots",
        ));
    }
    let roots = update::parse_trust_roots(text.as_bytes())?;
    update::bridge_key_ids(&roots, authenticated_signer, &update::production_keys()?)
}
/// Initial bootstrap has no arbitrary privileged destination/source argument. The macOS
/// archive root is derived from the running ARCHIVE/control/plurx-control executable.
pub fn bootstrap(
    method: InstallMethod,
    version: &str,
    commit: &str,
    desktop: bool,
    browser_origin: &str,
    package_root: Option<&Path>,
) -> Result<InstallationV1> {
    bootstrap_with_disposition(
        method,
        version,
        commit,
        desktop,
        browser_origin,
        package_root,
    )
    .map(|(installed, _)| installed)
}
fn bootstrap_with_disposition(
    method: InstallMethod,
    version: &str,
    commit: &str,
    desktop: bool,
    browser_origin: &str,
    package_root: Option<&Path>,
) -> Result<(InstallationV1, BootstrapDisposition)> {
    semver::Version::parse(version).map_err(|_| invalid("invalid bootstrap semver"))?;
    if commit.len() != 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid("invalid bootstrap source commit"));
    }
    validate_browser_url(browser_origin)?;
    let uid = native_uid();
    let mac = method == InstallMethod::MacosNative;
    if mac != cfg!(target_os = "macos")
        || (mac && uid == 0)
        || (!mac && uid != 0)
        || !matches!(
            method,
            InstallMethod::MacosNative | InstallMethod::LinuxDeb | InstallMethod::LinuxRpm
        )
    {
        return Err(Error::new(
            ErrorCode::AuthorizationDenied,
            "bootstrap requires native platform authority (per-user macOS; root Linux)",
        ));
    }
    let record = default_record()?;
    if record.exists() {
        let i = InstallationV1::load(&record)?;
        if i.method != method {
            return Err(invalid("existing installation uses a different method"));
        }
        return Ok((i, BootstrapDisposition::ExistingRecord));
    }
    let (service_uid, paths, service, architecture) = if mac {
        let home = home_for(uid)?;
        let runtime = home.join("Library/Application Support/plurx/runtime");
        (
            uid,
            ManagedPathsV1 {
                state: runtime.join("control"),
                socket: runtime.join("control/desktop-control.sock"),
                runtime: runtime.clone(),
                config: home.join("Library/Application Support/plurx/plurx.toml"),
                data: home.join("Library/Application Support/plurx"),
                log: home.join("Library/Logs/plurxd.log"),
                canonical_plist: Some(runtime.join("control/com.plurx.plurxd.plist")),
                login_plist: Some(home.join("Library/LaunchAgents/com.plurx.plurxd.plist")),
            },
            ServiceV1 {
                kind: SupervisorKind::Launchd,
                name: LAUNCH_LABEL.into(),
                domain: format!("gui/{uid}"),
            },
            Architecture::Universal,
        )
    } else {
        // Resolve the package-created service account from the OS account database.
        let name = std::ffi::CString::new("plurx").map_err(|_| invalid("service account name"))?;
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut out = std::ptr::null_mut();
        let mut buf = vec![0u8; 65536];
        let r = unsafe {
            libc::getpwnam_r(
                name.as_ptr(),
                &mut pwd,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut out,
            )
        };
        if r != 0 || out.is_null() || pwd.pw_uid == 0 {
            return Err(invalid(
                "package must create dedicated plurx account before bootstrap",
            ));
        }
        (
            pwd.pw_uid,
            ManagedPathsV1 {
                state: "/var/lib/plurx-control".into(),
                socket: "/run/plurx/desktop-control.sock".into(),
                runtime: "/usr/lib/plurx".into(),
                config: "/etc/plurx/plurx.toml".into(),
                data: "/var/lib/plurx".into(),
                log: "/var/log/plurxd.log".into(),
                canonical_plist: None,
                login_plist: None,
            },
            ServiceV1 {
                kind: SupervisorKind::Systemd,
                name: SYSTEM_UNIT.into(),
                domain: "system".into(),
            },
            if cfg!(target_arch = "aarch64") {
                Architecture::Arm64
            } else {
                Architecture::Amd64
            },
        )
    };
    let mut i = InstallationV1 {
        schema_version: 1,
        installation_id: uuid::Uuid::new_v4().to_string(),
        method,
        owner_uid: uid,
        service_uid,
        architecture,
        desktop_installed: desktop,
        repository: REPOSITORY.into(),
        current_version: version.into(),
        current_tag: format!("v{version}"),
        current_commit: commit.into(),
        controller_protocol: 1,
        trust_key_ids: update::production_keys()?
            .iter()
            .map(|k| k.id.to_string())
            .collect(),
        last_transaction_id: None,
        paths,
        service,
        browser_origin: browser_origin.into(),
    };
    let disposition = if mac {
        bootstrap_macos(
            &mut i,
            package_root.ok_or_else(|| invalid("native bootstrap package root absent"))?,
        )?
    } else {
        BootstrapDisposition::Fresh
    };
    private_dir(
        record.parent().ok_or_else(|| invalid("record parent"))?,
        uid,
    )?;
    private_dir(&i.paths.state, uid)?;
    if !mac {
        std::fs::set_permissions(&i.paths.state, std::fs::Permissions::from_mode(0o755))?;
        std::fs::set_permissions(
            record.parent().ok_or_else(|| invalid("record parent"))?,
            std::fs::Permissions::from_mode(0o755),
        )?;
        for (path, mode) in [
            (i.paths.state.join("shutdown-receipts"), 0o700),
            (PathBuf::from("/run/plurx"), 0o755),
        ] {
            if !path.exists() {
                std::fs::create_dir(&path)?;
            }
            let dir = secure_fs::open_dir(&path, i.service_uid)?;
            if unsafe { libc::fchown(dir.fd(), i.service_uid, u32::MAX) } != 0
                || unsafe { libc::fchmod(dir.fd(), mode as libc::mode_t) } != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
    }
    let state = i.state_dir()?;
    state.child("transactions", true)?;
    if mac {
        state.child("shutdown-receipts", true)?;
    }
    // Linux package owns creation/chown of the service-writable receipts and /run directories.
    crate::launch_lock::initialize(&i)?;
    i.validate()?;
    secure_fs::open_dir(
        record.parent().ok_or_else(|| invalid("record parent"))?,
        uid,
    )?
    .write(
        "installation.json",
        &serde_json::to_vec(&i)?,
        if mac { 0o600 } else { 0o644 },
    )?;
    Ok((i, disposition))
}
fn bootstrap_macos(i: &mut InstallationV1, root: &Path) -> Result<BootstrapDisposition> {
    if !cfg!(target_os = "macos") {
        return Err(invalid("macOS bootstrap on other platform"));
    }
    let home = home_for(i.owner_uid)?;
    bootstrap_macos_with(i, root, &home, supervisor::output)
}

/// Staged bootstrap seam: the production wrapper supplies the account-database
/// home and fixed bounded executor; fixtures supply only their owned paths.
pub fn bootstrap_macos_with<F>(
    i: &mut InstallationV1,
    root: &Path,
    home: &Path,
    mut execute: F,
) -> Result<BootstrapDisposition>
where
    F: FnMut(&str, &[String], Duration) -> Result<(i32, String)>,
{
    let system = format!("system/{LAUNCH_LABEL}");
    let (system_code, _) = execute(
        "/bin/launchctl",
        &["print".into(), system],
        Duration::from_secs(2),
    )?;
    if system_code != 113 {
        return Err(invalid("system-domain service exists or cannot be excluded; explicit authorized bootstrap cleanup is required"));
    }
    let target = format!("{}/{}", i.service.domain, LAUNCH_LABEL);
    let (code, info) = execute(
        "/bin/launchctl",
        &["print".into(), target.clone()],
        Duration::from_secs(2),
    )?;
    if code != 113 && code != 0 {
        return Err(invalid("cannot establish legacy launchd state"));
    }
    if info.lines().any(|l| l.trim().starts_with("pid = ")) {
        return Err(invalid("legacy daemon must be positively stopped before bootstrap migration; no force-stop attempted"));
    }
    let login = i
        .paths
        .login_plist
        .clone()
        .ok_or_else(|| invalid("login plist"))?;
    let legacy_login = match std::fs::symlink_metadata(&login) {
        Ok(meta) if meta.is_file() => true,
        Ok(_) => return Err(invalid("legacy login registration must be a regular file")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    let disposition = if code == 0 || legacy_login {
        BootstrapDisposition::AdoptedStopped
    } else {
        BootstrapDisposition::Fresh
    };
    let mut plist = if legacy_login {
        let parent = secure_fs::open_dir(
            login.parent().ok_or_else(|| invalid("login parent"))?,
            i.owner_uid,
        )?;
        parent.read("com.plurx.plurxd.plist", 1024 * 1024)?;
        let (status, json) = execute(
            "/usr/bin/plutil",
            &[
                "-convert".into(),
                "json".into(),
                "-o".into(),
                "-".into(),
                login.to_string_lossy().into_owned(),
            ],
            Duration::from_secs(2),
        )?;
        if status != 0 {
            return Err(invalid("legacy plist cannot be parsed"));
        }
        serde_json::from_str::<serde_json::Value>(&json)?
    } else {
        serde_json::json!({"Label":LAUNCH_LABEL,"RunAtLoad":true,"KeepAlive":{"SuccessfulExit":false},"ProcessType":"Standard","EnvironmentVariables":{},"StandardOutPath":i.paths.log,"StandardErrorPath":i.paths.log})
    };
    if plist.get("Label").and_then(|v| v.as_str()) != Some(LAUNCH_LABEL) {
        return Err(invalid("unrelated legacy plist"));
    }
    if let Some(env) = plist
        .get("EnvironmentVariables")
        .and_then(|v| v.as_object())
    {
        if let Some(data) = env.get("PLURX_DATA_DIR").and_then(|v| v.as_str()) {
            i.paths.data = PathBuf::from(data);
        }
        if let Some(config) = env.get("PLURX_CONFIG").and_then(|v| v.as_str()) {
            i.paths.config = PathBuf::from(config);
        }
    }
    if let Some(log) = plist.get("StandardOutPath").and_then(|v| v.as_str()) {
        i.paths.log = log.into();
    }
    if code == 0 {
        let (status, _) = execute(
            "/bin/launchctl",
            &["bootout".into(), target],
            Duration::from_secs(5),
        )?;
        if status != 0 {
            return Err(invalid("cannot unregister already-exited legacy job"));
        }
    }

    verify_closure(root, &i.current_version)?;
    let id = format!("{}-bootstrap-{}", i.current_version, i.installation_id);
    let version = i.paths.runtime.join("versions").join(&id);
    copy_tree(root, &version, i.owner_uid)?;
    verify_closure(&version, &i.current_version)?;
    private_dir(&i.paths.state, i.owner_uid)?;
    for binary in CONTROL_BINARIES {
        i.state_dir()?.write(
            binary,
            &secure_fs::open_dir(&version.join("control"), i.owner_uid)?
                .read(binary, 256 * 1024 * 1024)?,
            0o500,
        )?;
    }
    let apps = home.join("Applications");
    private_dir(&apps, i.owner_uid)?;
    let app = apps.join("Plurx Server.app");
    if app.exists() {
        return Err(invalid("stable app already exists without an installation record; explicit recovery/migration required"));
    }
    copy_tree(&version.join("Plurx Server.app"), &app, i.owner_uid)?;
    pointer(&i.paths.runtime, &PathBuf::from("versions").join(id))?;
    plist["ProgramArguments"] = serde_json::json!([i.paths.state.join("plurx-service-launch")]);
    let env = plist["EnvironmentVariables"]
        .as_object_mut()
        .ok_or_else(|| invalid("legacy environment is not a dictionary"))?;
    env.entry("PLURX_DATA_DIR")
        .or_insert(serde_json::json!(i.paths.data));
    env.entry("PLURX_FFMPEG").or_insert(serde_json::json!(i
        .paths
        .runtime
        .join("current/runtime/ffmpeg")));
    env.entry("PLURX_FFPROBE").or_insert(serde_json::json!(i
        .paths
        .runtime
        .join("current/runtime/ffprobe")));
    let canonical = i
        .paths
        .canonical_plist
        .as_ref()
        .ok_or_else(|| invalid("canonical plist"))?;
    let temp = i.paths.state.join("canonical-plist.json");
    i.state_dir()?
        .write("canonical-plist.json", &serde_json::to_vec(&plist)?, 0o600)?;
    let (status, _) = execute(
        "/usr/bin/plutil",
        &[
            "-convert".into(),
            "xml1".into(),
            "-o".into(),
            canonical.to_string_lossy().into_owned(),
            temp.to_string_lossy().into_owned(),
        ],
        Duration::from_secs(2),
    )?;
    if status != 0 {
        return Err(invalid("cannot encode canonical launchd plist"));
    }
    std::fs::set_permissions(canonical, std::fs::Permissions::from_mode(0o600))?;
    std::fs::File::open(canonical)?.sync_all()?;
    // Preserve login policy: replace only an existing positively identified managed registration.
    if legacy_login {
        secure_fs::open_dir(
            login.parent().ok_or_else(|| invalid("login parent"))?,
            i.owner_uid,
        )?
        .write("com.plurx.plurxd.plist", &std::fs::read(canonical)?, 0o600)?;
    }
    Ok(disposition)
}
