//! Fixture-only tests for retained source acquisition.
//! They author ownership cases; no test or physical acceptance receipt is implied.
use super::*;
use crate::resource_custody::codec_v2 as v2;
use crate::source_identity::SourceIdentity;
#[cfg(target_os = "linux")]
use sha2::{Digest, Sha256};
#[cfg(target_os = "linux")]
use std::fs::File;
use std::fs::Metadata;
#[cfg(target_os = "linux")]
use std::io::Read;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

fn id() -> v2::Id {
    v2::Id::new(Uuid::new_v4()).expect("fixture identity")
}

#[cfg(target_os = "linux")]
fn namespace() -> v2::NativeValue<u64> {
    v2::NativeValue::Known {
        value: std::fs::metadata("/proc/self/ns/mnt")
            .expect("Linux mount namespace metadata")
            .ino(),
    }
}

fn filesystem_domain(mount_namespace: v2::NativeValue<u64>) -> v2::FilesystemDomain {
    v2::FilesystemDomain {
        platform: v2::FilesystemPlatform::Linux,
        domain_id: id(),
        daemon_owner_id: id(),
        os_boot: BootIdentity::Known {
            value: Uuid::new_v4(),
            provider: "fixture metadata only".into(),
            version: 1,
        },
        mount_namespace,
    }
}

