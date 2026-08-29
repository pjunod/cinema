//! Offline, read-only WAL diagnostics and additive evidence backup.
//!
//! No command in this module edits the source data directory. Recovery plans
//! name a next action, but applying one remains a separate adversarially
//! reviewed milestone.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use clap::Subcommand;
use hiqlite_wal::inspection::{inspect_lock, inspect_logs_dir, WalInspection, WalLockState};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use plurx_core::config::Config;

const MAX_BACKUP_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_BACKUP_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Subcommand)]
pub(crate) enum WalCommand {
    /// Inspect local lock ownership, metadata, and WAL boundaries without opening a store.
    Status {
        /// Authoritative Plurx data directory; defaults to the loaded config.
        #[arg(long)]
        data_dir: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Verify stopped-node metadata/header/record CRC and retained-index invariants.
    Verify {
        #[arg(long)]
        data_dir: PathBuf,
        /// Retained for the operator contract; inspection already verifies every bounded record.
        #[arg(long)]
        deep: bool,
    },
    /// Additively copy stopped-node WAL evidence and a checksum manifest.
    Backup {
        #[arg(long)]
        data_dir: PathBuf,
        /// New directory. Existing paths are never overwritten.
        #[arg(long)]
        output: PathBuf,
    },
    /// Produce a machine-readable, fingerprint-bound next-action plan.
    RecoveryPlan {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum RecoveryAction {
    NoAction,
    RestartNormally,
    BackupThenRemoveStaleUnlockedSentinel,
    BackupThenRunExistingIntegrityRecovery,
    RestoreThisNodeFromAuthenticatedPeer,
    StopAndEscalateMajorityAtRisk,
}

#[derive(Debug, Serialize)]
struct WalEvidence {
    schema_version: u32,
    observed_at_unix_ms: u64,
    node_id: String,
    raft_id: u64,
    wal_root: String,
    lock: WalLockState,
    inspection: Option<WalInspection>,
    action: RecoveryAction,
    reasons: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RecoveryPlan {
    schema_version: u32,
    created_at_unix_ms: u64,
    expected_node_id: String,
    expected_raft_id: u64,
    source_root: String,
    lock: WalLockState,
    action: RecoveryAction,
    permitted_mutations: Vec<String>,
    preconditions: Vec<String>,
    source_fingerprints: Vec<FileFingerprint>,
    reasons: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct FileFingerprint {
    path: String,
    size_bytes: u64,
    sha256: String,
}

#[derive(Deserialize)]
struct LocalIdentity {
    node_id: String,
    raft_id: u64,
}

struct OfflinePaths {
    data: PathBuf,
    wal: PathBuf,
    identity: LocalIdentity,
}

pub(crate) async fn run(command: WalCommand, config: &Config) -> anyhow::Result<()> {
    match command {
        WalCommand::Status { data_dir, json } => {
            let root = data_dir.unwrap_or_else(|| config.storage.data_dir.clone());
            status(&root, json)
        }
        WalCommand::Verify { data_dir, deep } => verify(&data_dir, deep),
        WalCommand::Backup { data_dir, output } => backup(&data_dir, &output),
        WalCommand::RecoveryPlan { data_dir, json } => recovery_plan(&data_dir, json),
    }
}

fn status(data_dir: &Path, json: bool) -> anyhow::Result<()> {
    let paths = offline_paths(data_dir)?;
    let evidence = collect_evidence(&paths);
    if json {
        println!("{}", serde_json::to_string_pretty(&evidence)?);
    } else {
        print_evidence(&evidence);
    }
    if evidence.lock == WalLockState::Locked {
        return Err(crate::cli_exit(
            1,
            "the WAL is owned by a running process; stop plurxd before offline diagnostics",
        ));
    }
    if evidence.action == RecoveryAction::RestartNormally {
        Ok(())
    } else {
        Err(crate::cli_exit(
            1,
            "WAL status was collected, but operator action or escalation is required",
        ))
    }
}

fn verify(data_dir: &Path, deep: bool) -> anyhow::Result<()> {
    let paths = offline_paths(data_dir)?;
    require_stopped(&paths.wal)?;
    let inspection = inspect_logs_dir(&paths.wal)
        .map_err(|error| crate::cli_exit(1, format!("WAL verification failed: {error}")))?;
    println!(
        "verified node {} (Raft {}){}",
        paths.identity.node_id,
        paths.identity.raft_id,
        if deep {
            " with full bounded record CRC inspection"
        } else {
            ""
        }
    );
    println!("verdicts: {}", inspection.invariant_verdicts.join(", "));
    if inspection.invariant_verdicts == ["clean"] {
        Ok(())
    } else {
        Err(crate::cli_exit(
            1,
            "WAL verification completed with invariant failures",
        ))
    }
}

fn backup(data_dir: &Path, output: &Path) -> anyhow::Result<()> {
    let paths = offline_paths(data_dir)?;
    require_stopped(&paths.wal)?;
    let output = validated_new_backup_path(&paths.data, output)?;
    let source_files = source_files(&paths)?;
    let mut required_bytes = 1024 * 1024_u64;
    for (source, _) in &source_files {
        let (_, size_bytes) = read_bounded_regular_file(source, MAX_BACKUP_FILE_BYTES)?;
        required_bytes = required_bytes.saturating_add(size_bytes);
    }
    anyhow::ensure!(
        required_bytes <= MAX_BACKUP_TOTAL_BYTES,
        "backup source exceeds the total byte bound"
    );
    let backup_parent = output.parent().expect("validated backup has a parent");
    let available = available_space_bytes(backup_parent)?;
    anyhow::ensure!(
        available >= required_bytes,
        "insufficient free space for an additive backup: need {required_bytes} bytes, have {available}"
    );
    create_private_directory(&output)?;

    let mut fingerprints = Vec::with_capacity(source_files.len());
    let mut total = 0_u64;
    for (source, relative) in source_files {
        let (bytes, size_bytes) = read_bounded_regular_file(&source, MAX_BACKUP_FILE_BYTES)
            .with_context(|| format!("reading backup source {}", source.display()))?;
        total = total.saturating_add(size_bytes);
        anyhow::ensure!(
            total <= MAX_BACKUP_TOTAL_BYTES,
            "backup source exceeds the total byte bound"
        );
        let target = output.join(&relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_new_private_file(&target, &bytes)?;
        fingerprints.push(FileFingerprint {
            path: path_label(&relative)?,
            size_bytes,
            sha256: hex::encode(Sha256::digest(&bytes)),
        });
    }
    let manifest = serde_json::to_vec_pretty(&serde_json::json!({
        "schema_version": 1,
        "created_at_unix_ms": unix_ms(),
        "expected_node_id": paths.identity.node_id,
        "expected_raft_id": paths.identity.raft_id,
        "source_root": paths.data,
        "files": fingerprints,
    }))?;
    write_new_private_file(&output.join("manifest.json"), &manifest)?;
    sync_directory(&output)?;
    println!("wrote additive WAL evidence backup {}", output.display());
    Ok(())
}

fn recovery_plan(data_dir: &Path, json: bool) -> anyhow::Result<()> {
    let paths = offline_paths(data_dir)?;
    let evidence = collect_evidence(&paths);
    // Never race file hashing against the live owner. The lock observation is
    // itself the complete plan evidence in this state.
    let fingerprints = if evidence.lock == WalLockState::Locked {
        Vec::new()
    } else {
        fingerprint_sources(&paths)?
    };
    let permitted_mutations = match evidence.action {
        RecoveryAction::BackupThenRemoveStaleUnlockedSentinel => {
            vec!["remove_stale_unlocked_lock_sentinel_after_backup".to_owned()]
        }
        RecoveryAction::BackupThenRunExistingIntegrityRecovery => {
            vec!["run_existing_hiqlite_integrity_recovery_after_backup".to_owned()]
        }
        _ => Vec::new(),
    };
    let plan = RecoveryPlan {
        schema_version: 1,
        created_at_unix_ms: unix_ms(),
        expected_node_id: paths.identity.node_id,
        expected_raft_id: paths.identity.raft_id,
        source_root: path_label(&paths.data)?,
        lock: evidence.lock,
        action: evidence.action,
        permitted_mutations,
        preconditions: vec![
            "re-prove the real WAL lock is not owned".to_owned(),
            "create and verify a timestamped additive backup".to_owned(),
            "re-read every source fingerprint immediately before any apply step".to_owned(),
            "confirm the expected stable node id".to_owned(),
        ],
        source_fingerprints: fingerprints,
        reasons: evidence.reasons,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&plan)?);
    } else {
        println!("action: {:?}", plan.action);
        for reason in &plan.reasons {
            println!("reason: {reason}");
        }
        println!(
            "This build intentionally has no wal apply-plan command; review the JSON plan before any separately reviewed mutation."
        );
    }
    if matches!(
        plan.action,
        RecoveryAction::NoAction | RecoveryAction::RestartNormally
    ) {
        Ok(())
    } else {
        Err(crate::cli_exit(
            1,
            "the recovery plan requires backup, recovery, or escalation",
        ))
    }
}

fn collect_evidence(paths: &OfflinePaths) -> WalEvidence {
    let lock = inspect_lock(&paths.wal).unwrap_or(WalLockState::Locked);
    let (inspection, action, reasons) = if lock == WalLockState::Locked {
        (
            None,
            RecoveryAction::NoAction,
            vec!["the real WAL advisory lock is owned; online status is authoritative".to_owned()],
        )
    } else {
        match inspect_logs_dir(&paths.wal) {
            Ok(inspection) => {
                let (action, reasons) = classify(&inspection, lock);
                (Some(inspection), action, reasons)
            }
            Err(error) => (
                None,
                RecoveryAction::StopAndEscalateMajorityAtRisk,
                vec![format!("bounded WAL inspection failed: {error}")],
            ),
        }
    };
    WalEvidence {
        schema_version: 1,
        observed_at_unix_ms: unix_ms(),
        node_id: paths.identity.node_id.clone(),
        raft_id: paths.identity.raft_id,
        wal_root: path_label(&paths.wal).unwrap_or_else(|_| "hiqlite/logs".to_owned()),
        lock,
        inspection,
        action,
        reasons,
    }
}

fn classify(inspection: &WalInspection, lock: WalLockState) -> (RecoveryAction, Vec<String>) {
    if inspection.invariant_verdicts == ["clean"] {
        if lock == WalLockState::UnlockedSentinel {
            return (
                RecoveryAction::BackupThenRemoveStaleUnlockedSentinel,
                vec!["WAL invariants are clean, but an unlocked crash sentinel remains".to_owned()],
            );
        }
        if inspection
            .observations
            .iter()
            .any(|value| value == "unexpected_valid_tail")
        {
            return (
                RecoveryAction::BackupThenRunExistingIntegrityRecovery,
                vec![
                    "a valid unexpected WAL tail is eligible only for existing recovery".to_owned(),
                ],
            );
        }
        return (
            RecoveryAction::RestartNormally,
            vec!["metadata, headers, record CRCs, and retained boundaries are clean".to_owned()],
        );
    }
    if inspection
        .invariant_verdicts
        .iter()
        .any(|value| value == "wal_missing")
        && inspection.metadata.crc_valid
    {
        return (
            RecoveryAction::RestoreThisNodeFromAuthenticatedPeer,
            vec!["metadata exists but this node has no retained WAL files".to_owned()],
        );
    }
    (
        RecoveryAction::StopAndEscalateMajorityAtRisk,
        vec![format!(
            "offline invariants failed: {}",
            inspection.invariant_verdicts.join(", ")
        )],
    )
}

fn print_evidence(evidence: &WalEvidence) {
    println!("node: {} (Raft {})", evidence.node_id, evidence.raft_id);
    println!("lock: {:?}", evidence.lock);
    if let Some(inspection) = &evidence.inspection {
        println!("segments: {}", inspection.wal_files.len());
        println!("verdicts: {}", inspection.invariant_verdicts.join(", "));
    }
    println!("action: {:?}", evidence.action);
    for reason in &evidence.reasons {
        println!("reason: {reason}");
    }
}

fn offline_paths(data_dir: &Path) -> anyhow::Result<OfflinePaths> {
    let metadata = std::fs::symlink_metadata(data_dir).map_err(|error| {
        crate::cli_exit(
            2,
            format!("opening data directory {}: {error}", data_dir.display()),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(crate::cli_exit(
            2,
            "--data-dir must be an existing real directory, not a symlink",
        ));
    }
    let data = std::fs::canonicalize(data_dir)?;
    let membership_path = data.join("membership.json");
    let membership_metadata = std::fs::symlink_metadata(&membership_path).map_err(|error| {
        crate::cli_exit(
            2,
            format!(
                "reading stable node identity {}: {error}",
                membership_path.display()
            ),
        )
    })?;
    if membership_metadata.file_type().is_symlink() || !membership_metadata.is_file() {
        return Err(crate::cli_exit(2, "membership.json must be a regular file"));
    }
    let identity = serde_json::from_slice::<LocalIdentity>(&std::fs::read(&membership_path)?)
        .map_err(|_| {
            crate::cli_exit(2, "membership.json does not contain a stable node identity")
        })?;
    if identity.node_id.trim().is_empty() || identity.raft_id == 0 {
        return Err(crate::cli_exit(
            2,
            "membership.json contains an invalid node identity",
        ));
    }
    let active = data.join("hiqlite");
    let wal = active.join("logs");
    for path in [&active, &wal] {
        let metadata = std::fs::symlink_metadata(path).map_err(|error| {
            crate::cli_exit(2, format!("opening WAL root {}: {error}", path.display()))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(crate::cli_exit(
                2,
                "the WAL root must not contain symlink directories",
            ));
        }
    }
    let wal = std::fs::canonicalize(wal)?;
    if !wal.starts_with(&data) {
        return Err(crate::cli_exit(
            2,
            "the WAL root escapes the configured data directory",
        ));
    }
    Ok(OfflinePaths {
        data,
        wal,
        identity,
    })
}

fn require_stopped(wal: &Path) -> anyhow::Result<WalLockState> {
    let lock = inspect_lock(wal)
        .map_err(|error| crate::cli_exit(1, format!("probing the real WAL lock: {error}")))?;
    if lock == WalLockState::Locked {
        return Err(crate::cli_exit(
            1,
            "the WAL lock is owned by a running process; stop plurxd first",
        ));
    }
    Ok(lock)
}

fn source_files(paths: &OfflinePaths) -> anyhow::Result<Vec<(PathBuf, PathBuf)>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&paths.wal)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .map(ToOwned::to_owned)
            .ok_or_else(|| crate::cli_exit(2, "WAL source has an invalid filename"))?;
        files.push((path, PathBuf::from("hiqlite/logs").join(name)));
    }
    for (source, relative) in [
        (
            paths.data.join("membership.json"),
            PathBuf::from("membership.json"),
        ),
        (
            paths.data.join("hiqlite/activation.json"),
            PathBuf::from("hiqlite/activation.json"),
        ),
    ] {
        if source.exists() {
            files.push((source, relative));
        }
    }
    files.sort_by(|left, right| left.1.cmp(&right.1));
    Ok(files)
}

fn fingerprint_sources(paths: &OfflinePaths) -> anyhow::Result<Vec<FileFingerprint>> {
    let mut fingerprints = Vec::new();
    let mut total = 0_u64;
    for (path, relative) in source_files(paths)? {
        let (bytes, size_bytes) = read_bounded_regular_file(&path, MAX_BACKUP_FILE_BYTES)?;
        total = total.saturating_add(size_bytes);
        anyhow::ensure!(total <= MAX_BACKUP_TOTAL_BYTES, "source set is too large");
        fingerprints.push(FileFingerprint {
            path: path_label(&relative)?,
            size_bytes,
            sha256: hex::encode(Sha256::digest(bytes)),
        });
    }
    Ok(fingerprints)
}

fn read_bounded_regular_file(path: &Path, max_bytes: u64) -> anyhow::Result<(Vec<u8>, u64)> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    anyhow::ensure!(metadata.is_file(), "source is not a regular file");
    anyhow::ensure!(metadata.len() <= max_bytes, "source file is too large");
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let size_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    anyhow::ensure!(size_bytes <= max_bytes, "source file is too large");
    Ok((bytes, size_bytes))
}

fn validated_new_backup_path(data: &Path, output: &Path) -> anyhow::Result<PathBuf> {
    let name = output
        .file_name()
        .ok_or_else(|| crate::cli_exit(2, "--output must name a new backup directory"))?;
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let parent = std::fs::canonicalize(parent).map_err(|error| {
        crate::cli_exit(
            2,
            format!("canonicalizing backup parent {}: {error}", parent.display()),
        )
    })?;
    let output = parent.join(name);
    if output.starts_with(data) || data.starts_with(&output) {
        return Err(crate::cli_exit(
            2,
            "the backup directory must be outside the authoritative data directory",
        ));
    }
    Ok(output)
}

fn create_private_directory(path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir(path).map_err(|error| {
        crate::cli_exit(
            2,
            format!(
                "refusing to overwrite backup directory {}: {error}",
                path.display()
            ),
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(unix)]
fn available_space_bytes(path: &Path) -> anyhow::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| crate::cli_exit(2, "backup parent contains an invalid NUL byte"))?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is a valid NUL-terminated string and `stats` points to
    // writable storage for one `statvfs` result.
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: statvfs returned success and initialized the output structure.
    let stats = unsafe { stats.assume_init() };
    let bytes = u128::from(stats.f_bavail).saturating_mul(u128::from(stats.f_frsize));
    Ok(u64::try_from(bytes).unwrap_or(u64::MAX))
}

#[cfg(not(unix))]
fn available_space_bytes(_path: &Path) -> anyhow::Result<u64> {
    // The supported deployment targets are Unix. Refuse to guess on another
    // platform until an equivalent real filesystem-space proof is wired.
    Err(crate::cli_exit(
        2,
        "WAL backup free-space proof is not implemented on this platform",
    ))
}

fn write_new_private_file(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn sync_directory(path: &Path) -> anyhow::Result<()> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

fn path_label(path: &Path) -> anyhow::Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| crate::cli_exit(2, "operator path is not valid UTF-8"))
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_stopped_wal_classifies_restart_or_stale_sentinel() {
        let clean = WalInspection {
            metadata: hiqlite_wal::inspection::MetadataInspection {
                present: true,
                format_version: Some(1),
                crc_valid: true,
                last_purged_log_id_present: false,
                last_purged_log_id: None,
                vote: None,
            },
            wal_files: Vec::new(),
            invariant_verdicts: vec!["clean".to_owned()],
            observations: Vec::new(),
        };
        assert_eq!(
            classify(&clean, WalLockState::Missing).0,
            RecoveryAction::RestartNormally
        );
        assert_eq!(
            classify(&clean, WalLockState::UnlockedSentinel).0,
            RecoveryAction::BackupThenRemoveStaleUnlockedSentinel
        );
    }

    #[test]
    fn corrupt_invariants_stop_instead_of_inventing_a_repair() {
        let corrupt = WalInspection {
            metadata: hiqlite_wal::inspection::MetadataInspection {
                present: true,
                format_version: Some(1),
                crc_valid: false,
                last_purged_log_id_present: false,
                last_purged_log_id: None,
                vote: None,
            },
            wal_files: Vec::new(),
            invariant_verdicts: vec!["metadata_corrupt".to_owned()],
            observations: Vec::new(),
        };
        assert_eq!(
            classify(&corrupt, WalLockState::Missing).0,
            RecoveryAction::StopAndEscalateMajorityAtRisk
        );
    }

    #[test]
    fn bounded_source_reader_does_not_modify_the_opened_evidence() {
        let root = crate::test_tempdir().expect("temporary evidence root");
        let source = root.path().join("evidence.bin");
        let expected = b"immutable WAL evidence";
        std::fs::write(&source, expected).expect("seed evidence");

        let (read, size) = read_bounded_regular_file(&source, 1024).expect("read bounded evidence");

        assert_eq!(read, expected);
        assert_eq!(
            size,
            u64::try_from(expected.len()).expect("small fixture size")
        );
        assert_eq!(std::fs::read(&source).expect("re-read evidence"), expected);
    }

    #[cfg(unix)]
    #[test]
    fn bounded_source_reader_refuses_a_final_component_symlink() {
        use std::os::unix::fs::symlink;

        let root = crate::test_tempdir().expect("temporary evidence root");
        let source = root.path().join("authoritative.bin");
        let link = root.path().join("swapped.bin");
        std::fs::write(&source, b"authoritative").expect("seed evidence");
        symlink(&source, &link).expect("seed symlink");

        assert!(read_bounded_regular_file(&link, 1024).is_err());
        assert_eq!(
            std::fs::read(&source).expect("re-read authoritative evidence"),
            b"authoritative"
        );
    }
}
