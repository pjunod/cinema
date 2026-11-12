//! Original library-scan inputs share the admitted library job's byte budget; a
//! job with no source custody (a targeted probe) reads its named file directly.
use plurx_core::{
    error::ProbeError,
    scan::probe::{ProbeInput, ProbeInputProvider},
};
use std::{future::Future, path::Path, pin::Pin};

pub(super) struct LibraryProbeInputs(pub(super) crate::background_jobs::JobFence);

impl ProbeInputProvider for LibraryProbeInputs {
    fn prepare<'a>(
        &'a self,
        path: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<ProbeInput, ProbeError>> + Send + 'a>> {
        Box::pin(async move {
            let failure = |reason: String| ProbeError::Transient {
                path: path.display().to_string(),
                reason,
            };
            if !self
                .0
                .source_may_continue()
                .await
                .map_err(|error| failure(error.to_string()))?
                || plurx_core::process::bounded::cancellation()
                    .is_some_and(|cancel| cancel.is_cancelled())
            {
                return Err(failure("library source ownership interrupted".into()));
            }
            // New discoveries have no MediaFile yet. The no-follow held
            // descriptor owns identity; a full scan's job owns the physical
            // source domains, a targeted probe's owns none.
            let held = plurx_core::fs_secure::open_read_nofollow(path)
                .await
                .map_err(|error| failure(error.to_string()))?
                .into_std()
                .await;
            let source = crate::fragment_index_cluster::SourceFence::clone_current_held(&held)
                .map_err(&failure)?;
            let version = source.object_version().to_owned();
            if !self.0.holds_source_custody() {
                // A targeted probe takes no background source reader (contract
                // v2 §5.1, managed resource view v5), so its job registered no
                // source custody and the source service would never grant its
                // bridge a byte: every read would wait out the probe's wall
                // time. It reads the named file directly. Playback protection
                // still holds it, at the claim (`background_yield_reason`).
                // FFprobe opens the path, so the cleanup proves the path still
                // names the held, unmodified object: a fresh no-follow open of
                // the path and the held descriptor both carry the object
                // version (device, inode, size, mtime, ctime) read at open.
                drop(source);
                let logical_path = path.display().to_string();
                let probed = path.to_path_buf();
                return Ok(ProbeInput::new(path.to_path_buf(), async move {
                    if named_object_unchanged(&probed, &held, &version).await {
                        Ok(())
                    } else {
                        Err(ProbeError::Transient {
                            path: logical_path,
                            reason: "library source changed during probe".into(),
                        })
                    }
                }));
            }
            let input = crate::source_input::SourceInput::open_background_held(
                source,
                "application/octet-stream",
                self.0.source_budget(),
            )
            .await
            .map_err(&failure)?;
            let location = std::path::PathBuf::from(input.url());
            let logical_path = path.display().to_string();
            Ok(ProbeInput::new(location, async move {
                let closed = input.close_and_wait().await;
                let unchanged =
                    crate::fragment_index_cluster::SourceFence::clone_current_held(&held)
                        .is_ok_and(|current| current.object_version() == version);
                closed.map_err(|error| ProbeError::Transient {
                    path: logical_path.clone(),
                    reason: error.to_string(),
                })?;
                if !unchanged {
                    return Err(ProbeError::Transient {
                        path: logical_path,
                        reason: "library source changed during probe".into(),
                    });
                }
                Ok(())
            }))
        })
    }
}

/// Whether `path` still names the object `held` was opened on, unmodified:
/// a fresh no-follow open of the path and the held descriptor both carry
/// `version` (device, inode, size, mtime, ctime).
async fn named_object_unchanged(path: &Path, held: &std::fs::File, version: &str) -> bool {
    let current = |file: &std::fs::File| {
        crate::fragment_index_cluster::SourceFence::clone_current_held(file)
            .is_ok_and(|now| now.object_version() == version)
    };
    let named = match plurx_core::fs_secure::open_read_nofollow(path).await {
        Ok(file) => current(&file.into_std().await),
        Err(_) => false,
    };
    named && current(held)
}

#[cfg(test)]
mod tests {
    use super::named_object_unchanged;

    async fn opened(path: &std::path::Path) -> (std::fs::File, String) {
        let held = plurx_core::fs_secure::open_read_nofollow(path)
            .await
            .expect("open fixture")
            .into_std()
            .await;
        let version = crate::fragment_index_cluster::SourceFence::clone_current_held(&held)
            .expect("fixture object version")
            .object_version()
            .to_owned();
        (held, version)
    }

    /// A targeted probe reads the path, not its held descriptor: the cleanup
    /// accepts only the same, unmodified object still at that path.
    #[tokio::test]
    async fn a_direct_probe_accepts_only_the_unmodified_object_it_held() {
        let root = crate::test_tempdir().expect("root");
        let path = root.path().join("movie.mkv");
        std::fs::write(&path, b"original bytes").expect("fixture");

        let (held, version) = opened(&path).await;
        assert!(named_object_unchanged(&path, &held, &version).await);

        // Modified in place: same inode, new size/mtime/ctime.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut file| std::io::Write::write_all(&mut file, b" grown"))
            .expect("append");
        assert!(!named_object_unchanged(&path, &held, &version).await);

        // Replaced by another object at the same path.
        std::fs::write(&path, b"original bytes").expect("restore fixture");
        let (held, version) = opened(&path).await;
        let other = root.path().join("other.mkv");
        std::fs::write(&other, b"original bytes").expect("other");
        std::fs::rename(&other, &path).expect("replace");
        assert!(!named_object_unchanged(&path, &held, &version).await);

        // Removed, or replaced by a symlink to the held object.
        let (held, version) = opened(&path).await;
        let kept = root.path().join("kept.mkv");
        std::fs::rename(&path, &kept).expect("move away");
        assert!(!named_object_unchanged(&path, &held, &version).await);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&kept, &path).expect("symlink");
            assert!(!named_object_unchanged(&path, &held, &version).await);
        }
    }
}
