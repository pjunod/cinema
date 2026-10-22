//! Fixture-only sync custody tests; these are not ordinary-path or journal receipts.
use super::*;
use crate::fs_secure::{std_file_identity, sync_files::SyncFile};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::Duration;
use v2::scratch::{DirectoryObservedV1, NativeOutcomeV1};

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
            reason: "sync fixture has no namespace authority".into(),
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
                reason: "sync fixture mount identity is not a native receipt".into(),
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
    };
    (root, directory, original_fd)
}

fn sync_for(name: &str, expected: crate::fs_secure::FileIdentity) -> Sync {
    let io_uuid = Uuid::new_v4();
    let request = SyncRequest::new(
        io_uuid,
        vec![SyncFile::new(name, expected, None).expect("valid fixture file")],
    )
    .expect("valid fixture request");
    Sync {
        io: v2::Id::new(io_uuid).expect("sync operation identity"),
        observe: id(),
        join: id(),
        files: vec![id()],
        request: Some(request),
        pending: None,
        result: None,
        failed_join: false,
        acknowledged: false,
    }
}

fn identity(path: &Path) -> crate::fs_secure::FileIdentity {
    std_file_identity(&File::open(path).expect("open fixture file")).expect("file identity")
}

#[tokio::test]
async fn cancelled_entry_latch_retains_original_child_and_worker_result() {
    let parent = tempfile::tempdir().expect("fixture parent");
    let child = parent.path().join("held-child");
    std::fs::create_dir(&child).expect("fixture child");
    std::fs::write(child.join("init.mp4"), b"init").expect("fixture init");
    let (mut root, directory, original_fd) = held_root(&child);
    let mut sync = sync_for("init.mp4", identity(&child.join("init.mp4")));

    let enter = sync
        .launch(&mut root, &directory)
        .expect("register sync before native entry");
    assert!(sync.pending.is_some());
    assert!(root.result.as_ref().unwrap().files.is_empty());
    drop(enter);
    sync.join().await.expect("join original worker");

    let result = sync.result.as_mut().unwrap();
    assert!(result.result.is_err());
    assert!(!result.native.entered());
    assert!(result.native.has_held_directory());
    assert!(result.native.take_held_directory().is_none());
    assert!(root.result.as_ref().unwrap().files.is_empty());
    assert!(original_fd >= 0);
}

#[tokio::test]
async fn successful_sync_uses_same_held_child_after_path_swap() {
    let parent = tempfile::tempdir().expect("fixture parent");
    let original_path = parent.path().join("rendition");
    let moved_path = parent.path().join("rendition-moved");
    std::fs::create_dir(&original_path).expect("fixture rendition");
    std::fs::write(original_path.join("init.mp4"), b"held rendition").expect("fixture init");
    let (mut root, directory, original_fd) = held_root(&original_path);
    let mut sync = sync_for("init.mp4", identity(&original_path.join("init.mp4")));
    std::fs::rename(&original_path, &moved_path).expect("move opened directory");
    std::fs::create_dir(&original_path).expect("replace old pathname");
    std::fs::write(original_path.join("init.mp4"), b"replacement").expect("replacement init");

    let enter = sync
        .launch(&mut root, &directory)
        .expect("register sync on held child");
    enter.send(()).expect("release native sync entry");
    sync.join().await.expect("join sync");
    let result = sync.result.as_mut().unwrap();
    assert!(result.result.is_ok());
    assert!(result.native.completed());
    assert_eq!(
        std::fs::read(moved_path.join("init.mp4")).unwrap(),
        b"held rendition"
    );
    assert_eq!(
        std::fs::read(original_path.join("init.mp4")).unwrap(),
        b"replacement"
    );
    let returned = result
        .native
        .take_held_directory()
        .expect("successful sync returns held child");
    assert_eq!(returned.as_raw_fd(), original_fd);
}

#[tokio::test]
async fn wrong_file_identity_retains_opened_file_and_held_directory() {
    let parent = tempfile::tempdir().expect("fixture parent");
    let child = parent.path().join("held-child");
    std::fs::create_dir(&child).expect("fixture child");
    std::fs::write(child.join("init.mp4"), b"actual").expect("actual init");
    std::fs::write(child.join("seg00001.m4s"), b"expected other inode").expect("other file");
    let (mut root, directory, _) = held_root(&child);
    let mut sync = sync_for("init.mp4", identity(&child.join("seg00001.m4s")));

    let enter = sync.launch(&mut root, &directory).expect("register sync");
    enter.send(()).expect("release native sync entry");
    sync.join().await.expect("join failed sync");
    let result = sync.result.as_mut().unwrap();
    assert!(result.result.is_err());
    assert!(result.native.entered());
    assert!(result.native.current_file().is_some());
    assert!(result.native.has_held_directory());
    assert!(result.native.take_held_directory().is_none());
    assert!(root.result.as_ref().unwrap().files.is_empty());
}

#[tokio::test]
async fn cancelled_wait_keeps_original_join_handle_and_rejects_replay() {
    let parent = tempfile::tempdir().expect("fixture parent");
    let child = parent.path().join("held-child");
    std::fs::create_dir(&child).expect("fixture child");
    std::fs::write(child.join("init.mp4"), b"init").expect("fixture init");
    let (mut root, directory, _) = held_root(&child);
    let mut sync = sync_for("init.mp4", identity(&child.join("init.mp4")));

    let enter = sync.launch(&mut root, &directory).expect("register sync");
    let original_task = sync.pending.as_ref().expect("original worker").id();
    assert!(tokio::time::timeout(Duration::from_millis(1), sync.join())
        .await
        .is_err());
    assert_eq!(
        sync.pending.as_ref().expect("same worker").id(),
        original_task
    );
    assert!(sync.result.is_none());
    assert!(sync.launch(&mut root, &directory).is_err());

    enter.send(()).expect("release original worker");
    sync.join().await.expect("join original sync");
    assert!(sync.result.as_ref().unwrap().result.is_ok());
    assert!(sync.result.as_ref().unwrap().native.completed());
}
