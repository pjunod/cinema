#![allow(
    clippy::unwrap_used,
    reason = "Original-file fixture assertions deliberately fail the test on the opposite result; production is checked separately."
)]
use super::{SyncFile, SyncIo, SyncRequest};
use crate::fs_secure::std_file_identity;
use std::fs::{self, File};
use std::io;
use std::os::fd::AsRawFd;

fn request_for(directory: &std::path::Path, name: &str) -> SyncRequest {
    let file = File::open(directory.join(name)).unwrap();
    let identity = std_file_identity(&file).unwrap();
    SyncRequest::new(
        uuid::Uuid::new_v4(),
        vec![SyncFile::new(name, identity, None).unwrap()],
    )
    .unwrap()
}

#[test]
fn held_directory_survives_path_replacement_and_returns_same_descriptor() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("rendition");
    let moved = root.path().join("moved");
    fs::create_dir(&original).unwrap();
    fs::write(original.join("init.mp4"), b"original").unwrap();
    let request = request_for(&original, "init.mp4");
    let directory = File::open(&original).unwrap();
    let raw = directory.as_raw_fd();
    let mut io = SyncIo::from_held_directory(request, directory);
    fs::rename(&original, &moved).unwrap();
    fs::create_dir(&original).unwrap();
    fs::write(original.join("init.mp4"), b"replacement").unwrap();

    io.run().unwrap();
    assert!(io.completed());
    let returned = io.take_held_directory().unwrap();
    assert_eq!(returned.as_raw_fd(), raw);
    assert_eq!(fs::read(moved.join("init.mp4")).unwrap(), b"original");
}

#[test]
fn mismatched_open_file_is_retained_and_cannot_be_replayed() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path();
    fs::write(directory.join("init.mp4"), b"actual").unwrap();
    fs::write(directory.join("seg00001.m4s"), b"different").unwrap();
    let other = File::open(directory.join("seg00001.m4s")).unwrap();
    let request = SyncRequest::new(
        uuid::Uuid::new_v4(),
        vec![SyncFile::new("init.mp4", std_file_identity(&other).unwrap(), None).unwrap()],
    )
    .unwrap();
    let mut io = SyncIo::from_held_directory(request, File::open(directory).unwrap());

    assert!(io.run().is_err());
    assert!(io.entered());
    assert!(io.current_file().is_some());
    assert!(io.has_held_directory());
    assert!(io.take_held_directory().is_none());
    assert_eq!(io.run().unwrap_err().kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn missing_file_fails_without_releasing_directory_or_allowing_reentry() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("init.mp4"), b"exists").unwrap();
    let request = request_for(root.path(), "init.mp4");
    fs::remove_file(root.path().join("init.mp4")).unwrap();
    let mut io = SyncIo::from_held_directory(request, File::open(root.path()).unwrap());

    assert!(io.run().is_err());
    assert!(io.entered());
    assert!(io.current_file().is_none());
    assert!(io.has_held_directory());
    assert!(io.take_held_directory().is_none());
    assert!(io.run().is_err());
}
