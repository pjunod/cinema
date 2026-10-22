//! Actual blocking publication registration. Entry permission is released only
//! after the original JoinHandle is installed in the rendition's owner.
//! This local I/O fact is not durable acquisition or asset-close authority.
use super::*;

fn start_publication(
    owner: std::sync::Arc<PendingWrites>,
    tmp: PathBuf,
    destination: PathBuf,
    bytes: Vec<u8>,
) -> (PendingWrite, std::sync::mpsc::Sender<()>) {
    let (release, entry) = std::sync::mpsc::channel();
    let task = tokio::task::spawn_blocking(move || {
        let _owner = owner;
        entry.recv().map_err(|_| {
            io::Error::new(
                io::ErrorKind::Interrupted,
                "publication registration did not authorize native entry",
            )
        })?;
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, destination)
    });
    (PendingWrite { task }, release)
}

impl RenditionDir {
    pub(super) async fn publish_owned(&self, name: &str, bytes: Vec<u8>) -> io::Result<()> {
        let mut pending = self.writes.pending.lock().await;
        join_pending_write(&mut pending).await?;
        let tmp = self.dir.join(format!("{name}.tmp"));
        let destination = self.dir.join(name);
        let (write, release) =
            start_publication(std::sync::Arc::clone(&self.writes), tmp, destination, bytes);
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
            root.path().join("entry.tmp"),
            target.clone(),
            b"owned bytes".to_vec(),
        );
        let mut pending = dir.writes.pending.lock().await;
        *pending = Some(write);
        // Closing the unopened latch deterministically proves this concrete
        // blocking closure cannot perform I/O. No sleeps/timeout-as-proof.
        drop(release);
        let error = join_pending_write(&mut pending).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(!target.exists());
        assert!(!root.path().join("entry.tmp").exists());
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
}
