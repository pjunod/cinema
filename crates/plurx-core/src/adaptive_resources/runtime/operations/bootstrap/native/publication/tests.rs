//! Fixture-only tests for same-descriptor native publication ownership.
//! These author behavioral cases; they are not execution or qualification receipts.
use super::*;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::Duration;
use v2::scratch::{DirectoryObservedV1, NativeOutcomeV1, OutputKey};

fn id() -> v2::Id {
    v2::Id::new(Uuid::new_v4()).expect("fixture identity")
}

fn domain() -> v2::FilesystemDomain {
    v2::FilesystemDomain {
        platform: v2::FilesystemPlatform::Linux,
        domain_id: id(),
        daemon_owner_id: id(),
        os_boot: BootIdentity::Known {
            value: Uuid::new_v4(),
            provider: "fixture".into(),
            version: 1,
        },
        mount_namespace: v2::NativeValue::Unknown {
            reason: "publication fixture has no namespace authority".into(),
        },
    }
}

fn held_root(path: &Path) -> (RootCell, DirectoryObservedV1, i32) {
    let file = File::open(path).expect("open fixture directory");
    let original_fd = file.as_raw_fd();
    let metadata = file.metadata().expect("fixture directory metadata");
    let copy_id = id();
    let directory = DirectoryObservedV1 {
        directory_id: id(),
        acquisition_io_id: id(),
        held_copy_ids: vec![copy_id],
        native_object: v2::NativeObject {
            file_type: v2::FileType::Directory,
            device: v2::NativeValue::Known {
                value: metadata.dev(),
            },
            inode: v2::NativeValue::Known {
                value: metadata.ino(),
            },
            mount_id: v2::NativeValue::Unknown {
                reason: "fixture mount identity is not a native receipt".into(),
            },
            filesystem_domain: domain(),
        },
    };
    let root = RootCell {
        io_id: id(),
        observe_id: id(),
        join_id: id(),
        pending: None,
        result: Some(RootResult {
            files: BTreeMap::from([(copy_id, file)]),
            audit: BootstrapResultV1 {
                outcome: NativeOutcomeV1::Completed,
                acquired: Some(directory.clone()),
                closed_copy_ids: vec![],
                child_effect: ChildEffect::Created,
                inventory: InventoryCertainty::Exact,
            },
        }),
        failed_join: false,
        imported_retained: false,
    };
    (root, directory, original_fd)
}

fn publication(io_uuid: Uuid, payload: Vec<u8>) -> Publication {
    let logical_end = u64::try_from(payload.len()).expect("fixture payload length");
    let payload_capacity = u64::try_from(payload.capacity()).expect("fixture payload capacity");
    let request = PublicationRequest::for_operation("init.mp4", payload, io_uuid)
        .expect("valid fixture request");
    Publication {
        io_id: v2::Id::new(io_uuid).expect("publication I/O identity"),
        observe_id: id(),
        join_id: id(),
        copy_id: id(),
        output: OutputKey::Init,
        logical_end,
        payload_capacity,
        content_binding: "fixture-only content binding".into(),
        request: Some(request),
        pending: None,
        result: None,
        failed_join: false,
        acknowledged: false,
    }
}

#[tokio::test]
async fn cancelled_join_keeps_original_publication_and_never_entered_child() {
    let parent = tempfile::tempdir().expect("fixture parent");
    let child = parent.path().join("held-child");
    std::fs::create_dir(&child).expect("fixture child");
    let (mut root, directory, original_fd) = held_root(&child);
    let mut publication = publication(Uuid::new_v4(), b"fixture payload".to_vec());

    let enter = publication
        .launch(&mut root, &directory)
        .expect("register publication before entry");
    let original_task = publication.pending.as_ref().expect("pending job").id();
    assert!(root.result.as_ref().unwrap().files.is_empty());
    assert!(
        tokio::time::timeout(Duration::from_millis(1), publication.join())
            .await
            .is_err()
    );
    assert_eq!(
        publication.pending.as_ref().expect("same pending job").id(),
        original_task
    );
    assert!(publication.result.is_none());
    assert!(!child.join("init.mp4").exists());

    drop(enter);
    publication.join().await.expect("join original worker");
    let result = publication.result.as_mut().unwrap();
    assert!(result.result.is_err());
    assert!(!result.native.entered());
    assert!(result.native.uses_held_directory());
    assert!(result.native.has_held_directory());
    assert!(result.native.take_held_directory().is_err());
    assert!(!child.join("init.mp4").exists());
    assert!(root.result.as_ref().unwrap().files.is_empty());
    assert!(original_fd >= 0);
}

#[tokio::test]
async fn successful_publish_uses_same_held_child_after_path_swap() {
    let parent = tempfile::tempdir().expect("fixture parent");
    let original_path = parent.path().join("rendition");
    let moved_path = parent.path().join("rendition-moved");
    std::fs::create_dir(&original_path).expect("fixture rendition");
    let (mut root, directory, original_fd) = held_root(&original_path);
    std::fs::rename(&original_path, &moved_path).expect("move opened directory");
    std::fs::create_dir(&original_path).expect("replace original path");
    let io_uuid = Uuid::new_v4();
    let expected_temporary = format!(".init.mp4.{io_uuid}.tmp");
    let mut publication = publication(io_uuid, b"published through retained fd".to_vec());

    let enter = publication
        .launch(&mut root, &directory)
        .expect("register publication");
    assert!(publication.request.is_none());
    assert!(root.result.as_ref().unwrap().files.is_empty());
    enter.send(()).expect("release actual native entry");
    let result = publication.join().await.expect("join publication");
    assert!(result.result.is_ok());
    assert!(result.native.entered());
    assert!(result.native.published());
    assert!(result.native.uses_held_directory());
    assert_eq!(
        result.native.temporary_name().to_string_lossy(),
        expected_temporary
    );
    assert_eq!(
        std::fs::read(moved_path.join("init.mp4")).unwrap(),
        b"published through retained fd"
    );
    assert!(!original_path.join("init.mp4").exists());

    let returned = publication
        .result
        .as_mut()
        .unwrap()
        .native
        .take_held_directory()
        .expect("successful result transfers held child");
    assert_eq!(returned.as_raw_fd(), original_fd);
}

#[tokio::test]
async fn destination_conflict_retains_partial_native_object_and_child() {
    let parent = tempfile::tempdir().expect("fixture parent");
    let child = parent.path().join("held-child");
    std::fs::create_dir(&child).expect("fixture child");
    std::fs::write(child.join("init.mp4"), b"incumbent").expect("incumbent output");
    let (mut root, directory, _) = held_root(&child);
    let io_uuid = Uuid::new_v4();
    let mut publication = publication(io_uuid, b"partial candidate".to_vec());

    let enter = publication
        .launch(&mut root, &directory)
        .expect("register publication");
    enter.send(()).expect("release actual native entry");
    publication.join().await.expect("join failed publication");
    let result = publication.result.as_mut().unwrap();
    assert!(result.result.is_err());
    assert!(result.native.entered());
    assert!(!result.native.published());
    assert!(result.native.has_unresolved_object());
    assert!(result.native.has_held_directory());
    assert!(result.native.observation().is_some());
    assert!(result.native.take_held_directory().is_err());
    assert_eq!(std::fs::read(child.join("init.mp4")).unwrap(), b"incumbent");
    let temporary = result.native.temporary_name().to_string_lossy();
    assert_eq!(
        std::fs::read(child.join(temporary.as_ref())).unwrap(),
        b"partial candidate"
    );
    assert!(root.result.as_ref().unwrap().files.is_empty());
}
