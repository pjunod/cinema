//! Signed optional migration code, published outside the default runtime cohort.
//! The public loader accepts only the real installed record and retains a kernel
//! cleanup lease. The signed capsule authenticates code; it conveys no data-root
//! authority and the helper must still validate its real source under its lock.
use crate::{
    installation::{self, Architecture, InstallMethod, InstallationV1},
    protocol::ErrorCode,
    secure_fs::{self, SecureDir},
    transaction::TransactionStore,
    update::{
        self, AcceptedReleasesV1, MigrationBuildV1, MigrationHelperPurpose, MigrationHelperV1,
        MigrationSourceCohortV1, PublicReleaseClient, VerifiedManifest,
    },
    Error, Result,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::File,
    io::{BufReader, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::{Path, PathBuf},
};

const STORE: &str = "migration-helpers";
const GENERATIONS: &str = "generations";
const CURRENT: &str = "current.json";
const ACCEPTANCE: &str = "accepted-ledger.json";
const LEASE: &str = "cleanup.lease";
const EXECUTABLE: &str = "plurx-storage-migrate";
const MAX_EXECUTABLE: u64 = 2 * 1024 * 1024 * 1024;

fn refused(message: &str) -> Error {
    Error::new(ErrorCode::UntrustedUpdate, message)
}
fn canonical_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn sync_dir(dir: &SecureDir) -> Result<()> {
    // SAFETY: duplicate a live owned descriptor, then transfer ownership once.
    let fd = unsafe { libc::fcntl(dir.fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    unsafe { File::from_raw_fd(fd) }.sync_all()?;
    Ok(())
}
fn mode(file: &File, permissions: u32) -> Result<()> {
    // SAFETY: the descriptor is owned and the mode is a permission constant.
    if unsafe { libc::fchmod(file.as_raw_fd(), permissions as libc::mode_t) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    file.sync_all()?;
    Ok(())
}
fn dir_mode(dir: &SecureDir, permissions: u32) -> Result<()> {
    // SAFETY: the directory is held, never resolved through a mutable path.
    if unsafe { libc::fchmod(dir.fd(), permissions as libc::mode_t) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    sync_dir(dir)
}
fn readonly(file: &File, owner: u32) -> Result<()> {
    let m = file.metadata()?;
    if m.uid() != owner || !m.is_file() || m.nlink() != 1 || m.mode() & 0o222 != 0 {
        return Err(refused(
            "migration generation file is not owner-bound immutable regular data",
        ));
    }
    Ok(())
}
fn digest_file(file: &mut File, limit: u64) -> Result<String> {
    if file.metadata()?.len() > limit {
        return Err(refused("migration generation file exceeds bound"));
    }
    let mut hash = Sha256::new();
    let mut count = 0u64;
    let mut buf = [0; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        count = count
            .checked_add(n as u64)
            .ok_or_else(|| refused("migration file length overflow"))?;
        if count > limit {
            return Err(refused("migration generation file grew beyond bound"));
        }
        hash.update(&buf[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptedCapsule {
    version: u32,
    installation_id: String,
    ledger: AcceptedReleasesV1,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    version: u32,
    installation_id: String,
    generation_id: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepared {
    version: u32,
    installation_id: String,
    generation_id: String,
    manifest_digest: String,
}

/// Retain this (normally in Arc) until the actual helper and its descendants
/// have been reaped. No Drop implementation deletes a qualified generation.
#[derive(Debug)]
pub struct QualifiedGeneration {
    directory: SecureDir,
    lease: File,
    executable_file: File,
    executable_path: PathBuf,
    generation_id: String,
    installation_id: String,
    manifest_digest: String,
    key_id: String,
    metadata: MigrationHelperV1,
}
impl QualifiedGeneration {
    pub fn executable(&self) -> &Path {
        &self.executable_path
    }
    pub fn metadata(&self) -> &MigrationHelperV1 {
        &self.metadata
    }
    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }
    pub fn installation_id(&self) -> &str {
        &self.installation_id
    }
    pub fn manifest_digest(&self) -> &str {
        &self.manifest_digest
    }
    pub fn key_id(&self) -> &str {
        &self.key_id
    }
    /// CLOEXEC remains set in the parent. The process supervisor may clear it
    /// only in the exact helper child before exec, preserving parent-death safety.
    pub fn cleanup_lease_fd(&self) -> i32 {
        self.lease.as_raw_fd()
    }
    pub fn revalidate(&self) -> Result<()> {
        let parent = self
            .executable_path
            .parent()
            .ok_or_else(|| refused("missing generation parent"))?;
        let reopened = secure_fs::open_dir(parent, self.directory.owner())?;
        let held = fd_metadata(&self.directory)?;
        let current = fd_metadata(&reopened)?;
        if held.dev() != current.dev() || held.ino() != current.ino() {
            return Err(refused("migration generation path identity changed"));
        }
        let mut exe = self.directory.open(EXECUTABLE)?;
        readonly(&exe, self.directory.owner())?;
        let actual = exe.metadata()?;
        let held = self.executable_file.metadata()?;
        if actual.dev() != held.dev()
            || actual.ino() != held.ino()
            || actual.mode() & 0o111 == 0
            || digest_file(&mut exe, MAX_EXECUTABLE)? != self.metadata.executable_sha256
        {
            return Err(refused("migration executable identity or digest changed"));
        }
        Ok(())
    }
}
fn fd_metadata(dir: &SecureDir) -> Result<std::fs::Metadata> {
    // SAFETY: duplicate a held directory descriptor and transfer ownership once.
    let fd = unsafe { libc::fcntl(dir.fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { File::from_raw_fd(fd) }.metadata()?)
}
fn generation_id(
    i: &InstallationV1,
    verified: &VerifiedManifest,
    helper: &MigrationHelperV1,
) -> Result<String> {
    let mut hash = Sha256::new();
    hash.update(b"plurx.migration.helper.generation.v1\0");
    hash.update(i.installation_id.as_bytes());
    hash.update([0]);
    hash.update(verified.digest().as_bytes());
    hash.update([0]);
    hash.update(serde_json::to_vec(helper)?);
    Ok(hex::encode(hash.finalize()))
}
fn target(i: &InstallationV1) -> Result<&'static str> {
    let arch = installation::native_architecture()
        .ok_or_else(|| refused("unsupported helper host architecture"))?;
    if i.architecture != arch
        && !(i.method == InstallMethod::MacosNative && i.architecture == Architecture::Universal)
    {
        return Err(refused("helper host and installation architecture differ"));
    }
    match (i.method, &arch) {
        (InstallMethod::LinuxDeb | InstallMethod::LinuxRpm, Architecture::Amd64) => {
            Ok("x86_64-unknown-linux-gnu")
        }
        (InstallMethod::LinuxDeb | InstallMethod::LinuxRpm, Architecture::Arm64) => {
            Ok("aarch64-unknown-linux-gnu")
        }
        (InstallMethod::MacosNative, Architecture::Amd64) => Ok("x86_64-apple-darwin"),
        (InstallMethod::MacosNative, Architecture::Arm64) => Ok("aarch64-apple-darwin"),
        _ => Err(refused("unsupported migration helper installation")),
    }
}
fn helper_for<'a>(
    verified: &'a VerifiedManifest,
    i: &InstallationV1,
    daemon: &MigrationBuildV1,
    purpose: MigrationHelperPurpose,
) -> Result<update::VerifiedMigrationHelper<'a>> {
    let entry = verified
        .migration_helpers()
        .iter()
        .find(|h| h.daemon_build == *daemon)
        .ok_or_else(|| refused("signed release has no matching migration helper"))?;
    let cohort = entry
        .source_cohorts
        .first()
        .ok_or_else(|| refused("helper source cohorts absent"))?;
    update::select_migration_helper(verified, i, purpose, daemon, cohort)
}
fn load_from(
    i: &InstallationV1,
    base: &SecureDir,
    id: &str,
    daemon: &MigrationBuildV1,
    cohorts: &[MigrationSourceCohortV1],
    keys: &[update::TrustKey],
    now: u64,
) -> Result<QualifiedGeneration> {
    if !canonical_id(id) {
        return Err(refused("invalid helper generation identity"));
    }
    let generations = base.child(GENERATIONS, false)?;
    for parent in [base, &generations] {
        let metadata = fd_metadata(parent)?;
        if metadata.uid() != i.owner_uid
            || metadata.mode() & 0o022 != 0
            || (i.owner_uid == i.service_uid && metadata.mode() & 0o222 != 0)
        {
            return Err(refused(
                "helper parent is writable by the daemon or another authority",
            ));
        }
    }
    let directory = generations.child(id, false)?;
    if fd_metadata(&directory)?.mode() & 0o222 != 0 {
        return Err(refused("migration generation directory is writable"));
    }
    let lease = directory.open(LEASE)?;
    readonly(&lease, i.owner_uid)?;
    // SAFETY: read-only descriptor shared by all users of this immutable generation.
    if unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } != 0 {
        return Err(Error::new(
            ErrorCode::Busy,
            "migration generation cleanup is active",
        ));
    }
    for name in [update::MANIFEST_NAME, update::SIGNATURE_NAME, ACCEPTANCE] {
        readonly(&directory.open(name)?, i.owner_uid)?;
    }
    let bytes = directory.read(update::MANIFEST_NAME, update::MAX_MANIFEST as u64)?;
    let signature = directory.read(update::SIGNATURE_NAME, 64)?;
    let capsule: AcceptedCapsule = directory.json(ACCEPTANCE)?;
    if capsule.version != 1 || capsule.installation_id != i.installation_id {
        return Err(refused(
            "accepted helper capsule belongs to another installation",
        ));
    }
    let verified = update::verify_accepted_installed_manifest(
        &bytes,
        &signature,
        keys,
        i,
        &capsule.ledger,
        now,
        daemon,
    )?;
    let selected = helper_for(
        &verified,
        i,
        daemon,
        MigrationHelperPurpose::LegacyDataUpgrade,
    )?;
    if selected.binding().source_cohorts != cohorts
        || generation_id(i, &verified, selected.binding())? != id
    {
        return Err(refused("helper generation/cohort binding differs"));
    }
    let artifact = selected.artifact();
    let mut bundle = directory.open(&artifact.name)?;
    readonly(&bundle, i.owner_uid)?;
    if bundle.metadata()?.len() != artifact.size
        || digest_file(&mut bundle, artifact.size)? != artifact.sha256
    {
        return Err(refused("immutable helper bundle changed"));
    }
    let mut executable_file = directory.open(EXECUTABLE)?;
    readonly(&executable_file, i.owner_uid)?;
    if executable_file.metadata()?.mode() & 0o111 == 0
        || digest_file(&mut executable_file, MAX_EXECUTABLE)? != selected.executable_sha256()
    {
        return Err(refused("immutable helper executable changed"));
    }
    let result = QualifiedGeneration {
        directory,
        lease,
        executable_file,
        executable_path: i
            .paths
            .runtime
            .join(STORE)
            .join(GENERATIONS)
            .join(id)
            .join(EXECUTABLE),
        generation_id: id.into(),
        installation_id: i.installation_id.clone(),
        manifest_digest: verified.digest().into(),
        key_id: verified.key_id().into(),
        metadata: selected.binding().clone(),
    };
    result.revalidate()?;
    Ok(result)
}

/// Read only: the unprivileged daemon never takes the privileged controller's
/// transaction lock or trusts a pathname/boolean supplied by a caller.
pub fn load_qualified(
    daemon: &MigrationBuildV1,
    cohorts: &[MigrationSourceCohortV1],
) -> Result<QualifiedGeneration> {
    let i = InstallationV1::load(&installation::default_record()?)?;
    i.managed()?;
    if installation::native_uid() != i.service_uid {
        return Err(Error::new(
            ErrorCode::AuthorizationDenied,
            "helper loading requires the exact installed service UID",
        ));
    }
    let base = secure_fs::open_dir(&i.paths.runtime.join(STORE), i.owner_uid)?;
    readonly(&base.open(CURRENT)?, i.owner_uid)?;
    let selection: Selection = base.json(CURRENT)?;
    if selection.version != 1 || selection.installation_id != i.installation_id {
        return Err(refused("helper selection belongs to another installation"));
    }
    load_from(
        &i,
        &base,
        &selection.generation_id,
        daemon,
        cohorts,
        &update::production_keys()?,
        crate::unix_time()?,
    )
}

fn same_directory(a: &SecureDir, b: &SecureDir) -> Result<bool> {
    let a = fd_metadata(a)?;
    let b = fd_metadata(b)?;
    Ok(a.dev() == b.dev() && a.ino() == b.ino())
}
fn owner(i: &InstallationV1, store: &TransactionStore) -> Result<()> {
    i.validate()?;
    crate::controller::require_owner(i)?;
    let actual = InstallationV1::load(&installation::default_record()?)?;
    if serde_json::to_vec(&actual)? != serde_json::to_vec(i)?
        || !same_directory(&i.state_dir()?, &store.root)?
    {
        return Err(refused(
            "helper publisher is not bound to the actual installation",
        ));
    }
    Ok(())
}
struct Bounded<R> {
    inner: R,
    left: u64,
}
impl<R: Read> Read for Bounded<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "helper archive expansion exceeds bound",
            ));
        }
        let allowed = buf.len().min(self.left as usize);
        let n = self.inner.read(&mut buf[..allowed])?;
        self.left -= n as u64;
        Ok(n)
    }
}
fn extract_executable(bundle: File, dir: &SecureDir, expected: &str) -> Result<()> {
    let mut decoder = flate2::bufread::GzDecoder::new(BufReader::new(bundle));
    {
        let mut bounded = Bounded {
            inner: &mut decoder,
            left: MAX_EXECUTABLE + 1024 * 1024,
        };
        {
            let mut archive = tar::Archive::new(&mut bounded);
            archive.set_ignore_zeros(true);
            let mut seen = false;
            for entry in archive.entries()?.raw(true) {
                let mut entry = entry?;
                if seen
                    || !entry.header().entry_type().is_file()
                    || entry.path()?.as_ref() != Path::new(EXECUTABLE)
                    || entry.size() == 0
                    || entry.size() > MAX_EXECUTABLE
                    || entry.header().mode()? & 0o7000 != 0
                    || entry.header().mode()? & 0o111 == 0
                {
                    return Err(refused(
                        "helper archive is not the closed single-executable profile",
                    ));
                }
                seen = true;
                let name =
                    CString::new(EXECUTABLE).map_err(|_| refused("helper executable name"))?;
                // SAFETY: fresh private staging directory, exact fixed name, exclusive nofollow file.
                let fd = unsafe {
                    libc::openat(
                        dir.fd(),
                        name.as_ptr(),
                        libc::O_WRONLY
                            | libc::O_CREAT
                            | libc::O_EXCL
                            | libc::O_NOFOLLOW
                            | libc::O_CLOEXEC,
                        0o600,
                    )
                };
                if fd < 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                let mut file = unsafe { File::from_raw_fd(fd) };
                let mut hash = Sha256::new();
                let mut total = 0u64;
                let mut buf = [0; 65536];
                loop {
                    let n = entry.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    total += n as u64;
                    hash.update(&buf[..n]);
                    file.write_all(&buf[..n])?;
                }
                if total != entry.size() || hex::encode(hash.finalize()) != expected {
                    return Err(refused("helper executable size/hash mismatch"));
                }
                mode(&file, 0o555)?;
            }
            if !seen {
                return Err(refused("helper archive has no executable"));
            }
        }
        std::io::copy(&mut bounded, &mut std::io::sink())?;
    }
    let mut trailing = decoder.into_inner();
    let mut byte = [0];
    if trailing.read(&mut byte)? != 0 {
        return Err(refused("trailing helper gzip members or bytes"));
    }
    sync_dir(dir)
}

struct RestoreReadonly<'a>(&'a SecureDir);
impl Drop for RestoreReadonly<'_> {
    fn drop(&mut self) {
        let _ = dir_mode(self.0, 0o555);
    }
}
fn prospective_installation(i: &InstallationV1, candidate: &VerifiedManifest) -> InstallationV1 {
    let mut prospective = i.clone();
    prospective.current_version = candidate.manifest().version.clone();
    prospective.current_tag = candidate.manifest().tag.clone();
    prospective.current_commit = candidate.manifest().source_commit.clone();
    prospective
}
struct Publication<'a> {
    bytes: &'a [u8],
    signature: &'a [u8],
    ledger: AcceptedReleasesV1,
    keys: &'a [update::TrustKey],
    now: u64,
}
fn publish(
    i: &InstallationV1,
    base: &SecureDir,
    verified: &VerifiedManifest,
    selected: &update::VerifiedMigrationHelper<'_>,
    reader: &mut impl Read,
    publication: Publication<'_>,
) -> Result<String> {
    let Publication {
        bytes,
        signature,
        ledger,
        keys,
        now,
    } = publication;
    let id = generation_id(i, verified, selected.binding())?;
    let generations = match base.child(GENERATIONS, false) {
        Ok(dir) => dir,
        Err(e) if e.os_error == Some(libc::ENOENT) => {
            dir_mode(base, 0o755)?;
            let restore = RestoreReadonly(base);
            let dir = base.child(GENERATIONS, true)?;
            dir_mode(&dir, 0o555)?;
            drop(restore);
            dir
        }
        Err(e) => return Err(e),
    };
    // Reuse is authenticated exactly like a service load; a pre-existing name
    // never permits overwrite, repair or acceptance of changed bytes.
    match generations.child(&id, false) {
        Ok(_) => {
            let prospective = prospective_installation(i, verified);
            let _lease = load_from(
                &prospective,
                base,
                &id,
                selected.daemon_build(),
                &selected.binding().source_cohorts,
                keys,
                now,
            )?;
            return Ok(id);
        }
        Err(e) if e.os_error == Some(libc::ENOENT) => (),
        Err(e) => return Err(e),
    }
    dir_mode(base, 0o755)?;
    let restore_base = RestoreReadonly(base);
    dir_mode(&generations, 0o755)?;
    let restore_generations = RestoreReadonly(&generations);
    let incoming = format!("incoming-{}", uuid::Uuid::new_v4());
    let dir = generations.child(&incoming, true)?;
    dir.write(update::MANIFEST_NAME, bytes, 0o444)?;
    dir.write(update::SIGNATURE_NAME, signature, 0o444)?;
    dir.write_json(
        ACCEPTANCE,
        &AcceptedCapsule {
            version: 1,
            installation_id: i.installation_id.clone(),
            ledger,
        },
    )?;
    mode(&dir.open(ACCEPTANCE)?, 0o444)?;
    dir.write(LEASE, b"", 0o444)?;
    let a = selected.artifact();
    update::stage_bounded_artifact(reader, &a.name, a.size, &a.sha256, &dir)?;
    mode(&dir.open(&a.name)?, 0o444)?;
    extract_executable(dir.open(&a.name)?, &dir, selected.executable_sha256())?;
    dir_mode(&dir, 0o555)?;
    let old = CString::new(incoming).map_err(|_| refused("incoming helper name"))?;
    let new = CString::new(id.as_str()).map_err(|_| refused("generation name"))?;
    rename_new_directory(&generations, &old, &new)?;
    sync_dir(&generations)?;
    dir_mode(&generations, 0o555)?;
    dir_mode(base, 0o555)?;
    drop(restore_generations);
    drop(restore_base);
    let prospective = prospective_installation(i, verified);
    let _lease = load_from(
        &prospective,
        base,
        &id,
        selected.daemon_build(),
        &selected.binding().source_cohorts,
        keys,
        now,
    )?;
    Ok(id)
}

