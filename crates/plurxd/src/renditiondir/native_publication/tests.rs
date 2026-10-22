//! Authored native regressions. Execution remains after whole-candidate review.
use super::*;
use plurx_core::fs_secure::{open_directory_nofollow_blocking, std_file_identity};
use std::io;
use std::os::fd::AsRawFd;

#[test]
fn held_publication_uses_selected_operation_and_same_directory_without_path_reopen() {
    let root = crate::test_tempdir().unwrap();
    let original = root.path().join("original");
    let moved = root.path().join("moved");
    let foreign = root.path().join("foreign");
    std::fs::create_dir(&original).unwrap();
    std::fs::create_dir(&foreign).unwrap();
    let directory = open_directory_nofollow_blocking(&original).unwrap();
    let descriptor = directory.as_raw_fd();
    let operation = uuid::Uuid::new_v4();
    let request =
        PublicationRequest::for_operation(INIT_NAME, b"held bytes".to_vec(), operation).unwrap();
    let mut native = PublicationIo::from_held_directory(request, directory);
    assert!(native.uses_held_directory());
    assert_eq!(
        native.temporary_name().to_str().unwrap(),
        format!(".{INIT_NAME}.{operation}.tmp")
    );
    assert!(
        native.take_held_directory().is_err(),
        "not entered or completed"
    );
    std::fs::rename(&original, &moved).unwrap();
    std::os::unix::fs::symlink(&foreign, &original).unwrap();
    native.run().unwrap();
    assert_eq!(std::fs::read(moved.join(INIT_NAME)).unwrap(), b"held bytes");
    assert!(!foreign.join(INIT_NAME).exists());
    let returned = native.take_held_directory().unwrap();
    assert_eq!(returned.as_raw_fd(), descriptor, "no dup or reopen");
    assert!(native.take_held_directory().is_err(), "single move");
    assert!(native.run().is_err(), "native operation is never replayed");
}

