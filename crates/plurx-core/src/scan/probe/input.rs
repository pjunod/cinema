//! Explicit input ownership for callers whose original media reads are mediated.
use crate::error::ProbeError;
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

type Cleanup = Pin<Box<dyn Future<Output = Result<(), ProbeError>> + Send>>;

/// A prepared input and its physical read settlement. The caller must await
/// the probe, including cancellation, rather than aborting its owning task.
pub struct ProbeInput {
    location: PathBuf,
    cleanup: Option<Cleanup>,
}

impl ProbeInput {
    /// Cleanup must join every actual reader before returning, including on
    /// probe errors and cancellation. It must not start a detached cleanup task.
    pub fn new(
        location: PathBuf,
        cleanup: impl Future<Output = Result<(), ProbeError>> + Send + 'static,
    ) -> Self {
        Self {
            location,
            cleanup: Some(Box::pin(cleanup)),
        }
    }

    pub(super) fn location(&self) -> &Path {
        &self.location
    }

    pub(super) async fn finish<T>(
        mut self,
        result: Result<T, ProbeError>,
    ) -> Result<T, ProbeError> {
        let settled = match self.cleanup.take() {
            Some(cleanup) => cleanup.await,
            None => Ok(()),
        };
        match result {
            Ok(value) => settled.map(|()| value),
            Err(error) => Err(error),
        }
    }
}

/// Prepares one held source capability for both stream and first-frame reads.
/// Spawned probe tasks must explicitly capture and reinstall this provider;
/// Tokio task-local scopes do not propagate across task spawn.
pub trait ProbeInputProvider: Send + Sync {
    fn prepare<'a>(
        &'a self,
        path: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<ProbeInput, ProbeError>> + Send + 'a>>;
}

tokio::task_local! { static INPUTS: Arc<dyn ProbeInputProvider>; }

pub fn probe_input_provider() -> Option<Arc<dyn ProbeInputProvider>> {
    INPUTS.try_with(Arc::clone).ok()
}

pub async fn with_probe_inputs<T>(
    provider: Arc<dyn ProbeInputProvider>,
    work: impl Future<Output = T>,
) -> T {
    INPUTS.scope(provider, work).await
}

pub(super) async fn prepare(path: &Path) -> Result<ProbeInput, ProbeError> {
    match probe_input_provider() {
        Some(provider) => provider.prepare(path).await,
        None => Ok(ProbeInput {
            location: path.to_path_buf(),
            cleanup: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct Provider {
        physical: PathBuf,
        prepared: Arc<AtomicUsize>,
        settled: Arc<AtomicUsize>,
    }
    impl ProbeInputProvider for Provider {
        fn prepare<'a>(
            &'a self,
            _path: &'a Path,
        ) -> Pin<Box<dyn Future<Output = Result<ProbeInput, ProbeError>> + Send + 'a>> {
            Box::pin(async move {
                self.prepared.fetch_add(1, Ordering::SeqCst);
                let settled = Arc::clone(&self.settled);
                Ok(ProbeInput::new(self.physical.clone(), async move {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    settled.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }))
            })
        }
    }

    #[tokio::test]
    async fn prepared_probe_preserves_logical_filename_and_awaits_physical_settlement() {
        let directory = tempfile::tempdir().expect("private probe input fixture directory");
        let logical = directory.path().join("catalog.mkv");
        let physical = directory.path().join("held-input.opaque");
        std::fs::write(&physical, b"fixture").expect("write held input fixture");
        let fixture = super::super::tests::install_fixture(
            &physical,
            super::super::tests::FixtureMode::SleepThenJson,
            Duration::from_secs(5),
        );
        fixture.calls.store(1, Ordering::SeqCst);
        let prepared = Arc::new(AtomicUsize::new(0));
        let settled = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(Provider {
            physical,
            prepared: Arc::clone(&prepared),
            settled: Arc::clone(&settled),
        });
        let result = with_probe_inputs(provider, super::super::probe(&logical))
            .await
            .expect("prepared input produces a valid probe result");
        assert_eq!(result.container.as_deref(), Some("mkv"));
        let json: serde_json::Value = serde_json::from_str(
            result
                .raw_json
                .as_deref()
                .expect("probe retains its logical document"),
        )
        .expect("retained probe document is valid JSON");
        assert_eq!(json["format"]["filename"].as_str(), logical.to_str());
        assert_eq!(prepared.load(Ordering::SeqCst), 1);
        assert_eq!(
            settled.load(Ordering::SeqCst),
            1,
            "probe must await bridge reader settlement"
        );
    }

    #[tokio::test]
    async fn fenced_probe_cancellation_joins_child_before_provider_cleanup_returns() {
        let directory = tempfile::tempdir().expect("private probe input fixture directory");
        let logical = directory.path().join("catalog.mkv");
        let physical = directory.path().join("held-input.opaque");
        std::fs::write(&physical, b"fixture").expect("write held input fixture");
        let fixture = super::super::tests::install_fixture(
            &physical,
            super::super::tests::FixtureMode::SleepThenJson,
            Duration::from_secs(30),
        );
        let prepared = Arc::new(AtomicUsize::new(0));
        let settled = Arc::new(AtomicUsize::new(0));
        let provider: Arc<dyn ProbeInputProvider> = Arc::new(Provider {
            physical,
            prepared,
            settled: Arc::clone(&settled),
        });
        let cancel = tokio_util::sync::CancellationToken::new();
        // Spawn explicitly propagates both provider and cancellation scopes.
        let task = tokio::spawn({
            let cancel = cancel.clone();
            async move {
                crate::process::bounded::cancellable(
                    cancel,
                    with_probe_inputs(provider, super::super::probe(&logical)),
                )
                .await
            }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while fixture.calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("probe reaches the owned child before cancellation");
        cancel.cancel();
        let error = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("cancelled probe settles within its cleanup bound")
            .expect("owned probe task joins without panic")
            .expect_err("cancelled probe cannot publish a result");
        assert!(matches!(error, ProbeError::Transient { .. }));
        assert_eq!(
            settled.load(Ordering::SeqCst),
            1,
            "lost fence must await provider cleanup rather than aborting it"
        );
    }
}
