//! Actual blocking publication registration. Entry permission is released only
//! after the original JoinHandle is installed in the rendition's owner.
//! This local I/O fact is not durable acquisition or asset-close authority.
use super::*;

#[cfg(unix)]
impl PendingWrites {
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

fn start_publication(
    owner: std::sync::Arc<PendingWrites>,
    directory: PathBuf,
    name: &str,
    bytes: Vec<u8>,
) -> io::Result<(PendingWrite, std::sync::mpsc::Sender<()>)> {
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
        #[cfg(unix)]
        {
            native
                .lock()
                .map_err(|_| io::Error::other("native publication inventory unknown"))?
                .run()
        }
        #[cfg(not(unix))]
        {
            // Legacy platform path; not no-follow or finite custody evidence.
            std::fs::write(&tmp, bytes)?;
            std::fs::rename(&tmp, destination)
        }
    });
    Ok((PendingWrite { task }, release))
}

impl RenditionDir {
    pub(super) async fn publish_owned(&self, name: &str, bytes: Vec<u8>) -> io::Result<()> {
        let mut pending = self.writes.pending.lock().await;
        join_pending_write(&mut pending).await?;
        let (write, release) = start_publication(
            std::sync::Arc::clone(&self.writes),
            self.dir.clone(),
            name,
            bytes,
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
mod tests {
    use super::*;

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
        let acquired = operation.lock().unwrap().acquired.unwrap().identity;
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
            .acquired
            .unwrap()
            .identity
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
        assert!(first.lock().unwrap().published);
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