fn scanner_mtime(metadata: &Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|modified| modified.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

fn source_cell(path: &Path, domain: v2::FilesystemDomain) -> SourceCell {
    let metadata = std::fs::metadata(path).expect("fixture source metadata");
    let owner_id = domain.daemon_owner_id;
    let graph_id = id();
    let member_id = id();
    let copy_id = id();
    let acquisition_id = id();
    let source = v2::SourcePlan {
        version: 1,
        holder_id: id(),
        member_id,
        source_association: v2::SourceAssociation {
            media_file_id: "fixture-media".into(),
            object_version: SourceIdentity::from_metadata(&metadata).render_s1(),
            recipe_source_binding: "fixture-software-recipe".into(),
        },
        access: v2::SourceAccess::ReadOnlyRegular,
        filesystem_domain: domain.clone(),
        planned_copies: vec![v2::SourceCopy {
            copy_id,
            owner: v2::CopyOwner::DaemonSourceCell {
                owner_id,
                graph_id,
                member_id,
            },
            acquisition_operation_id: acquisition_id,
            parent_copy_id: None,
        }],
        recovery: v2::SourceRecovery::LocalSourceHandlesV1 {
            owner_id,
            graph_id,
            filesystem_domain: domain,
        },
    };
    let member_plan = v2::MemberPlan::SourceV1 {
        slot: v2::MemberSlot {
            member_id,
            owned_by: None,
            depends_on: vec![],
            protected_paths: vec![path.to_string_lossy().into_owned()],
        },
        source,
    };
    SourceCell {
        spec: Some(SourceOpenSpec {
            path: path.to_owned(),
            scanner_size: i64::try_from(metadata.len()).expect("fixture source size"),
            scanner_mtime: scanner_mtime(&metadata),
            expected_sha256: None,
        }),
        plan: member_plan,
        acquisition_id,
        observation_id: Uuid::new_v4(),
        pending: None,
        result: None,
        failed_join: false,
        observed_committed: false,
        lease_issued: false,
        root_observation_id: Uuid::new_v4(),
        root_observed_committed: false,
        selected_root_member: None,
        pending_root: None,
        parent_close_operation_id: Uuid::new_v4(),
        parent_close: ParentCloseState::NotStarted,
    }
}

#[cfg(target_os = "linux")]
fn bind_expected_executable(cell: &mut SourceCell, digest: [u8; 32]) {
    cell.spec.as_mut().unwrap().expected_sha256 = Some(digest);
    let v2::MemberPlan::SourceV1 { source, .. } = &mut cell.plan else {
        unreachable!()
    };
    source.source_association.recipe_source_binding = executable_recipe_binding(&digest);
}

#[cfg(target_os = "linux")]
fn open_with_cache(cell: &mut SourceCell, cache: &Mutex<VerifiedExecutableCache>) -> SourceResult {
    let spec = cell.spec.take().expect("fixture source spec");
    let v2::MemberPlan::SourceV1 { source, .. } = &cell.plan else {
        panic!("fixture source plan")
    };
    open_source_with_cache(spec, source.clone(), cache)
}

#[cfg(target_os = "linux")]
fn fixture_elf() -> Vec<u8> {
    let mut bytes = vec![0u8; 32];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = if cfg!(target_pointer_width = "64") {
        2
    } else {
        1
    };
    bytes[5] = if cfg!(target_endian = "little") { 1 } else { 2 };
    bytes[6] = 1;
    let machine = if cfg!(target_arch = "x86_64") {
        62u16
    } else if cfg!(target_arch = "aarch64") {
        183
    } else if cfg!(target_arch = "x86") {
        3
    } else if cfg!(target_arch = "arm") {
        40
    } else if cfg!(target_arch = "riscv64") {
        243
    } else if cfg!(target_arch = "s390x") {
        22
    } else if cfg!(target_arch = "powerpc64") {
        21
    } else {
        panic!("unsupported fixture architecture")
    };
    let endian_bytes = if cfg!(target_endian = "little") {
        (3u16.to_le_bytes(), machine.to_le_bytes())
    } else {
        (3u16.to_be_bytes(), machine.to_be_bytes())
    };
    bytes[16..18].copy_from_slice(&endian_bytes.0);
    bytes[18..20].copy_from_slice(&endian_bytes.1);
    bytes
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn binary_digest_uses_same_source_descriptor_without_changing_its_offset() {
    let directory = tempfile::Builder::new()
        .prefix("plurx-source-digest-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .expect("repository-local fixture directory");
    let path = directory.path().join("source.bin");
    let contents = fixture_elf();
    std::fs::write(&path, &contents).expect("fixture source");
    let mut cell = source_cell(&path, filesystem_domain(namespace()));
    let digest = Sha256::digest(&contents).into();
    bind_expected_executable(&mut cell, digest);

    let enter = cell.launch().expect("register source job");
    enter.send(()).expect("enter source open");
    cell.join().await.expect("join source open");
    let result = cell.result.as_ref().expect("cached source result");
    assert!(result.observed.is_some());
    assert_eq!(result.verified_sha256, Some(digest));
    assert!(result.verified_elf);
    let file = result.file.as_ref().expect("same held descriptor");
    let mut descriptor = file.as_ref();
    let mut prefix = [0u8; 4];
    descriptor
        .read_exact(&mut prefix)
        .expect("read original offset");
    assert_eq!(&prefix, b"\x7fELF");
}

#[cfg(target_os = "linux")]
#[test]
fn publication_rechecks_the_same_held_file_after_in_place_mutation() {
    let directory = tempfile::Builder::new()
        .prefix("plurx-source-publication-fence-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .expect("repository-local fixture directory");
    let path = directory.path().join("source.bin");
    std::fs::write(&path, b"original-source").expect("fixture source");
    let mut cell = source_cell(&path, filesystem_domain(namespace()));
    let cache = Mutex::new(VerifiedExecutableCache::new());
    let opened = open_with_cache(&mut cell, &cache);
    cell.result = Some(opened);
    assert!(cell.result.as_ref().unwrap().observed.is_some());
    // The runtime sets these only after both original observations commit;
    // this fixture isolates the later native identity check.
    cell.observed_committed = true;
    cell.root_observed_committed = true;
    cell.selected_root_member = Some(Uuid::new_v4());
    assert!(cell.unchanged_for_publication().is_ok());

    std::fs::write(&path, b"rewritten-source-longer").expect("mutate original inode");
    assert!(cell.unchanged_for_publication().is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn repeated_held_source_uses_bounded_identity_cache_after_descriptor_stat_checks() {
    let directory = tempfile::Builder::new()
        .prefix("plurx-source-digest-cache-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .expect("repository-local fixture directory");
    let path = directory.path().join("source.bin");
    let contents = fixture_elf();
    std::fs::write(&path, &contents).expect("fixture source");
    let digest = Sha256::digest(&contents).into();
    let cache = Mutex::new(VerifiedExecutableCache::new());

    let mut first = source_cell(&path, filesystem_domain(namespace()));
    bind_expected_executable(&mut first, digest);
    let first_result = open_with_cache(&mut first, &cache);
    assert_eq!(first_result.verified_sha256, Some(digest));
    assert!(!first_result.verified_sha256_cached);

    let mut second = source_cell(&path, filesystem_domain(namespace()));
    bind_expected_executable(&mut second, digest);
    let second_result = open_with_cache(&mut second, &cache);
    assert_eq!(second_result.verified_sha256, Some(digest));
    assert!(second_result.verified_elf);
    assert!(second_result.verified_sha256_cached);
}

#[cfg(target_os = "linux")]
#[test]
fn changed_source_identity_or_digest_cannot_reuse_cached_verification() {
    let directory = tempfile::Builder::new()
        .prefix("plurx-source-digest-cache-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .expect("repository-local fixture directory");
    let path = directory.path().join("source.bin");
    let original = fixture_elf();
    std::fs::write(&path, &original).expect("fixture source");
    let original_digest = Sha256::digest(&original).into();
    let cache = Mutex::new(VerifiedExecutableCache::new());
    let mut first = source_cell(&path, filesystem_domain(namespace()));
    bind_expected_executable(&mut first, original_digest);
    assert!(open_with_cache(&mut first, &cache)
        .verified_sha256
        .is_some());

    let mut changed = original.clone();
    *changed.last_mut().expect("fixture has bytes") ^= 1;
    std::fs::write(&path, &changed).expect("change executable bytes");
    let mut second = source_cell(&path, filesystem_domain(namespace()));
    bind_expected_executable(&mut second, original_digest);
    let result = open_with_cache(&mut second, &cache);
    assert!(result.file.is_some(), "actual descriptor remains retained");
    assert!(result.observed.is_none());
    assert_eq!(result.verified_sha256, None);
    assert!(!result.verified_sha256_cached);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn binary_digest_mismatch_keeps_actual_file_and_withholds_observation() {
    let directory = tempfile::Builder::new()
        .prefix("plurx-source-digest-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .expect("repository-local fixture directory");
    let path = directory.path().join("source.bin");
    std::fs::write(&path, fixture_elf()).expect("fixture source");
    let mut cell = source_cell(&path, filesystem_domain(namespace()));
    bind_expected_executable(&mut cell, [0u8; 32]);

    let enter = cell.launch().expect("register source job");
    enter.send(()).expect("enter source open");
    cell.join().await.expect("join source open");
    let result = cell.result.as_ref().expect("cached failed result");
    assert!(
        result.file.is_some(),
        "actual opened descriptor remains owned"
    );
    assert!(
        result.observed.is_none(),
        "mismatch cannot produce observation"
    );
    assert_eq!(result.verified_sha256, None);
    assert!(!result.verified_elf);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn script_with_matching_digest_is_not_accepted_as_executable_elf() {
    let directory = tempfile::Builder::new()
        .prefix("plurx-source-digest-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .expect("repository-local fixture directory");
    let path = directory.path().join("source.bin");
    let script = b"#!/bin/sh\nexit 0\n";
    std::fs::write(&path, script).expect("fixture source");
    let mut cell = source_cell(&path, filesystem_domain(namespace()));
    bind_expected_executable(&mut cell, Sha256::digest(script).into());

    let enter = cell.launch().expect("register source job");
    enter.send(()).expect("enter source open");
    cell.join().await.expect("join source open");
    let result = cell.result.as_ref().expect("cached rejected result");
    assert!(result.file.is_some(), "actual file remains owned");
    assert!(result.observed.is_none());
    assert_eq!(result.verified_sha256, None);
    assert!(!result.verified_elf);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn inherited_source_stages_only_the_planned_child_copy_on_the_same_file() {
    let directory = tempfile::tempdir().expect("fixture directory");
    let path = directory.path().join("source.mkv");
    std::fs::write(&path, b"inherited source fixture").expect("fixture source");
    let mut cell = source_cell(&path, filesystem_domain(namespace()));
    let root_member = id().uuid();
    let (graph_id, source_copy) = {
        let v2::MemberPlan::SourceV1 { source, .. } = &mut cell.plan else {
            panic!("fixture source plan")
        };
        let graph_id = match &source.recovery {
            v2::SourceRecovery::LocalSourceHandlesV1 { graph_id, .. } => *graph_id,
        };
        let source_copy = source.planned_copies[0].clone();
        source.planned_copies.push(v2::SourceCopy {
            copy_id: id(),
            owner: v2::CopyOwner::OwnedRoot {
                graph_id,
                root_member_id: v2::Id::new(root_member).expect("root member"),
            },
            acquisition_operation_id: id(),
            parent_copy_id: Some(source_copy.copy_id),
        });
        (graph_id, source_copy)
    };
    let enter = cell.launch().expect("register source job");
    enter.send(()).expect("enter source open");
    cell.join().await.expect("join source acquisition");
    cell.observed_committed = true;
    let original_fd = cell
        .result
        .as_ref()
        .and_then(|result| result.file.as_ref())
        .expect("retained source")
        .as_raw_fd();

    let first = cell
        .inherited_source(root_member)
        .expect("planned root inherits retained source");
    assert_eq!(first.raw_fd, original_fd);
    assert_eq!(first.observation_id, cell.root_observation_id);
    assert!(!first.acknowledged);
    let observed = match first.envelope.observed.as_ref().expect("observed source") {
        v2::Observed::SourceV1 { source } => source,
        _ => panic!("source observation"),
    };
    assert_eq!(observed.copies.len(), 2);
    assert!(observed.copies.contains(&source_copy));
    assert!(observed.copies.iter().any(|copy| matches!(
        copy.owner,
        v2::CopyOwner::OwnedRoot { graph_id: actual, root_member_id }
            if actual == graph_id && root_member_id.uuid() == root_member
    )));

    let repeated = cell
        .inherited_source(root_member)
        .expect("same root reuses staged observation");
    assert_eq!(repeated.raw_fd, original_fd);
    assert_eq!(repeated.observation_id, first.observation_id);
    assert_eq!(repeated.envelope, first.envelope);
    assert!(cell.inherited_source(id().uuid()).is_err());
    assert!(
        !cell.lease_issued,
        "inheritance does not export an Arc lease"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn parent_close_releases_original_descriptor_once_and_keeps_it_unexportable() {
    let directory = tempfile::tempdir().expect("fixture directory");
    let path = directory.path().join("source.mkv");
    std::fs::write(&path, b"parent close fixture").expect("fixture source");
    let mut cell = source_cell(&path, filesystem_domain(namespace()));
    let enter = cell.launch().expect("register source job");
    enter.send(()).expect("enter source open");
    cell.join().await.expect("join source acquisition");
    cell.observed_committed = true;
    let original_fd = cell
        .result
        .as_ref()
        .and_then(|result| result.file.as_ref())
        .expect("retained source")
        .as_raw_fd();
    let close_operation = cell.parent_close_operation_id();

    cell.close_parent().await.expect("close original parent fd");
    assert!(cell.parent_closed());
    assert!(cell.result.as_ref().unwrap().file.is_none());
    assert_eq!(cell.parent_close_operation_id(), close_operation);

    // If the OS reuses the just-closed number, an idempotent repeat must not
    // close the new owner of that number.
    let sentinel_path = directory.path().join("sentinel");
    std::fs::write(&sentinel_path, b"sentinel remains open").expect("sentinel file");
    let sentinel = File::open(&sentinel_path).expect("open sentinel");
    let reused = sentinel.as_raw_fd() == original_fd;
    cell.close_parent()
        .await
        .expect("repeat successful parent close is idempotent");
    assert!(cell.parent_closed());
    assert_eq!(cell.parent_close_operation_id(), close_operation);
    if reused {
        // SAFETY: the sentinel File still owns this descriptor.
        assert_ne!(unsafe { libc::fcntl(original_fd, libc::F_GETFD) }, -1);
    }
}

#[tokio::test]
async fn unreleased_source_latch_retains_original_job_and_never_acquires_file() {
    let directory = tempfile::tempdir().expect("fixture directory");
    let path = directory.path().join("source.mkv");
    std::fs::write(&path, b"fixture source bytes").expect("fixture source");
    let mut cell = source_cell(
        &path,
        filesystem_domain(v2::NativeValue::Unknown {
            reason: "fixture does not enter native source open".into(),
        }),
    );

    let enter = cell.launch().expect("register source job");
    let original_job = cell.pending.as_ref().expect("registered job").id();
    assert!(tokio::time::timeout(Duration::from_millis(1), cell.join())
        .await
        .is_err());
    assert_eq!(
        cell.pending.as_ref().expect("same original job").id(),
        original_job
    );
    assert!(cell.result.is_none());
    assert!(cell.spec.is_none());

    drop(enter);
    let result = cell.join().await.expect("join original never-entered job");
    assert!(result.file.is_none());
    assert!(result.observed.is_none());
    assert!(cell.pending.is_none());
    assert_eq!(std::fs::read(path).unwrap(), b"fixture source bytes");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn source_open_uses_scanner_s1_and_retains_the_same_file_arc() {
    let directory = tempfile::tempdir().expect("fixture directory");
    let path = directory.path().join("source.mkv");
    std::fs::write(&path, b"scanner-bound source").expect("fixture source");
    let expected = std::fs::metadata(&path).expect("fixture metadata");
    let mut cell = source_cell(&path, filesystem_domain(namespace()));
    let enter = cell.launch().expect("register source job");
    enter.send(()).expect("enter source open");

    let (original_arc, original_fd, original_inode, observed) = {
        let result = cell.join().await.expect("join opened source");
        let file = result.file.as_ref().expect("retained original File");
        let observed = result.observed.as_ref().expect("matching SourceObserved");
        assert_eq!(
            observed.source_association.object_version,
            SourceIdentity::from_metadata(&expected).render_s1()
        );
        assert_eq!(observed.version, 1);
        assert_eq!(observed.copies.len(), 1);
        assert!(matches!(
            &observed.copies[0].owner,
            v2::CopyOwner::DaemonSourceCell { .. }
        ));
        (
            Arc::as_ptr(file),
            file.as_raw_fd(),
            file.metadata().unwrap().ino(),
            observed.clone(),
        )
    };
    let result = cell
        .join()
        .await
        .expect("repeated join uses retained result");
    let file = result.file.as_ref().expect("same retained File");
    assert_eq!(Arc::as_ptr(file), original_arc);
    assert_eq!(file.as_raw_fd(), original_fd);
    assert_eq!(
        observed.native_object.inode,
        v2::NativeValue::Known {
            value: original_inode
        }
    );

    let lease = RetainedSourceLease {
        file: Arc::clone(file),
    };
    let borrowed: &File = lease.file();
    assert_eq!(borrowed.as_raw_fd(), original_fd);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn scanner_or_expected_version_mismatch_keeps_open_file_without_observation() {
    let directory = tempfile::tempdir().expect("fixture directory");
    let path = directory.path().join("source.mkv");
    std::fs::write(&path, b"mismatch fixture").expect("fixture source");
    let domain = filesystem_domain(namespace());

    let mut scanner_mismatch = source_cell(&path, domain.clone());
    scanner_mismatch.spec.as_mut().unwrap().scanner_size += 1;
    let enter = scanner_mismatch.launch().expect("register source job");
    enter.send(()).expect("enter source open");
    let result = scanner_mismatch.join().await.unwrap();
    assert!(
        result.file.is_some(),
        "actual opened descriptor is retained"
    );
    assert!(
        result.observed.is_none(),
        "scanner mismatch cannot mint an observation"
    );

    let mut version_mismatch = source_cell(&path, domain);
    let v2::MemberPlan::SourceV1 { source, .. } = &mut version_mismatch.plan else {
        panic!("fixture source member plan")
    };
    source.source_association.object_version.push_str("-stale");
    let enter = version_mismatch.launch().expect("register source job");
    enter.send(()).expect("enter source open");
    let result = version_mismatch.join().await.unwrap();
    assert!(
        result.file.is_some(),
        "actual opened descriptor is retained"
    );
    assert!(
        result.observed.is_none(),
        "stale expected s1 cannot mint an observation"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn path_replacement_after_open_does_not_redirect_retained_source() {
    let directory = tempfile::tempdir().expect("fixture directory");
    let path = directory.path().join("source.mkv");
    let moved = directory.path().join("source-opened.mkv");
    std::fs::write(&path, b"original opened bytes").expect("fixture source");
    let mut cell = source_cell(&path, filesystem_domain(namespace()));
    let enter = cell.launch().expect("register source job");
    enter.send(()).expect("enter source open");
    let (original_inode, original_fd) = {
        let result = cell.join().await.expect("join opened source");
        let file = result.file.as_ref().expect("retained actual file");
        (file.metadata().unwrap().ino(), file.as_raw_fd())
    };

    std::fs::rename(&path, &moved).expect("move path of opened source");
    std::fs::write(&path, b"replacement path bytes").expect("replace source path");
    let result = cell.join().await.expect("same retained source result");
    let file = result.file.as_ref().expect("same retained actual file");
    assert_eq!(file.as_raw_fd(), original_fd);
    assert_eq!(file.metadata().unwrap().ino(), original_inode);
    assert_eq!(
        result.observed.as_ref().unwrap().native_object.inode,
        v2::NativeValue::Known {
            value: original_inode
        }
    );
    let mut opened_contents = String::new();
    let mut borrowed_file = file.as_ref();
    borrowed_file.read_to_string(&mut opened_contents).unwrap();
    assert_eq!(opened_contents, "original opened bytes");
    assert_eq!(std::fs::read(path).unwrap(), b"replacement path bytes");
}

#[cfg(unix)]
mod close_resume;
