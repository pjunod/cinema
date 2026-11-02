//! Additive evidence preservation against real temporary files, including corrupt WALs.
use super::*;
use serde_json::{json, Value};

struct Fixture {
    _root: tempfile::TempDir,
    data: PathBuf,
    destination: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = crate::test_tempdir().expect("offline WAL fixture");
        let data = root.path().join("data");
        std::fs::create_dir_all(data.join("hiqlite/logs")).expect("WAL hierarchy");
        std::fs::write(
            data.join("membership.json"),
            serde_json::to_vec(&json!({"node_id":"evidence-node","raft_id":7})).expect("identity"),
        )
        .expect("identity file");
        std::fs::write(data.join("hiqlite/activation.json"), b"activation evidence")
            .expect("activation evidence");
        // Backup intentionally accepts unverified/corrupt evidence. Verification
        // must reject these bytes; success never claims a valid Raft WAL.
        std::fs::write(
            data.join("hiqlite/logs/corrupt.wal"),
            b"corrupt retained evidence",
        )
        .expect("corrupt evidence");
        let destination = root.path().join("backup");
        Self {
            _root: root,
            data,
            destination,
        }
    }
    fn source(&self) -> Vec<(String, Vec<u8>)> {
        source_files(&offline_paths(&self.data).expect("offline paths"))
            .expect("source inventory")
            .into_iter()
            .map(|(p, r)| {
                (
                    path_label(&r).expect("UTF8"),
                    std::fs::read(p).expect("evidence bytes"),
                )
            })
            .collect()
    }
}

#[test]
fn additive_backup_copies_exact_corrupt_evidence_and_verifiable_manifest_without_mutation() {
    let f = Fixture::new();
    let before = f.source();
    backup(&f.data, &f.destination).expect("additive backup of unverified evidence");
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(f.destination.join("manifest.json")).expect("manifest"),
    )
    .expect("JSON");
    assert_eq!(manifest["expected_node_id"], "evidence-node");
    assert_eq!(manifest["expected_raft_id"], 7);
    let rows = manifest["files"].as_array().expect("fingerprints");
    assert_eq!(rows.len(), before.len());
    for (relative, bytes) in &before {
        assert_eq!(
            std::fs::read(f.destination.join(relative)).expect("copied evidence"),
            *bytes
        );
        let row = rows
            .iter()
            .find(|r| r["path"].as_str() == Some(relative.as_str()))
            .expect("manifest entry");
        assert_eq!(row["size_bytes"], bytes.len() as u64);
        assert_eq!(row["sha256"], hex::encode(Sha256::digest(bytes)));
    }
    assert_eq!(f.source(), before);
    assert!(
        backup(&f.data, &f.destination).is_err(),
        "never overwrite an existing evidence backup"
    );
    assert_eq!(f.source(), before);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&f.destination)
                .expect("directory")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(f.destination.join("manifest.json"))
                .expect("manifest metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn backup_refuses_authoritative_overlap_missing_parent_and_oversized_evidence_before_creation() {
    let f = Fixture::new();
    let before = f.source();
    assert!(backup(&f.data, &f.data.join("inside")).is_err());
    assert!(backup(&f.data, f.data.parent().expect("parent")).is_err());
    assert!(backup(&f.data, &f.destination.join("missing/backup")).is_err());
    let oversized = f.data.join("hiqlite/logs/oversized.bin");
    std::fs::File::create(&oversized)
        .expect("sparse evidence")
        .set_len(MAX_BACKUP_FILE_BYTES + 1)
        .expect("size bound");
    let error = backup(&f.data, &f.destination).expect_err("oversized file");
    assert!(error.to_string().contains("too large"));
    assert!(!f.destination.exists());
    std::fs::remove_file(oversized).expect("remove owned sparse file");
    assert_eq!(f.source(), before);
}

#[cfg(unix)]
#[test]
fn offline_identity_and_evidence_symlinks_are_refused_without_reading_their_targets() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let identity = f.data.join("membership.json");
    let real = f.data.join("stable-identity.json");
    std::fs::rename(&identity, &real).expect("retain identity");
    symlink(&real, &identity).expect("identity link");
    assert!(offline_paths(&f.data).is_err());
    std::fs::remove_file(&identity).expect("unlink fixture");
    std::fs::rename(&real, &identity).expect("restore");
    let outside = f._root.path().join("outside-secret");
    std::fs::write(&outside, b"unrelated-canary").expect("outside");
    symlink(&outside, f.data.join("hiqlite/logs/linked.wal")).expect("evidence link");
    assert!(backup(&f.data, &f.destination).is_err());
    assert!(!f.destination.exists());
    assert_eq!(
        std::fs::read(outside).expect("untouched target"),
        b"unrelated-canary"
    );
    let alias = f._root.path().join("data-alias");
    symlink(&f.data, &alias).expect("directory alias");
    assert!(offline_paths(&alias).is_err());
}

