//! Actual blocking publication registration. Entry permission is released only
//! after the original JoinHandle is installed in the rendition's owner.
//! This local I/O fact is not durable acquisition or asset-close authority.
use super::*;

#[cfg(unix)]
impl PendingWrites {
    #[cfg(test)]
    fn test_barrier(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let mut barrier = self
            .native_barrier
            .lock()
            .expect("original publication fixture");
        assert!(barrier.is_none());
        let (acquired, observed) = tokio::sync::oneshot::channel();
        let (resume, release) = std::sync::mpsc::channel();
        *barrier = Some(NativePublicationBarrier { acquired, release });
        (observed, resume)
    }

    #[cfg(test)]
    fn run_native(
        &self,
        native: &std::sync::Arc<std::sync::Mutex<native_publication::PublicationIo>>,
    ) -> io::Result<()> {
        let barrier = self
            .native_barrier
            .lock()
            .expect("original publication fixture")
            .take();
        if let Some(barrier) = barrier {
            native
                .lock()
                .expect("original publication fixture")
                .acquire()?;
            let _ = barrier.acquired.send(());
            barrier
                .release
                .recv()
                .map_err(|_| io::Error::other("native test barrier closed after acquisition"))?;
            native
                .lock()
                .expect("original publication fixture")
                .publish_acquired()
        } else {
            native.lock().expect("original publication fixture").run()
        }
    }

