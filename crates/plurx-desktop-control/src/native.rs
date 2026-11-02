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
/// Local source installs explicitly attest their native architecture and exact
/// checkout identity. This marker is covered by the same complete file closure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceInstallV1 {
    pub schema_version: u32,
    pub source_commit: String,
    pub architecture: Architecture,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapPackageOrigin {
    Release,
    LocalSource,
}
/// Validate the explicit install route before any service or filesystem mutation.
/// Public release archives remain universal, independently of a local source build.
pub fn verify_bootstrap_identity(
    root: &Path,
    origin: BootstrapPackageOrigin,
) -> Result<ReleaseIdentityV1> {
    let dir = secure_fs::open_dir(root, native_uid())?;
    let release: ReleaseIdentityV1 = dir.json("release.json")?;
    release.validate()?;
    match origin {
        BootstrapPackageOrigin::Release if release.architecture != Architecture::Universal => {
            return Err(invalid("native macOS release must be universal"));
        }
        BootstrapPackageOrigin::LocalSource => {
            let source: SourceInstallV1 = dir.json("source-install-v1.json")?;
            if source.schema_version != 1
                || source.source_commit != release.source_commit
                || source.architecture != release.architecture
                || Some(source.architecture) != native_architecture()
            {
                return Err(invalid(
                    "local source install identity/architecture mismatch",
                ));
            }
        }
        _ => (),
    }
    verify_closure(root, &release.version)?;
    Ok(release)
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
    bootstrap_package_with_origin(root, no_start, BootstrapPackageOrigin::Release)
}
pub fn bootstrap_source_package(root: &Path, no_start: bool) -> Result<InstallationV1> {
    bootstrap_package_with_origin(root, no_start, BootstrapPackageOrigin::LocalSource)
}
fn bootstrap_package_with_origin(
    root: &Path,
    no_start: bool,
    origin: BootstrapPackageOrigin,
) -> Result<InstallationV1> {
    if !cfg!(target_os = "macos") || native_uid() == 0 || !root.is_absolute() {
        return Err(Error::new(ErrorCode::AuthorizationDenied,"macOS package bootstrap requires ordinary native owning UID and absolute extracted package directory"));
    }
    let release = verify_bootstrap_identity(root, origin)?;
    let record = default_record()?;
    if record.exists() {
        let old = InstallationV1::load(&record)?;
        let store = TransactionStore::locked(&old)?;
        let mut j = store.create(&old, crate::protocol::Operation::Upgrade, None)?;
        let result = (|| {
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
            installed.architecture = release.architecture;
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
            Ok(installed)
        })();
        if let Err(error) = &result {
            crate::controller::record_failure(&store, &mut j, error)?;
        }
        return result;
    }
    let (installed, disposition) = bootstrap_with_disposition(
        InstallMethod::MacosNative,
        &release.version,
        &release.source_commit,
        true,
        "http://127.0.0.1:32400",
        Some((root, release.architecture)),
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
/// Read-only source migration preflight. Release archive architecture/trust
/// routing remains independent of this explicitly local source entry point.
pub fn preflight_source_package(root: &Path) -> Result<()> {
    if !cfg!(target_os = "macos") || native_uid() == 0 || !root.is_absolute() {
        return Err(invalid(
            "source migration preflight requires the ordinary macOS owning user",
        ));
    }
    verify_bootstrap_identity(root, BootstrapPackageOrigin::LocalSource)?;
    let record = default_record()?;
    if record.exists() {
        let (system, _) = supervisor::output(
            "/bin/launchctl",
            &["print".into(), format!("system/{LAUNCH_LABEL}")],
            Duration::from_secs(2),
        )?;
        if system != 113 {
            return Err(invalid(
                "system-domain service exists or cannot be excluded before migration",
            ));
        }
        let installed = InstallationV1::load(&record)?;
        installed.managed()?;
        let state = installed.state_dir()?;
        let transactions = state.child("transactions", false)?;
        crate::transaction::ensure_resolved(
            crate::transaction::read_active(&state, &transactions)?.as_ref(),
        )?;
        return Ok(());
    }
    let home = home_for(native_uid())?;
    preflight_macos_migration_with(&home, native_uid(), supervisor::output)
}

fn preflight_destination(path: &Path, uid: u32) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return Err(invalid(
            "migration destination must be normalized and absolute",
        ));
    }
    let mut ancestor = path;
    loop {
        match std::fs::symlink_metadata(ancestor) {
            Ok(meta) if meta.is_dir() => {
                secure_fs::open_dir(ancestor, uid)?;
                return Ok(());
            }
            Ok(_) => return Err(invalid("migration destination is not a trusted directory")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| invalid("migration destination parent"))?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn preflight_macos_destinations(home: &Path, uid: u32) -> Result<()> {
    let record = home.join("Library/Application Support/plurx/installation.json");
    let runtime = home.join("Library/Application Support/plurx/runtime");
    let app = home.join("Applications/Plurx Server.app");
    for path in [&record, &runtime, &app] {
        match std::fs::symlink_metadata(path) {
            Ok(_) => return Err(invalid("native app, runtime or record already exists without a managed installation; explicit recovery/migration required")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    for path in [
        home.join("Applications"),
        runtime,
        home.join("Library/LaunchAgents"),
        home.join("Library/Logs"),
    ] {
        preflight_destination(&path, uid)?;
    }
    Ok(())
}

/// Fixture seam for preflight: no mkdir, copy, bootout, signal or startup
/// policy changes are permitted through this read-only executor.
pub fn preflight_macos_migration_with<F>(home: &Path, uid: u32, mut execute: F) -> Result<()>
where
    F: FnMut(&str, &[String], Duration) -> Result<(i32, String)>,
{
    preflight_macos_destinations(home, uid)?;
    let (system, _) = execute(
        "/bin/launchctl",
        &["print".into(), format!("system/{LAUNCH_LABEL}")],
        Duration::from_secs(2),
    )?;
    if system != 113 {
        return Err(invalid(
            "system-domain service exists or cannot be excluded before migration",
        ));
    }
    let (code, info) = execute(
        "/bin/launchctl",
        &["print".into(), format!("gui/{uid}/{LAUNCH_LABEL}")],
        Duration::from_secs(2),
    )?;
    if code != 0 && code != 113 {
        return Err(invalid(
            "cannot establish loaded legacy identity before migration",
        ));
    }
    let login = home.join("Library/LaunchAgents/com.plurx.plurxd.plist");
    match std::fs::symlink_metadata(&login) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && code == 113 => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(invalid(
                "loaded legacy service has no durable owned registration",
            ))
        }
        Err(error) => return Err(error.into()),
        Ok(meta) if !meta.is_file() || meta.uid() != uid || meta.mode() & 0o022 != 0 => {
            return Err(invalid(
                "legacy registration is not an owned safe regular file",
            ))
        }
        Ok(_) => (),
    }
    secure_fs::open_dir(
        login
            .parent()
            .ok_or_else(|| invalid("legacy plist parent"))?,
        uid,
    )?
    .read("com.plurx.plurxd.plist", 1024 * 1024)?;
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
        return Err(invalid(
            "legacy registration cannot be parsed before migration",
        ));
    }
    let plist: serde_json::Value = serde_json::from_str(&json)?;
    let args = plist
        .get("ProgramArguments")
        .and_then(|value| value.as_array())
        .ok_or_else(|| invalid("legacy executable identity is absent"))?;
    let binary = args
        .first()
        .and_then(|value| value.as_str())
        .map(PathBuf::from)
        .ok_or_else(|| invalid("legacy executable identity is absent"))?;
    if plist.get("Label").and_then(|value| value.as_str()) != Some(LAUNCH_LABEL)
        || !binary.is_absolute()
        || binary.file_name().and_then(|name| name.to_str()) != Some("plurxd")
        || args.len() > 2
        || (args.len() == 2 && args[1].as_str() != Some("run"))
    {
        return Err(invalid(
            "legacy service identity/arguments require explicit migration",
        ));
    }
    let meta = std::fs::symlink_metadata(&binary)?;
    if !meta.is_file()
        || (meta.uid() != uid && meta.uid() != 0)
        || meta.mode() & 0o022 != 0
        || meta.mode() & 0o111 == 0
    {
        return Err(invalid("legacy executable is not a trusted regular file"));
    }
    preflight_destination(
        binary
            .parent()
            .ok_or_else(|| invalid("legacy executable parent"))?,
        uid,
    )?;
    if code == 0 {
        let program = info
            .lines()
            .find_map(|line| line.trim().strip_prefix("program = "));
        if program != binary.to_str() {
            return Err(invalid(
                "loaded service executable differs from its on-disk registration",
            ));
        }
    }
    let env = plist
        .get("EnvironmentVariables")
        .and_then(|value| value.as_object())
        .ok_or_else(|| invalid("legacy environment is not a dictionary"))?;
    for value in [
        env.get("PLURX_DATA_DIR"),
        env.get("PLURX_CONFIG"),
        plist.get("StandardOutPath"),
        plist.get("StandardErrorPath"),
    ]
    .into_iter()
    .flatten()
    {
        let path = PathBuf::from(
            value
                .as_str()
                .ok_or_else(|| invalid("legacy path is not a string"))?,
        );
        preflight_destination(
            path.parent().ok_or_else(|| invalid("legacy path parent"))?,
            uid,
        )?;
    }
    Ok(())
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
/// Verify completed promotion before explicitly finishing an interrupted health check.
/// Write-ahead promotion flags alone do not prove the filesystem changes completed.
pub fn verify_promoted_installation(
    i: &InstallationV1,
    store: &TransactionStore,
    j: &JournalV1,
) -> Result<()> {
    let app = home_for(i.owner_uid)?.join("Applications/Plurx Server.app");
    verify_promoted_installation_at(i, store, j, &app)
}

fn verify_promoted_installation_at(
    i: &InstallationV1,
    store: &TransactionStore,
    j: &JournalV1,
    app: &Path,
) -> Result<()> {
    if i.method != InstallMethod::MacosNative
        || j.phase != crate::transaction::Phase::Restoring
        || j.operation != crate::protocol::Operation::Upgrade
        || j.installation_id != i.installation_id
        || i.last_transaction_id.as_deref() != Some(&j.transaction_id)
        || j.target_version.as_deref() != Some(&i.current_version)
    {
        return Err(invalid(
            "installed native candidate does not match restoration transaction",
        ));
    }
    let p: PromotionV1 = store.dir(&j.transaction_id)?.json("promotion.json")?;
    let version_id = format!("{}-{}", i.current_version, j.transaction_id);
    let target = PathBuf::from("versions").join(&version_id);
    if p.schema_version != 1
        || p.transaction_id != j.transaction_id
        || p.version_id != version_id
        || !p.app_promoted
        || !p.pointer_promoted
        || p.controls_promoted != CONTROL_BINARIES
        || current_target(&i.paths.runtime)?.as_ref() != Some(&target)
    {
        return Err(invalid(
            "native promotion is incomplete or current runtime differs",
        ));
    }
    let root = i.paths.runtime.join(target);
    let dir = secure_fs::open_dir(&root, i.owner_uid)?;
    let closure: RuntimeClosureV1 = dir.json(CLOSURE_NAME)?;
    if !closure.files.iter().any(|f| f.path == "release.json") {
        return Err(invalid(
            "promoted release identity is not bound to its closure",
        ));
    }
    let source = closure
        .files
        .iter()
        .any(|f| f.path == "source-install-v1.json");
    let release = verify_bootstrap_identity(
        &root,
        if source {
            BootstrapPackageOrigin::LocalSource
        } else {
            BootstrapPackageOrigin::Release
        },
    )?;
    if release.version != i.current_version
        || release.source_commit != i.current_commit
        || release.architecture != i.architecture
        || release.repository != i.repository
    {
        return Err(invalid(
            "promoted release differs from installed candidate identity",
        ));
    }
    verify_promoted_copies(i, app, &closure)
}

fn verify_promoted_copies(
    i: &InstallationV1,
    app: &Path,
    closure: &RuntimeClosureV1,
) -> Result<()> {
    let mut expected_app = BTreeSet::new();
    for file in &closure.files {
        let destination = if let Some(name) = file.path.strip_prefix("control/") {
            if !CONTROL_BINARIES.contains(&name) {
                continue;
            }
            i.paths.state.join(name)
        } else if let Some(name) = file.path.strip_prefix("Plurx Server.app/") {
            expected_app.insert(name.to_owned());
            app.join(name)
        } else {
            continue;
        };
        let parent = secure_fs::open_dir(
            destination
                .parent()
                .ok_or_else(|| invalid("promoted member parent"))?,
            i.owner_uid,
        )?;
        let mut installed = parent.open(
            destination
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| invalid("promoted member name"))?,
        )?;
        let metadata = installed.metadata()?;
        let mut digest = Sha256::new();
        std::io::copy(&mut installed, &mut DigestWriter(&mut digest))?;
        if metadata.len() != file.size
            || (metadata.mode() & 0o111 != 0) != file.executable
            || hex::encode(digest.finalize()) != file.sha256
        {
            return Err(invalid(
                "installed app/controller differs from promoted closure",
            ));
        }
    }
    let mut actual_app = BTreeSet::new();
    regular_files(app, app, &mut actual_app)?;
    if actual_app != expected_app {
        return Err(invalid(
            "installed app inventory differs from promoted closure",
        ));
    }
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

/// Retire media-tool paths written by the old macOS installer while retaining
/// genuine operator overrides. Resolve through `current` so upgrades and
/// rollback select the daemon and its media closure together.
pub fn packaged_media_override(
    runtime: &Path,
    tool: &str,
    inherited: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    let packaged = runtime.join("current/runtime").join(tool);
    let Some(value) = inherited.filter(|value| !value.is_empty()) else {
        return Some(packaged);
    };
    let path = Path::new(value);
    if path.file_name() != Some(std::ffi::OsStr::new(tool)) {
        return None;
    }
    let parent = path.parent()?;
    let legacy = ["/opt/homebrew/libexec/plurx", "/usr/local/libexec/plurx"]
        .iter()
        .any(|prefix| {
            parent.parent() == Some(Path::new(prefix))
                && parent
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("jellyfin-ffmpeg-"))
        });
    (legacy || path.starts_with(runtime.join("versions"))).then_some(packaged)
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
        package_root.map(|root| (root, Architecture::Universal)),
    )
    .map(|(installed, _)| installed)
}
fn bootstrap_with_disposition(
    method: InstallMethod,
    version: &str,
    commit: &str,
    desktop: bool,
    browser_origin: &str,
    package_root: Option<(&Path, Architecture)>,
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
            package_root
                .as_ref()
                .ok_or_else(|| invalid("native bootstrap package root absent"))?
                .1
                .clone(),
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
            package_root
                .ok_or_else(|| invalid("native bootstrap package root absent"))?
                .0,
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
    // Reject deterministic conflicts before unregistering even a stopped job.
    preflight_macos_destinations(home, i.owner_uid)?;
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

#[cfg(test)]
mod restoration_tests {
    use super::*;
    use crate::{protocol::*, transaction::Phase};

    #[test]
    fn interrupted_native_restoration_requires_exact_promoted_identity_and_copies() {
        let scratch = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/native-restoration-tests")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&scratch).expect("scratch");
        let scratch = scratch.canonicalize().expect("absolute scratch");
        let uid = native_uid();
        let id = uuid::Uuid::new_v4().to_string();
        let installation_id = uuid::Uuid::new_v4().to_string();
        let runtime = scratch.join("runtime");
        let mut i: InstallationV1 = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "installation_id": installation_id,
            "method": "macos-native", "owner_uid": uid, "service_uid": uid,
            "architecture": native_architecture().expect("native architecture"), "desktop_installed": true,
            "repository": REPOSITORY, "current_version": "0.7.0", "current_tag": "v0.7.0",
            "current_commit": "a".repeat(40), "controller_protocol": 1,
            "trust_key_ids": [], "last_transaction_id": id,
            "paths": {"state": runtime.join("control"), "runtime": runtime,
                "socket": scratch.join("socket"), "config": scratch.join("config"),
                "data": scratch.join("data"), "log": scratch.join("log")},
            "service": {"kind": "launchd", "name": LAUNCH_LABEL, "domain": format!("gui/{uid}")},
            "browser_origin": "http://127.0.0.1:32400"
        }))
        .expect("installation fixture");
        std::fs::create_dir_all(&i.paths.state).expect("state");
        let store = TransactionStore::locked(&i).expect("store");
        let mut j: JournalV1 = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "transaction_id": id, "installation_id": installation_id,
            "operation": "upgrade", "old_version": "0.7.0", "target_version": "0.7.0",
            "phase": "restoring", "initial_running": true,
            "initial_startup": {"state": "enabled", "detail": null},
            "policy_revision": "fixture", "config_digest": "fixture", "rollback": "manual_recovery"
        }))
        .expect("journal fixture");
        let transaction = store.transactions.child(&id, true).expect("transaction");
        let version_id = format!("0.7.0-{id}");
        let root = runtime.join("versions").join(&version_id);
        std::fs::create_dir_all(&root).expect("version");
        let release = ReleaseIdentityV1 {
            schema_version: 1,
            repository: REPOSITORY.into(),
            version: "0.7.0".into(),
            source_commit: i.current_commit.clone(),
            architecture: native_architecture().expect("native architecture"),
        };
        let mut members = vec![(
            "release.json".to_owned(),
            serde_json::to_vec(&release).expect("release"),
            false,
        )];
        members.push((
            "source-install-v1.json".into(),
            serde_json::to_vec(&SourceInstallV1 {
                schema_version: 1,
                source_commit: i.current_commit.clone(),
                architecture: release.architecture.clone(),
            })
            .expect("source identity"),
            false,
        ));
        for path in [
            "runtime/plurxd",
            "runtime/ffmpeg",
            "runtime/ffprobe",
            "Plurx Server.app/Contents/Info.plist",
        ]
        .into_iter()
        .map(str::to_owned)
        .chain(
            CONTROL_BINARIES
                .iter()
                .map(|name| format!("control/{name}")),
        ) {
            members.push((path, b"fixture".to_vec(), true));
        }
        let mut closure = RuntimeClosureV1 {
            schema_version: 1,
            version: "0.7.0".into(),
            files: Vec::new(),
        };
        for (name, bytes, executable) in &members {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("member parent");
            secure_fs::open_dir(path.parent().expect("parent"), uid)
                .expect("parent")
                .write(
                    path.file_name().expect("name").to_str().expect("UTF8"),
                    bytes,
                    if *executable { 0o500 } else { 0o400 },
                )
                .expect("member");
            closure.files.push(ClosureFileV1 {
                path: name.clone(),
                size: bytes.len() as u64,
                sha256: hex::encode(Sha256::digest(bytes)),
                executable: *executable,
            });
        }
        secure_fs::open_dir(&root, uid)
            .expect("root")
            .write_json(CLOSURE_NAME, &closure)
            .expect("closure");
        let app = scratch.join("Plurx Server.app");
        copy_tree(&root.join("Plurx Server.app"), &app, uid).expect("app");
        for name in CONTROL_BINARIES {
            store.root.write(name, b"fixture", 0o500).expect("control");
        }
        let mut p = PromotionV1 {
            schema_version: 1,
            transaction_id: id.clone(),
            version_id: version_id.clone(),
            previous_current: None,
            app_backup: None,
            app_promoted: true,
            controls_promoted: CONTROL_BINARIES.iter().map(|name| (*name).into()).collect(),
            pointer_promoted: true,
        };
        transaction
            .write_json("promotion.json", &p)
            .expect("promotion");
        pointer(&runtime, &PathBuf::from("versions").join(&version_id)).expect("pointer");
        let verify =
            |i: &InstallationV1, j: &JournalV1| verify_promoted_installation_at(i, &store, j, &app);
        verify(&i, &j).expect("healthy same-version source replacement");
        j.expected_node_id = Some("original-node".into());
        let mut identity: DaemonIdentityV1 = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "installation_id": installation_id,
            "process": {"pid": 123, "uid": uid, "start_generation": "fixture", "nonce": "fixture"},
            "node_id": "original-node", "instance_id": "fixture", "version": "0.7.0",
            "build": i.current_commit, "browser_origin": i.browser_origin,
            "supervisor": {"kind": "launchd", "name": LAUNCH_LABEL, "invocation_id": "fixture"},
            "ready": true
        }))
        .expect("daemon identity");
        let ready = |identity: &DaemonIdentityV1| {
            crate::controller::readiness_matches(&i, &j, identity, Some(&i.current_commit))
        };
        assert!(ready(&identity));
        identity.build = "b".repeat(40);
        assert!(
            !ready(&identity),
            "same version with a different build cannot recover"
        );
        identity.build = i.current_commit.clone();
        identity.node_id = "another-node".into();
        assert!(!ready(&identity));
        identity.node_id = "original-node".into();
        identity.ready = false;
        assert!(!ready(&identity));
        i.current_commit = "b".repeat(40);
        assert!(
            verify(&i, &j).is_err(),
            "version alone cannot identify candidate"
        );
        i.current_commit = release.source_commit;
        i.last_transaction_id = None;
        assert!(verify(&i, &j).is_err());
        i.last_transaction_id = Some(id.clone());
        j.installation_id = uuid::Uuid::new_v4().to_string();
        assert!(verify(&i, &j).is_err());
        j.installation_id = installation_id;
        p.controls_promoted.pop();
        transaction
            .write_json("promotion.json", &p)
            .expect("incomplete promotion");
        assert!(verify(&i, &j).is_err());
        p.controls_promoted = CONTROL_BINARIES.iter().map(|name| (*name).into()).collect();
        transaction
            .write_json("promotion.json", &p)
            .expect("promotion");
        pointer(&runtime, Path::new("versions/another-transaction")).expect("other pointer");
        assert!(verify(&i, &j).is_err());
        pointer(&runtime, &PathBuf::from("versions").join(version_id)).expect("pointer");
        store
            .root
            .write("plurx-control", b"altered", 0o500)
            .expect("altered control");
        assert!(verify(&i, &j).is_err());
        store
            .root
            .write("plurx-control", b"fixture", 0o500)
            .expect("control");
        secure_fs::open_dir(&app, uid)
            .expect("app")
            .write("extra", b"extra", 0o400)
            .expect("extra");
        assert!(verify(&i, &j).is_err());
        secure_fs::open_dir(&app, uid)
            .expect("app")
            .remove("extra")
            .expect("remove extra");
        secure_fs::open_dir(&root.join("runtime"), uid)
            .expect("runtime")
            .write("plurxd", b"altered", 0o500)
            .expect("altered daemon");
        assert!(verify(&i, &j).is_err());
        assert_eq!(
            j.phase,
            Phase::Restoring,
            "verification never commits on failure"
        );
        j.start_permit_expires = Some(u64::MAX);
        let failure = Error::new(ErrorCode::RecoveryRequired, "health check failed");
        crate::controller::record_failure(&store, &mut j, &failure)
            .expect("record restoration failure");
        assert_eq!(
            store.load(&id).expect("retained journal").phase,
            Phase::Restoring
        );
        assert_eq!(j.start_permit_expires, None);
        assert_eq!(j.detail.as_deref(), Some("health check failed"));
        j.phase = Phase::Prepared;
        crate::controller::record_failure(&store, &mut j, &failure)
            .expect("record preparation failure");
        assert_eq!(
            store.load(&id).expect("cancelled journal").phase,
            Phase::Cancelled
        );
        drop(store);
        std::fs::remove_dir_all(scratch).expect("cleanup");
    }
}
