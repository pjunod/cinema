//! A real node resource runtime for a test whose serving path needs one.
//!
//! The daemon opens custody only once it owns a selected Store and that
//! Store's retained daemon lock (`resource_runtime::open_before_cleanup`).
//! Finite VOD on Linux builds every rendition through the runtime, so a test
//! that serves one opens a real local singleton and runs that same startup
//! custody. The Store the test seeds and serves from stays its own: custody
//! binds to the singleton's daemon lock, which this fixture keeps alive.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use plurx_core::adaptive_resources::runtime::NodeResourceRuntime;

use crate::playback_cache::PlaybackCacheBudget;

pub(crate) struct RealRuntime {
    runtime: Arc<NodeResourceRuntime>,
    // The global playback budgets this fixture registered; the registry
    // holds them weakly, so the fixture keeps them for its whole life.
    playback_budgets: Mutex<Vec<Arc<PlaybackCacheBudget>>>,
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
            playback_budgets: Mutex::default(),
            _selected: selected,
            _root: root,
        }
    }

    pub(crate) fn handle(&self) -> Arc<NodeResourceRuntime> {
        Arc::clone(&self.runtime)
    }

    /// The global playback budget the daemon's `AppState` registers over its
    /// regenerable namespaces before it serves, for a test that builds a
    /// `VodServe` or manager without an `AppState`. Finite admission takes its
    /// operator quota from it (`playback_cache::reserve_finite`); register the
    /// serve or manager as its owner, as `AppState` does. It lives as long as
    /// this fixture.
    pub(crate) fn playback_budget(
        &self,
        store: Arc<dyn plurx_core::store::Store>,
        roots: Vec<PathBuf>,
    ) -> Arc<PlaybackCacheBudget> {
        let budget = PlaybackCacheBudget::new(store, roots);
        self.playback_budgets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Arc::clone(&budget));
        budget
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
