//! A real node resource runtime for a test whose serving path needs one.
//!
//! The daemon opens custody only once it owns a selected Store and that
//! Store's retained daemon lock (`resource_runtime::open_before_cleanup`).
//! Finite VOD on Linux builds every rendition through the runtime, so a test
//! that serves one opens a real local singleton and runs that same startup
//! custody. The Store the test seeds and serves from stays its own: custody
//! binds to the singleton's daemon lock, which this fixture keeps alive.
use std::sync::Arc;

use plurx_core::adaptive_resources::runtime::NodeResourceRuntime;

pub(crate) struct RealRuntime {
    runtime: Arc<NodeResourceRuntime>,
    // Field order is drop order: custody goes before the lock that owns it,
    // and the lock before the directory that holds both.
    _selected: plurx_core::cluster::migration::SelectedStore,
    _root: tempfile::TempDir,
}

impl RealRuntime {
    pub(crate) async fn open() -> Self {
        let root = crate::test_tempdir().expect("runtime fixture root");
        let (mut config, _http) = crate::owned_loopback_config(&root.path().join("database"));
        crate::canonicalize_storage_roots(&mut config)
            .expect("production non-destructive storage preflight before Store ownership");
        let mut selected = crate::sharing_fixture_clock::select_applied_singleton(&config)
            .await
            .expect("actual selected local voter");
        let runtime = crate::resource_runtime::open_before_cleanup(&mut selected, &config)
            .await
            .expect("production custody startup")
            .expect("custody runtime");
        Self {
            runtime,
            _selected: selected,
            _root: root,
        }
    }

    pub(crate) fn handle(&self) -> Arc<NodeResourceRuntime> {
        Arc::clone(&self.runtime)
    }
}

/// The runtime this platform's finite serving path reads: Linux builds
/// finite renditions through it; elsewhere those paths never consult it, so
/// a test keeps exercising the platform's own shape.
pub(crate) async fn for_finite_serving() -> Option<RealRuntime> {
    if cfg!(target_os = "linux") {
        Some(RealRuntime::open().await)
    } else {
        None
    }
}
