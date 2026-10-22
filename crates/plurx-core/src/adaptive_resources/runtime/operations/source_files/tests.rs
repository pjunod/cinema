//! Fixture-only tests for retained source acquisition.
//! They author ownership cases; no test or physical acceptance receipt is implied.
use super::*;
use crate::resource_custody::codec_v2 as v2;
use crate::source_identity::SourceIdentity;
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
    }
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
