//! Authored native regressions. Execution remains after whole-candidate review.
use super::*;

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
    let pointer = bytes.as_ptr();
    let capacity = bytes.capacity();
    let native = PublicationIo::new(root.path().into(), INIT_NAME, bytes).unwrap();
    assert_eq!(native.payload.as_ptr(), pointer);
    assert_eq!(native.payload.capacity(), capacity);
    assert_eq!(native.logical_end, 7);
    assert!(!native.entered);
    assert!(native.directory.is_none());
    assert!(native.acquired.is_none());
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
    assert!(!native.published);
    assert!(native.has_unresolved_object());
    let observed = native.acquired.unwrap();
    assert_eq!(observed.identity.size, 11);
    assert_eq!(observed.reported_allocated_bytes % 512, 0);
    assert_eq!(
        std_file_identity(native.file.as_ref().unwrap()).unwrap(),
        observed.identity
    );
    let before = observed.identity;
    assert!(
        native.run().is_err(),
        "same native operation is never repeated"
    );
    assert!(native.acquired.unwrap().identity.same_inode(before));
}

#[test]
fn native_publication_exclusive_temp_cannot_follow_or_truncate_existing_alias() {
    let root = crate::test_tempdir().unwrap();
    let target = root.path().join("protected");
    std::fs::write(&target, b"keep").unwrap();
    let mut native = PublicationIo::new(root.path().into(), INIT_NAME, b"new".to_vec()).unwrap();
    let tmp = root.path().join(native.temporary_name.to_str().unwrap());
    std::os::unix::fs::symlink(&target, &tmp).unwrap();
    assert_eq!(
        native.run().unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert!(native.file.is_none());
    assert!(native.acquired.is_none());
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
    native.write_and_publish().unwrap();
    native.observe().unwrap();
    assert_eq!(std::fs::read(moved.join(INIT_NAME)).unwrap(), b"owned");
    assert!(!foreign.join(INIT_NAME).exists());
    // A FRESH acquisition may not follow the replacement path either.
    let mut later = PublicationIo::new(original, "seg00000.m4s", vec![1]).unwrap();
    assert!(later.run().is_err());
    assert!(later.file.is_none());
}

#[test]
fn native_publication_changed_temp_alias_does_not_publish_another_object() {
    let root = crate::test_tempdir().unwrap();
    let mut native = PublicationIo::new(root.path().into(), INIT_NAME, b"owned".to_vec()).unwrap();
    native.acquire().unwrap();
    let original = native.acquired.unwrap().identity;
    let tmp = root.path().join(native.temporary_name.to_str().unwrap());
    std::fs::rename(&tmp, root.path().join("original-held")).unwrap();
    std::fs::write(&tmp, b"foreign").unwrap();
    assert!(native.write_and_publish().is_err());
    assert!(!root.path().join(INIT_NAME).exists());
    assert!(native.has_unresolved_object());
    assert!(std_file_identity(native.file.as_ref().unwrap())
        .unwrap()
        .same_inode(original));
    assert_eq!(std::fs::read(&tmp).unwrap(), b"foreign");
}

#[test]
fn native_publication_write_retries_are_bounded_and_partial_errors_are_not_success() {
    struct Partial {
        bytes: Vec<u8>,
        calls: usize,
        fail: bool,
    }
    impl Write for Partial {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            if self.calls == 1 {
                return Err(io::ErrorKind::Interrupted.into());
            }
            if self.fail && self.bytes.len() >= 2 {
                return Err(io::ErrorKind::StorageFull.into());
            }
            let count = bytes.len().min(2);
            self.bytes.extend_from_slice(&bytes[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Partial {
        bytes: vec![],
        calls: 0,
        fail: false,
    };
    assert!(write_bounded(&mut writer, b"abcde", 4).is_err());
    assert_eq!(
        writer.calls, 0,
        "out-of-bound payload refuses before native write"
    );
    write_bounded(&mut writer, b"abcde", 5).unwrap();
    assert_eq!(writer.bytes, b"abcde");
    let mut partial = Partial {
        bytes: vec![],
        calls: 0,
        fail: true,
    };
    assert!(write_bounded(&mut partial, b"abcde", 5).is_err());
    assert_eq!(partial.bytes, b"ab");
}
