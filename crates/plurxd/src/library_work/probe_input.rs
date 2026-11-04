//! Original library-scan inputs share the admitted library job's byte budget.
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
            // New discoveries have no MediaFile yet. Their library job owns
            // the physical domains; the no-follow held descriptor owns identity.
            let held = plurx_core::fs_secure::open_read_nofollow(path)
                .await
                .map_err(|error| failure(error.to_string()))?
                .into_std()
                .await;
            let source = crate::fragment_index_cluster::SourceFence::clone_current_held(&held)
                .map_err(&failure)?;
            let version = source.object_version().to_owned();
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
