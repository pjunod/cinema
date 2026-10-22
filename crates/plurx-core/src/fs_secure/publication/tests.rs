use super::*;
use std::io::Write;

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

#[test]
fn held_publication_with_missing_parent_descriptor_never_falls_back_to_a_path() {
    let root = tempfile::tempdir().unwrap();
    let directory = open_directory_nofollow_blocking(root.path()).unwrap();
    let request =
        PublicationRequest::for_operation(INIT_NAME, b"payload".to_vec(), uuid::Uuid::new_v4())
            .unwrap();
    let mut native = PublicationIo::from_held_directory(request, directory);
    let retained_parent = native.directory.take().unwrap();

    assert!(native.run().is_err());
    assert!(native.uses_held_directory());
    assert!(native.entered());
    assert!(native.observation().is_none());
    assert!(!root.path().join(INIT_NAME).exists());
    assert!(retained_parent.metadata().unwrap().is_dir());
}

#[test]
fn native_publication_preserves_the_original_payload_allocation() {
    let root = tempfile::tempdir().unwrap();
    let mut payload = Vec::with_capacity(4096);
    payload.extend_from_slice(b"payload");
    let pointer = payload.as_ptr();
    let capacity = payload.capacity();

    let native = PublicationIo::new(root.path().to_owned(), INIT_NAME, payload).unwrap();
    assert_eq!(native.payload.as_ptr(), pointer);
    assert_eq!(native.payload.capacity(), capacity);
    assert_eq!(native.logical_end(), 7);
    assert!(!native.entered());
    assert!(!native.has_held_directory());
    assert!(native.observation().is_none());
}

#[test]
fn completed_payload_releases_only_after_the_held_directory_returns() {
    let root = tempfile::tempdir().unwrap();
    let directory = open_directory_nofollow_blocking(root.path()).unwrap();
    let mut payload = Vec::with_capacity(4096);
    payload.extend_from_slice(b"payload");
    let capacity = payload.capacity();
    let request =
        PublicationRequest::for_operation(INIT_NAME, payload, uuid::Uuid::new_v4()).unwrap();
    let mut native = PublicationIo::from_held_directory(request, directory);

    native.run().unwrap();
    assert_eq!(native.payload_capacity(), capacity);
    assert!(native.release_completed_payload().is_none());
    assert!(!native.payload_released);

    let returned = native.take_held_directory().unwrap();
    assert!(returned.metadata().unwrap().is_dir());
    assert_eq!(native.release_completed_payload(), Some(capacity));
    assert!(native.payload_released);
    assert_eq!(native.payload_capacity(), 0);
    assert_eq!(native.logical_end(), 7);
    assert!(native.observation().is_some());
    assert!(native.release_completed_payload().is_none());
}

#[test]
fn failed_publication_attempt_cannot_write_again() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(INIT_NAME), b"existing").unwrap();
    let mut native =
        PublicationIo::new(root.path().to_owned(), INIT_NAME, b"replacement".to_vec()).unwrap();
    native.acquire().unwrap();
    assert_eq!(
        native.publish_acquired().unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    let file = native.file.as_ref().unwrap();
    let before = file.metadata().unwrap();
    let identity_before = std_file_identity(file).unwrap();

    assert!(native.publish_acquired().is_err());
    assert!(native.payload_capacity() > 0);
    assert!(native.release_completed_payload().is_none());
    let file = native.file.as_ref().unwrap();
    let after = file.metadata().unwrap();
    assert_eq!(after.len(), before.len());
    assert!(std_file_identity(file).unwrap().same_inode(identity_before));
}

#[test]
fn failed_acquisition_cannot_start_publication() {
    let root = tempfile::tempdir().unwrap();
    let directory = open_directory_nofollow_blocking(root.path()).unwrap();
    let request =
        PublicationRequest::for_operation(INIT_NAME, b"payload".to_vec(), uuid::Uuid::new_v4())
            .unwrap();
    let mut native = PublicationIo::from_held_directory(request, directory);
    drop(native.directory.take());

    assert!(native.acquire().is_err());
    assert!(native.publish_acquired().is_err());
    assert!(native.observation().is_none());
    assert!(!root.path().join(INIT_NAME).exists());
}