/// An existing operation pins its original daemon/helper tuple. Never replace
/// installation code or the selected helper while that operation is pending.
pub(crate) fn require_no_pending_migration(i: &InstallationV1) -> Result<()> {
    let data = match secure_fs::open_dir(&i.paths.data, i.service_uid) {
        Ok(d) => d,
        Err(e) if e.os_error == Some(libc::ENOENT) => return Ok(()),
        Err(e) => return Err(e),
    };
    let name =
        CString::new("storage-migration.json").map_err(|_| refused("migration intent name"))?;
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: held owner-bound data directory and exact nofollow leaf. Any
    // object, including a malformed file or symlink, must preserve the fence.
    if unsafe {
        libc::fstatat(
            data.fd(),
            name.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        return Err(Error::new(ErrorCode::RecoveryRequired,"pending storage migration pins installed code and helper; finish or explicitly recover it before updating"));
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOENT) {
        Ok(())
    } else {
        Err(error.into())
    }
}

/// Descriptive trigger only. It cannot select a database or authorize writes.
fn legacy_present(i: &InstallationV1) -> Result<bool> {
    let data = match secure_fs::open_dir(&i.paths.data, i.service_uid) {
        Ok(d) => d,
        Err(e) if e.os_error == Some(libc::ENOENT) => return Ok(false),
        Err(e) => return Err(e),
    };
    let mut legacy = false;
    for name in ["hiqlite", "hiqlite-activated.json"] {
        let name = CString::new(name).map_err(|_| refused("legacy name"))?;
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: exact known leaf, held data root and caller-owned stat buffer.
        if unsafe {
            libc::fstatat(
                data.fd(),
                name.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            if stat.st_mode & libc::S_IFMT == libc::S_IFLNK {
                return Err(refused("legacy detection encountered a symlink"));
            }
            legacy = true;
        } else if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOENT) {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    if !legacy {
        return Ok(false);
    }
    match data.read("storage-mode.json", 65536) {
        Ok(bytes) => {
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            Ok(value.get("mode").and_then(|m| m.as_str()) != Some("sqlite"))
        }
        Err(e) if e.os_error == Some(libc::ENOENT) => Ok(true),
        Err(e) => Err(e),
    }
}

fn helper_base(i: &InstallationV1) -> Result<SecureDir> {
    let runtime = match secure_fs::open_dir(&i.paths.runtime, i.owner_uid) {
        Ok(dir) => dir,
        Err(e) if e.os_error == Some(libc::ENOENT) => {
            let parent = i
                .paths
                .runtime
                .parent()
                .ok_or_else(|| refused("runtime has no parent"))?;
            let name = i
                .paths
                .runtime
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| refused("runtime name"))?;
            let parent = secure_fs::open_dir(parent, i.owner_uid)?;
            let dir = parent.child(name, true)?;
            if i.owner_uid != i.service_uid {
                dir_mode(&dir, 0o755)?;
            }
            dir
        }
        Err(e) => return Err(e),
    };
    runtime.child(STORE, true)
}

pub(crate) fn prepare_legacy_update(
    i: &InstallationV1,
    store: &TransactionStore,
    transaction: &SecureDir,
    candidate: &VerifiedManifest,
    bytes: &[u8],
    signature: &[u8],
    client: &PublicReleaseClient,
) -> Result<()> {
    owner(i, store)?;
    require_no_pending_migration(i)?;
    if !legacy_present(i)? {
        return Ok(());
    }
    let daemon = MigrationBuildV1 {
        commit: candidate.manifest().source_commit.clone(),
        tree: candidate.manifest().source_tree.clone(),
        target: target(i)?.into(),
    };
    let selected = helper_for(
        candidate,
        i,
        &daemon,
        MigrationHelperPurpose::LegacyDataUpgrade,
    )?;
    // This is the real existing acceptance ledger, durably advanced by the
    // controller after signature/cohort verification, never a ready sidecar.
    store.accept(candidate)?;
    let ledger = store.ledger()?;
    let base = helper_base(i)?;
    let mut response = client.helper_response(&selected)?;
    let keys = update::production_keys()?;
    let id = publish(
        i,
        &base,
        candidate,
        &selected,
        &mut response,
        Publication {
            bytes,
            signature,
            ledger,
            keys: &keys,
            now: crate::unix_time()?,
        },
    )?;
    transaction.write_json(
        "migration-helper.json",
        &Prepared {
            version: 1,
            installation_id: i.installation_id.clone(),
            generation_id: id,
            manifest_digest: candidate.digest().into(),
        },
    )
}

pub(crate) fn activate_prepared(
    i: &InstallationV1,
    store: &TransactionStore,
    transaction: &SecureDir,
    candidate: &VerifiedManifest,
) -> Result<()> {
    owner(i, store)?;
    require_no_pending_migration(i)?;
    let prepared: Prepared = match transaction.json("migration-helper.json") {
        Ok(p) => p,
        Err(e) if e.os_error == Some(libc::ENOENT) => return Ok(()),
        Err(e) => return Err(e),
    };
    if prepared.version != 1
        || prepared.installation_id != i.installation_id
        || prepared.manifest_digest != candidate.digest()
    {
        return Err(refused(
            "prepared helper does not match this installed transaction",
        ));
    }
    let daemon = MigrationBuildV1 {
        commit: i.current_commit.clone(),
        tree: candidate.manifest().source_tree.clone(),
        target: target(i)?.into(),
    };
    let base = secure_fs::open_dir(&i.paths.runtime.join(STORE), i.owner_uid)?;
    let selected = helper_for(
        candidate,
        i,
        &daemon,
        MigrationHelperPurpose::LegacyDataUpgrade,
    )?;
    let _lease = load_from(
        i,
        &base,
        &prepared.generation_id,
        &daemon,
        &selected.binding().source_cohorts,
        &update::production_keys()?,
        crate::unix_time()?,
    )?;
    dir_mode(&base, 0o755)?;
    let restore = RestoreReadonly(&base);
    let result = base.write_json(
        CURRENT,
        &Selection {
            version: 1,
            installation_id: i.installation_id.clone(),
            generation_id: prepared.generation_id,
        },
    );
    if result.is_ok() {
        mode(&base.open(CURRENT)?, 0o444)?;
    }
    dir_mode(&base, 0o555)?;
    drop(restore);
    result
}

fn rename_new_directory(parent: &SecureDir, old: &CString, new: &CString) -> Result<()> {
    #[cfg(target_os = "linux")]
    // SAFETY: validated private same-parent names and no-replacement flag.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            parent.fd(),
            old.as_ptr(),
            parent.fd(),
            new.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    // SAFETY: validated private same-parent names and no-replacement flag.
    let result = unsafe {
        libc::renameatx_np(
            parent.fd(),
            old.as_ptr(),
            parent.fd(),
            new.as_ptr(),
            libc::RENAME_EXCL,
        )
    } as libc::c_long;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return Err(Error::new(
        ErrorCode::Unsupported,
        "immutable helper publication is unsupported on this platform",
    ));
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

/// Exclusive cleanup capability only. Current and active-transaction code is
/// retained. A cleaner must keep this guard until its descriptor-relative
/// cleanup is complete; dropping it does not delete anything.
#[derive(Debug)]
pub struct CleanupLease {
    _lease: File,
    _directory: SecureDir,
    path: PathBuf,
}
impl CleanupLease {
    pub fn directory(&self) -> &Path {
        &self.path
    }
}
pub fn try_cleanup_lease(id: &str) -> Result<CleanupLease> {
    let i = InstallationV1::load(&installation::default_record()?)?;
    crate::controller::require_owner(&i)?;
    let store = TransactionStore::locked(&i)?;
    let base = secure_fs::open_dir(&i.paths.runtime.join(STORE), i.owner_uid)?;
    let current: Option<Selection> = match base.json(CURRENT) {
        Ok(v) => Some(v),
        Err(e) if e.os_error == Some(libc::ENOENT) => None,
        Err(e) => return Err(e),
    };
    if current.is_some_and(|c| c.generation_id == id) {
        return Err(Error::new(
            ErrorCode::Busy,
            "current migration helper generation is retained",
        ));
    }
    if let Some(active) = store.active()? {
        let transaction = store.dir(&active.transaction_id)?;
        let pending: Option<Prepared> = match transaction.json("migration-helper.json") {
            Ok(v) => Some(v),
            Err(e) if e.os_error == Some(libc::ENOENT) => None,
            Err(e) => return Err(e),
        };
        if pending.is_some_and(|p| p.generation_id == id) {
            return Err(Error::new(
                ErrorCode::Busy,
                "pending migration helper generation is retained",
            ));
        }
    }
    cleanup_lease(&i, &base, id)
}
fn cleanup_lease(i: &InstallationV1, base: &SecureDir, id: &str) -> Result<CleanupLease> {
    if !canonical_id(id) {
        return Err(refused("invalid cleanup generation identity"));
    }
    let directory = base.child(GENERATIONS, false)?.child(id, false)?;
    let lease = directory.open(LEASE)?;
    readonly(&lease, i.owner_uid)?;
    // SAFETY: this distinct read-only descriptor excludes every shared user.
    if unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::new(
            ErrorCode::Busy,
            "migration helper is still leased by a process",
        ));
    }
    Ok(CleanupLease {
        _lease: lease,
        _directory: directory,
        path: i.paths.runtime.join(STORE).join(GENERATIONS).join(id),
    })
}

#[cfg(test)]
mod tests;

/// Explicit managed-install operation. It reuses the actual persisted accepted
/// signed manifest for this installed release; it does not discover a helper
/// from PATH, accept unsigned hashes, or turn an offered update into an install.
/// Source/external installations use the separate operator bootstrap workflow.
pub fn stage_explicit(daemon: &MigrationBuildV1) -> Result<String> {
    let i = InstallationV1::load(&installation::default_record()?)?;
    crate::controller::require_owner(&i)?;
    let store = TransactionStore::locked(&i)?;
    owner(&i, &store)?;
    let _launch = crate::launch_lock::acquire(&i, true)?;
    if !crate::supervisor::observe(&i)?.stopped() {
        return Err(Error::new(
            ErrorCode::PolicyConflict,
            "stop the exact installed daemon before staging an explicit migration helper",
        ));
    }
    require_no_pending_migration(&i)?;
    if store.active()?.is_some() {
        return Err(Error::new(
            ErrorCode::Busy,
            "an installation transaction is active",
        ));
    }
    let transaction=i.last_transaction_id.as_deref().ok_or_else(||refused("installed release has no retained accepted signed manifest; explicit trusted installation bootstrap is required"))?;
    let dir = store.dir(transaction)?;
    let bytes = dir.read(update::MANIFEST_NAME, update::MAX_MANIFEST as u64)?;
    let signature = dir.read(update::SIGNATURE_NAME, 64)?;
    let ledger = store.ledger()?;
    let keys = update::production_keys()?;
    let now = crate::unix_time()?;
    let verified = update::verify_accepted_installed_manifest(
        &bytes, &signature, &keys, &i, &ledger, now, daemon,
    )?;
    let selected = helper_for(
        &verified,
        &i,
        daemon,
        MigrationHelperPurpose::ExplicitTransition,
    )?;
    let base = helper_base(&i)?;
    let mut response = PublicReleaseClient::new()?.helper_response(&selected)?;
    let id = publish(
        &i,
        &base,
        &verified,
        &selected,
        &mut response,
        Publication {
            bytes: &bytes,
            signature: &signature,
            ledger,
            keys: &keys,
            now,
        },
    )?;
    // The actual data root may have gained an intent during the network fetch.
    // Never replace an operation's helper selection after that point.
    require_no_pending_migration(&i)?;
    let _lease = load_from(
        &i,
        &base,
        &id,
        daemon,
        &selected.binding().source_cohorts,
        &keys,
        now,
    )?;
    dir_mode(&base, 0o755)?;
    let restore = RestoreReadonly(&base);
    base.write_json(
        CURRENT,
        &Selection {
            version: 1,
            installation_id: i.installation_id.clone(),
            generation_id: id.clone(),
        },
    )?;
    mode(&base.open(CURRENT)?, 0o444)?;
    dir_mode(&base, 0o555)?;
    drop(restore);
    Ok(id)
}
