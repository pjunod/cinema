//! Persisted installation-location guard regressions; not host-continuity tests.
use super::super::tests::{options, owner};
use super::*;

#[cfg(unix)]
fn copy_installation(source: &Path, target: &Path) {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(target)
        .expect("private copy directory");
    for name in ["installation.json", "obligations.sqlite3"] {
        std::fs::copy(source.join(name), target.join(name)).expect("coherent ordinary copy");
    }
}

#[cfg(unix)]
#[test]
fn coherent_same_host_copy_is_refused_without_rewriting_original_or_copy() {
    let original = tempfile::tempdir().expect("original");
    let copied = tempfile::tempdir().expect("copied");
    let identity = owner();
    let store = Storage::open(&options(
        original.path(),
        identity.clone(),
        InstallationContext::FirstInstallation,
    ))
    .expect("install");
    drop(store);
    let original_custody = original.path().join("resource-custody");
    let copied_custody = copied.path().join("resource-custody");
    copy_installation(&original_custody, &copied_custody);
    let witness = std::fs::read(copied_custody.join("installation.json")).expect("copied witness");
    let database =
        std::fs::read(copied_custody.join("obligations.sqlite3")).expect("copied database");
    let error = Storage::open(&options(
        copied.path(),
        identity.clone(),
        InstallationContext::Existing,
    ))
    .err()
    .expect("no copied authority");
    assert_eq!(error.kind, ErrorKind::IdentityMismatch);
    assert_eq!(
        std::fs::read(copied_custody.join("installation.json")).expect("preserved witness"),
        witness
    );
    assert_eq!(
        std::fs::read(copied_custody.join("obligations.sqlite3")).expect("preserved database"),
        database
    );
    Storage::open(&options(
        original.path(),
        identity,
        InstallationContext::Existing,
    ))
    .expect("original authority unaffected");
}

#[cfg(unix)]
#[test]
fn same_path_directory_copy_replacement_fails_persisted_inode_binding() {
    let root = tempfile::tempdir().expect("fixture");
    let identity = owner();
    let store = Storage::open(&options(
        root.path(),
        identity.clone(),
        InstallationContext::FirstInstallation,
    ))
    .expect("install");
    drop(store);
    let custody = root.path().join("resource-custody");
    let preserved = root.path().join("preserved-custody");
    std::fs::rename(&custody, &preserved).expect("retain original native object");
    copy_installation(&preserved, &custody);
    let error = Storage::open(&options(
        root.path(),
        identity,
        InstallationContext::Existing,
    ))
    .err()
    .expect("same path is not same installation object");
    assert_eq!(error.kind, ErrorKind::IdentityMismatch);
    assert!(preserved.join("obligations.sqlite3").exists());
    assert!(custody.join("obligations.sqlite3").exists());
}