#[test]
fn stable_identity_is_required_and_corruption_collects_escalation_without_applying_repair() {
    let f = Fixture::new();
    let before = f.source();
    let paths = offline_paths(&f.data).expect("stable identity");
    let evidence = collect_evidence(&paths);
    assert_eq!(evidence.node_id, "evidence-node");
    assert_eq!(evidence.raft_id, 7);
    assert_eq!(
        evidence.action,
        RecoveryAction::StopAndEscalateMajorityAtRisk
    );
    assert!(!evidence.reasons.is_empty());
    assert!(status(&f.data, true).is_err());
    assert!(status(&f.data, false).is_err());
    assert!(verify(&f.data, true).is_err());
    assert!(verify(&f.data, false).is_err());
    assert!(recovery_plan(&f.data, true).is_err());
    assert!(recovery_plan(&f.data, false).is_err());
    assert_eq!(
        f.source(),
        before,
        "all diagnostic and recovery-plan operations are read-only"
    );
    for identity in [
        json!({"node_id":" ","raft_id":7}),
        json!({"node_id":"node","raft_id":0}),
        json!({"node_id":"node"}),
    ] {
        std::fs::write(
            f.data.join("membership.json"),
            serde_json::to_vec(&identity).expect("JSON"),
        )
        .expect("invalid identity");
        assert!(offline_paths(&f.data).is_err());
    }
}

#[cfg(unix)]
#[test]
fn actual_advisory_lock_refuses_hashing_backup_and_offline_inspection_until_release() {
    use std::os::fd::AsRawFd;
    let f = Fixture::new();
    let lock = std::fs::File::create(f.data.join("hiqlite/logs/lock.hql")).expect("lock sentinel");
    // SAFETY: the live file descriptor is owned by this scope; no process is signalled.
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let paths = offline_paths(&f.data).expect("paths");
    let evidence = collect_evidence(&paths);
    assert_eq!(evidence.lock, WalLockState::Locked);
    assert!(evidence.inspection.is_none());
    assert_eq!(evidence.action, RecoveryAction::NoAction);
    assert!(require_stopped(&paths.wal).is_err());
    assert!(backup(&f.data, &f.destination).is_err());
    assert!(status(&f.data, true).is_err());
    recovery_plan(&f.data, true).expect("locked owner yields a read-only NO_ACTION plan");
    assert!(!f.destination.exists());
    // End this owned advisory lease explicitly. Closing just this descriptor
    // could leave the same open-file description locked in an unrelated child
    // forked by a concurrent test before its exec. LOCK_UN releases that shared
    // description, so the following assertions observe a real unlock.
    // SAFETY: this scope still owns the live descriptor whose lock it acquired.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) }, 0);
    drop(lock);
    assert_eq!(
        require_stopped(&paths.wal).expect("unlocked sentinel"),
        WalLockState::UnlockedSentinel
    );
    backup(&f.data, &f.destination).expect("additive evidence backup after real lock release");
}

#[tokio::test]
async fn command_dispatch_uses_config_default_and_never_treats_corrupt_backup_as_qualification() {
    let f = Fixture::new();
    let mut config = Config::default();
    config.storage.data_dir = f.data.clone();
    assert!(run(
        WalCommand::Status {
            data_dir: None,
            json: true
        },
        &config
    )
    .await
    .is_err());
    assert!(run(
        WalCommand::Verify {
            data_dir: f.data.clone(),
            deep: true
        },
        &config
    )
    .await
    .is_err());
    assert!(run(
        WalCommand::RecoveryPlan {
            data_dir: f.data.clone(),
            json: true
        },
        &config
    )
    .await
    .is_err());
    run(
        WalCommand::Backup {
            data_dir: f.data.clone(),
            output: f.destination.clone(),
        },
        &config,
    )
    .await
    .expect("backup dispatch");
    assert!(
        verify(&f.data, true).is_err(),
        "preserving bytes does not repair them"
    );
}

#[test]
fn private_backup_primitives_refuse_existing_files_and_non_regular_sources() {
    let f = Fixture::new();
    let path = f._root.path().join("exclusive");
    write_new_private_file(&path, b"first").expect("exclusive write");
    assert!(write_new_private_file(&path, b"replacement").is_err());
    assert_eq!(std::fs::read(&path).expect("read"), b"first");
    assert!(read_bounded_regular_file(&f.data, 1024).is_err());
    let fingerprints =
        fingerprint_sources(&offline_paths(&f.data).expect("paths")).expect("fingerprints");
    assert_eq!(fingerprints.len(), f.source().len());
    assert!(fingerprints.windows(2).all(|w| w[0].path < w[1].path));
}