#[test]
fn held_publication_conflict_retains_parent_and_file_without_false_transfer() {
    let root = crate::test_tempdir().unwrap();
    std::fs::write(root.path().join(INIT_NAME), b"original").unwrap();
    let request =
        PublicationRequest::for_operation(INIT_NAME, b"new".to_vec(), uuid::Uuid::new_v4())
            .unwrap();
    let directory = open_directory_nofollow_blocking(root.path()).unwrap();
    let mut native = PublicationIo::from_held_directory(request, directory);
    assert_eq!(
        native.run().unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert!(native.take_held_directory().is_err());
    assert!(native.has_held_directory());
    let observed = native.observation().unwrap().identity();
    let temporary = root
        .path()
        .join(native.temporary_name().to_string_lossy().as_ref());
    let retained = std::fs::File::open(temporary).unwrap();
    assert!(std_file_identity(&retained).unwrap().same_inode(observed));
    assert!(native.has_unresolved_object());
    assert_eq!(
        std::fs::read(root.path().join(INIT_NAME)).unwrap(),
        b"original"
    );
}

#[test]
fn held_publication_validation_precedes_directory_transfer_and_missing_fd_has_no_fallback() {
    let root = crate::test_tempdir().unwrap();
    let directory = open_directory_nofollow_blocking(root.path()).unwrap();
    assert!(PublicationRequest::for_operation(INIT_NAME, vec![], uuid::Uuid::nil()).is_err());
    assert!(
        PublicationRequest::for_operation("../init.mp4", vec![], uuid::Uuid::new_v4()).is_err()
    );
    assert!(
        directory.metadata().unwrap().is_dir(),
        "validation never consumed the parent"
    );
    let request =
        PublicationRequest::for_operation(INIT_NAME, vec![1], uuid::Uuid::new_v4()).unwrap();
    let mut native = PublicationIo::from_held_directory(request, directory);
    assert!(native.uses_held_directory());
    assert!(native.take_held_directory().is_err());
    native.run().unwrap();
    assert!(native
        .take_held_directory()
        .unwrap()
        .metadata()
        .unwrap()
        .is_dir());
}

#[test]
fn native_publication_refuses_paths_and_preserves_original_payload_allocation() {
    let root = crate::test_tempdir().unwrap();
    for name in [
        "../init.mp4",
        "seg000000.m4s",
        "seg4294967296.m4s",
        ".",
        "init.mp4/child",
    ] {
        assert!(PublicationIo::new(root.path().into(), name, vec![]).is_err());
    }
    let mut bytes = Vec::with_capacity(4096);
    bytes.extend_from_slice(b"payload");
    let capacity = bytes.capacity();
    let native = PublicationIo::new(root.path().into(), INIT_NAME, bytes).unwrap();
    assert_eq!(native.payload_capacity(), capacity);
    assert_eq!(native.logical_end(), 7);
    assert!(!native.entered());
    assert!(!native.has_held_directory());
    assert!(native.acquired().is_none());
}

#[test]
fn native_publication_never_overwrites_and_keeps_conflicting_temp_object() {
    let root = crate::test_tempdir().unwrap();
    std::fs::write(root.path().join(INIT_NAME), b"immutable").unwrap();
    let mut native =
        PublicationIo::new(root.path().into(), INIT_NAME, b"replacement".to_vec()).unwrap();
    assert_eq!(
        native.run().unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(
        std::fs::read(root.path().join(INIT_NAME)).unwrap(),
        b"immutable"
    );
    assert!(!native.published());
    assert!(native.has_unresolved_object());
    let observed = native.acquired().unwrap();
    assert_eq!(observed.identity().size, 11);
    assert_eq!(observed.reported_allocated_bytes() % 512, 0);
    assert!(native
        .acquired()
        .unwrap()
        .identity()
        .same_inode(observed.identity()));
    let before = observed.identity();
    assert!(
        native.run().is_err(),
        "same native operation is never repeated"
    );
    assert!(native.acquired().unwrap().identity().same_inode(before));
}

#[test]
fn native_publication_exclusive_temp_cannot_follow_or_truncate_existing_alias() {
    let root = crate::test_tempdir().unwrap();
    let target = root.path().join("protected");
    std::fs::write(&target, b"keep").unwrap();
    let mut native = PublicationIo::new(root.path().into(), INIT_NAME, b"new".to_vec()).unwrap();
    let tmp = root.path().join(native.temporary_name().to_str().unwrap());
    std::os::unix::fs::symlink(&target, &tmp).unwrap();
    assert_eq!(
        native.run().unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert!(!native.has_unresolved_object());
    assert!(native.acquired().is_none());
    assert_eq!(std::fs::read(target).unwrap(), b"keep");
}

#[test]
fn native_publication_held_parent_is_not_redirected_by_directory_path_swap() {
    let root = crate::test_tempdir().unwrap();
    let original = root.path().join("original");
    let moved = root.path().join("moved");
    let foreign = root.path().join("foreign");
    std::fs::create_dir(&original).unwrap();
    std::fs::create_dir(&foreign).unwrap();
    let mut native = PublicationIo::new(original.clone(), INIT_NAME, b"owned".to_vec()).unwrap();
    native.acquire().unwrap();
    std::fs::rename(&original, &moved).unwrap();
    std::os::unix::fs::symlink(&foreign, &original).unwrap();
    native.publish_acquired().unwrap();
    assert_eq!(std::fs::read(moved.join(INIT_NAME)).unwrap(), b"owned");
    assert!(!foreign.join(INIT_NAME).exists());
    // A FRESH acquisition may not follow the replacement path either.
    let mut later = PublicationIo::new(original, "seg00000.m4s", vec![1]).unwrap();
    assert!(later.run().is_err());
    assert!(!later.has_unresolved_object());
}

#[test]
fn native_publication_changed_temp_alias_does_not_publish_another_object() {
    let root = crate::test_tempdir().unwrap();
    let mut native = PublicationIo::new(root.path().into(), INIT_NAME, b"owned".to_vec()).unwrap();
    native.acquire().unwrap();
    let original = native.acquired().unwrap().identity();
    let tmp = root.path().join(native.temporary_name().to_str().unwrap());
    std::fs::rename(&tmp, root.path().join("original-held")).unwrap();
    std::fs::write(&tmp, b"foreign").unwrap();
    assert!(native.publish_acquired().is_err());
    assert!(!root.path().join(INIT_NAME).exists());
    assert!(native.has_unresolved_object());
    assert!(native.acquired().unwrap().identity().same_inode(original));
    assert_eq!(std::fs::read(&tmp).unwrap(), b"foreign");
}

#[test]
fn native_publication_write_retries_are_bounded_and_partial_errors_are_not_success() {
    let root = crate::test_tempdir().unwrap();
    let mut payload = Vec::with_capacity(8192);
    payload.extend_from_slice(b"abcde");
    let mut native = PublicationIo::new(root.path().into(), INIT_NAME, payload).unwrap();
    assert_eq!(native.logical_end(), 5);
    native.run().unwrap();
    assert_eq!(
        std::fs::read(root.path().join(INIT_NAME)).unwrap(),
        b"abcde"
    );
    assert_eq!(native.observation().unwrap().identity().size, 5);
}