    pub(super) fn check_native_result(&self) -> io::Result<()> {
        let retained = self
            .native
            .lock()
            .map_err(|_| io::Error::other("native publication owner poisoned"))?;
        if let Some(previous) = retained.as_ref() {
            let previous = previous
                .lock()
                .map_err(|_| io::Error::other("native publication inventory unknown"))?;
            if previous.has_unresolved_object() {
                return Err(io::Error::other(
                    "failed publication retains an unresolved native object",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
fn start_publication(
    owner: std::sync::Arc<PendingWrites>,
    directory: PathBuf,
    name: &str,
    bytes: Vec<u8>,
) -> io::Result<(PendingWrite, std::sync::mpsc::Sender<()>)> {
    start_reserved_publication(owner, directory, name, bytes, None)
}

fn start_reserved_publication(
    owner: std::sync::Arc<PendingWrites>,
    directory: PathBuf,
    name: &str,
    bytes: Vec<u8>,
    reservation: Option<crate::playback_cache::CacheReservation>,
) -> io::Result<(PendingWrite, std::sync::mpsc::Sender<()>)> {
    let publication_bytes = bytes.len() as u64;
    #[cfg(unix)]
    let native = {
        owner.check_native_result()?;
        let mut retained = owner
            .native
            .lock()
            .map_err(|_| io::Error::other("native publication owner poisoned"))?;

        let next = std::sync::Arc::new(std::sync::Mutex::new(
            native_publication::PublicationIo::new(directory, name, bytes)?,
        ));
        *retained = Some(std::sync::Arc::clone(&next));
        next
    };
    #[cfg(not(unix))]
    let (tmp, destination) = (
        directory.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4())),
        directory.join(name),
    );
    let (release, entry) = std::sync::mpsc::channel();
    let task = tokio::task::spawn_blocking(move || {
        let _owner = owner;
        entry.recv().map_err(|_| {
            io::Error::new(
                io::ErrorKind::Interrupted,
                "publication registration did not authorize native entry",
            )
        })?;
        // The existing global cache grant follows the actual blocking write,
        // including cancellation of its async waiter. It is disk-budget state,
        // not a finite native close receipt.
        let result = {
            #[cfg(unix)]
            {
                #[cfg(test)]
                {
                    _owner.run_native(&native)
                }
                #[cfg(not(test))]
                {
                    native
                        .lock()
                        .map_err(|_| io::Error::other("native publication inventory unknown"))?
                        .run()
                }
            }
            #[cfg(not(unix))]
            {
                // Legacy platform path; not no-follow or finite custody evidence.
                std::fs::write(&tmp, bytes)?;
                std::fs::rename(&tmp, destination)
            }
        };
        if result.is_ok() {
            if let Some(reservation) = reservation {
                reservation.commit(publication_bytes);
            }
        }
        result
    });
    Ok((PendingWrite { task }, release))
}

impl RenditionDir {
    pub(super) async fn publish_owned(&self, name: &str, bytes: Vec<u8>) -> io::Result<()> {
        #[cfg(unix)]
        if let Some(finite) = &self.writes.finite {
            return finite.publish(name, bytes).await;
        }
        // Segment callers already reserve before their manifest lock. Init
        // publication takes its existing global-cache grant before our lock.
        let reservation = if name == INIT_NAME {
            crate::playback_cache::reserve(self.path(), bytes.len() as u64).await?
        } else {
            None
        };
        self.publish_reserved_owned(name, bytes, reservation).await
    }

    /// Move an existing grant into the original blocking publication. The
    /// caller acquires it before its manifest/component locks; cancellation
    /// after registration must not release its bytes or namespace pin early.
    pub(super) async fn publish_reserved_owned(
        &self,
        name: &str,
        bytes: Vec<u8>,
        reservation: Option<crate::playback_cache::CacheReservation>,
    ) -> io::Result<()> {
        #[cfg(unix)]
        if let Some(finite) = &self.writes.finite {
            return finite.publish(name, bytes).await;
        }
        let mut pending = self.writes.pending.lock().await;
        join_pending_write(&mut pending).await?;
        let (write, release) = start_reserved_publication(
            std::sync::Arc::clone(&self.writes),
            self.dir.clone(),
            name,
            bytes,
            reservation,
        )?;
        // The actual native closure is waiting on entry. No await or physical
        // filesystem call occurs before retaining its original JoinHandle.
        *pending = Some(write);
        // A disconnected receiver is resolved by joining the retained task,
        // not by dropping the handle or treating send failure as native closure.
        let _ = release.send(());
        join_pending_write(&mut pending).await
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "Original-owner fixture assertions deliberately fail the test on the opposite result; production is checked separately."
)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_segment_waiter_keeps_original_cache_grant_until_blocking_publication_finishes(
    ) {
        use plurx_core::segplan::{
            PlanCut, PlanEntry, PlanEntryKind, SegmentPlan, SEGPLAN_VERSION,
        };
        use plurx_core::store::SqliteStore;
        use std::sync::Arc;

        let temporary = crate::test_tempdir().expect("real cache/publication fixture");
        let base = std::fs::canonicalize(temporary.path()).expect("canonical cache root");
        let cache_root = base.join("renditions");
        let directory = cache_root.join("a".repeat(64));
        std::fs::create_dir_all(&directory).expect("actual rendition namespace");
        let store = Arc::new(SqliteStore::open_in_memory().expect("real cache settings store"));
        let budget = crate::playback_cache::PlaybackCacheBudget::new(store, vec![cache_root]);
        budget.configure_for_test(1024 * 1024);
        let baseline = budget
            .used_bytes()
            .await
            .expect("actual baseline inventory");
        let dir = RenditionDir::new(directory);
        let payload = vec![0x5a; 64 * 1024];
        let len = payload.len() as u64;
        // The production caller reserves BEFORE taking the manifest lock.
        // This is the same real move-only cache grant passed to publication.
        let reservation = crate::playback_cache::reserve(dir.path(), len)
            .await
            .expect("real pre-publication cache grant")
            .expect("registered budget");
        let mut manifest = Manifest::new(SegmentPlan {
            version: SEGPLAN_VERSION,
            timescale: 1,
            entries: vec![PlanEntry {
                index: 0,
                kind: PlanEntryKind::Video,
                start_ticks: 0,
                duration_ticks: 1,
                est_bytes: len,
                cut: PlanCut::EndOfStream,
                fragments: 1,
            }],
            target_duration: 1,
        });
        let (acquired, resume) = dir.writes.test_barrier();
        {
            let write = dir.materialize_owned(&mut manifest, 0, payload, 0, Some(reservation));
            tokio::pin!(write);
            tokio::select! {
                _ = acquired => {},
                result = &mut write => panic!("original worker must wait with its actual File acquired: {result:?}"),
            }
            // Dropping this borrowed materialization frame cannot release the
            // grant: the registered original blocking worker now owns it.
        }
        assert!(
            dir.writes.pending.lock().await.is_some(),
            "same blocking task retained"
        );
        assert!(
            crate::playback_cache::is_pinned(dir.path()),
            "live native write still owns namespace pin"
        );
        assert!(
            budget.used_bytes().await.expect("held grant accounting") >= baseline + len,
            "future bytes remain charged even before the blocked worker writes them"
        );
        assert!(
            !manifest
                .state(0)
                .expect("same plan entry")
                .is_materialized(),
            "cancelled logical publication does not fabricate a manifest claim"
        );
        resume.send(()).expect("release original native writer");
        dir.join_writes()
            .await
            .expect("join same blocking publication");
        assert!(
            !crate::playback_cache::is_pinned(dir.path()),
            "worker settled its own grant"
        );
        assert!(
            budget
                .used_bytes()
                .await
                .expect("committed byte accounting")
                >= baseline + len
        );
        assert_eq!(
            std::fs::read(dir.path().join(segment_name(0))).expect("actual committed segment"),
            vec![0x5a; len as usize]
        );
    }

    #[tokio::test]
    async fn unregistered_native_publication_cannot_enter_and_original_handle_is_joined() {
        let root = crate::test_tempdir().unwrap();
        let dir = RenditionDir::new(root.path());
        let target = root.path().join(INIT_NAME);
        let (write, release) = start_publication(
            std::sync::Arc::clone(&dir.writes),
            root.path().to_owned(),
            INIT_NAME,
            b"owned bytes".to_vec(),
        )
        .unwrap();
        let mut pending = dir.writes.pending.lock().await;
        *pending = Some(write);
        // Closing the unopened latch deterministically proves this concrete
        // blocking closure cannot perform I/O. No sleeps/timeout-as-proof.
        drop(release);
        let error = join_pending_write(&mut pending).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(!target.exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        assert!(pending.is_none());
    }

    #[tokio::test]
    async fn registered_publication_moves_payload_and_joins_real_io_without_wrapper() {
        let root = crate::test_tempdir().unwrap();
        let dir = RenditionDir::new(root.path());
        let payload = b"moved owned payload".to_vec();
        dir.publish_owned(INIT_NAME, payload).await.unwrap();
        assert!(dir.writes.pending.lock().await.is_none());
        assert_eq!(
            tokio::fs::read(root.path().join(INIT_NAME)).await.unwrap(),
            b"moved owned payload"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_native_publication_waiter_retains_actual_open_file_and_original_job() {
        let root = crate::test_tempdir().unwrap();
        let dir = RenditionDir::new(root.path());
        let (write, entry) = start_publication(
            std::sync::Arc::clone(&dir.writes),
            root.path().into(),
            INIT_NAME,
            b"actual native writer".to_vec(),
        )
        .unwrap();
        let native = dir.writes.native.lock().unwrap().as_ref().unwrap().clone();
        let (acquired, resume) = dir.writes.test_barrier();
        *dir.writes.pending.lock().await = Some(write);
        entry.send(()).unwrap();
        acquired.await.unwrap();
        {
            let join = dir.join_writes();
            tokio::pin!(join);
            tokio::select! {biased; _=&mut join=>panic!("actual native writer is still blocked with an open file"), _=tokio::task::yield_now()=>{},}
        }
        assert!(
            dir.writes.pending.lock().await.is_some(),
            "cancelled borrow did not detach the original blocking handle"
        );
        resume.send(()).unwrap();
        dir.join_writes().await.unwrap();
        assert!(native.lock().unwrap().published());
        assert!(!native.lock().unwrap().has_unresolved_object());
        assert_eq!(
            std::fs::read(root.path().join(INIT_NAME)).unwrap(),
            b"actual native writer"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_actual_publication_keeps_native_custody_and_refuses_fresh_acquisition() {
        let root = crate::test_tempdir().unwrap();
        let dir = RenditionDir::new(root.path());
        std::fs::write(root.path().join(INIT_NAME), b"retained").unwrap();
        assert_eq!(
            dir.publish_owned(INIT_NAME, b"new".to_vec())
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(
            dir.writes.pending.lock().await.is_none(),
            "actual handle joined"
        );
        let operation = dir.writes.native.lock().unwrap().as_ref().unwrap().clone();
        let acquired = operation.lock().unwrap().acquired().unwrap().identity();
        assert!(operation.lock().unwrap().has_unresolved_object());
        assert!(
            dir.join_writes().await.is_err(),
            "a consumed task error cannot hide the retained native object"
        );
        assert!(dir
            .publish_owned("seg00000.m4s", b"later".to_vec())
            .await
            .is_err());
        assert!(!root.path().join("seg00000.m4s").exists());
        let retained = dir.writes.native.lock().unwrap().as_ref().unwrap().clone();
        assert!(std::sync::Arc::ptr_eq(&operation, &retained));
        assert!(retained
            .lock()
            .unwrap()
            .acquired()
            .unwrap()
            .identity()
            .same_inode(acquired));
        assert_eq!(
            std::fs::read(root.path().join(INIT_NAME)).unwrap(),
            b"retained"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn joined_success_closes_writable_copy_and_reuses_only_the_operation_cell() {
        let root = crate::test_tempdir().unwrap();
        let dir = RenditionDir::new(root.path());
        dir.publish_owned(INIT_NAME, b"init".to_vec())
            .await
            .unwrap();
        let first = dir.writes.native.lock().unwrap().as_ref().unwrap().clone();
        assert!(first.lock().unwrap().published());
        assert!(!first.lock().unwrap().has_unresolved_object());
        dir.publish_owned("seg00000.m4s", b"segment".to_vec())
            .await
            .unwrap();
        let second = dir.writes.native.lock().unwrap().as_ref().unwrap().clone();
        assert!(!std::sync::Arc::ptr_eq(&first, &second));
        assert!(!second.lock().unwrap().has_unresolved_object());
        assert_eq!(std::fs::read(root.path().join(INIT_NAME)).unwrap(), b"init");
        assert_eq!(
            std::fs::read(root.path().join("seg00000.m4s")).unwrap(),
            b"segment"
        );
    }
}
