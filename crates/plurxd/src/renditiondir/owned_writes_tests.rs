#![allow(
    clippy::unwrap_used,
    reason = "Original-owner fixture assertions deliberately fail the test on the opposite result; production is checked separately."
)]
//! Authored real blocking-I/O custody regressions; run after final review only.
use super::*;
use std::sync::{mpsc, Arc};

#[tokio::test]
async fn cancelled_write_join_retains_the_actual_blocking_publication() {
    let root = crate::test_tempdir().unwrap();
    let dir = RenditionDir::new(root.path());
    let (release, blocked) = mpsc::channel();
    let (started, entered) = tokio::sync::oneshot::channel();
    let owner = Arc::clone(&dir.writes);
    let path = root.path().join(INIT_NAME);
    let task = tokio::task::spawn_blocking(move || {
        let _owner = owner;
        let _ = started.send(());
        blocked.recv().unwrap();
        std::fs::write(path, b"owned output")
    });
    *dir.writes.pending.lock().await = Some(PendingWrite { task });
    entered.await.unwrap();
    {
        let join = dir.join_writes();
        tokio::pin!(join);
        tokio::select! {biased; _=&mut join=>panic!("actual write still blocked"),_=tokio::task::yield_now()=>{},}
    }
    assert!(
        dir.writes.pending.lock().await.is_some(),
        "cancelled waiter did not take the handle"
    );
    release.send(()).unwrap();
    dir.join_writes().await.unwrap();
    assert!(dir.writes.pending.lock().await.is_none());
    assert_eq!(
        tokio::fs::read(root.path().join(INIT_NAME)).await.unwrap(),
        b"owned output"
    );
}

#[tokio::test]
async fn blocking_publication_owns_its_custody_until_all_os_work_ends() {
    let root = crate::test_tempdir().unwrap();
    let dir = RenditionDir::new(root.path());
    let weak = Arc::downgrade(&dir.writes);
    let owner = Arc::clone(&dir.writes);
    let (release, blocked) = mpsc::channel();
    let (started, entered) = tokio::sync::oneshot::channel();
    let (finished, done) = tokio::sync::oneshot::channel();
    let path = root.path().join(INIT_NAME);
    let task = tokio::task::spawn_blocking(move || {
        let _ = started.send(());
        blocked.recv().unwrap();
        let result = std::fs::write(path, b"still owned");
        drop(owner);
        let _ = finished.send(());
        result
    });
    *dir.writes.pending.lock().await = Some(PendingWrite { task });
    entered.await.unwrap();
    drop(dir);
    assert!(
        weak.upgrade().is_some(),
        "physical writer still owns the cell"
    );
    release.send(()).unwrap();
    done.await.unwrap();
    assert!(
        weak.upgrade().is_none(),
        "no permanent owner/task cycle after physical work"
    );
    assert_eq!(
        tokio::fs::read(root.path().join(INIT_NAME)).await.unwrap(),
        b"still owned"
    );
}

#[tokio::test]
async fn unconsumed_write_failure_is_joined_before_a_new_publication() {
    let root = crate::test_tempdir().unwrap();
    let dir = RenditionDir::new(root.path());
    let owner = Arc::clone(&dir.writes);
    let missing = root.path().join("absent/init.tmp");
    let task = tokio::task::spawn_blocking(move || {
        let _owner = owner;
        std::fs::write(missing, b"not published")
    });
    *dir.writes.pending.lock().await = Some(PendingWrite { task });
    assert!(
        dir.write_init(b"new output").await.is_err(),
        "failed original operation is not silently forgotten"
    );
    assert!(!root.path().join(INIT_NAME).exists());
    dir.write_init(b"new output").await.unwrap();
    dir.join_writes().await.unwrap();
    assert_eq!(
        tokio::fs::read(root.path().join(INIT_NAME)).await.unwrap(),
        b"new output"
    );
}
