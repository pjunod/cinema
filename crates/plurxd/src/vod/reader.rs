use super::*;

/// The copy recipe one rendition serves, minus `start_seconds` — exactly the
/// cache's key discipline (plan §2.4).
#[derive(Debug, Clone)]
pub(super) struct Recipe {
    pub(super) file: MediaFile,
    pub(super) audio_index: Option<i64>,
    pub(super) aac: bool,
    pub(super) video: CopyVideoOptions,
    /// Exact object version whose complete digest selected the cluster blob.
    /// None on the legacy node-local index path.
    pub(super) source_object_version: Option<String>,
    /// Content/pipeline identity selected by the v2 catalog. This becomes part
    /// of the rendition directory key so weak legacy metadata cannot alias
    /// segments across an in-place source rewrite.
    pub(super) cluster_cache_key: Option<String>,
    /// A frozen encoded strategy; None is the indexed compressed-copy path.
    pub(super) encoding: Option<Arc<crate::vodencode::Encoding>>,
}

/// One attached reader, in plan indexes.
#[derive(Debug, Clone)]
pub(super) struct Reader {
    /// Current playback anchor, owned by accepted control once available.
    /// Before control arrives, successful media commits are the fallback.
    pub(super) frontier: u32,
    pub(super) control_sequence: Option<u64>,
    /// The last segment actually served. Telemetry only: the observed runway
    /// the status publishes is measured from here, while both the eviction
    /// window and the production target are anchored on `frontier`, which
    /// accepted control owns. Reading this as the playhead would put a
    /// viewer's protected range back where their bytes came from rather than
    /// where they are now.
    pub(super) last_served: Option<u32>,
    /// Per-playback provenance for speculative marker production. This is
    /// deliberately separate from the ordinary reader frontier: moving the
    /// frontier to a marker would make speculative work outrank the playhead.
    pub(super) marker_prewarm: Arc<StdMutex<MarkerPrewarmLedger>>,
}

impl Reader {
    pub(super) fn new(frontier: u32) -> Self {
        Self {
            frontier,
            control_sequence: None,
            last_served: None,
            marker_prewarm: Arc::new(StdMutex::new(MarkerPrewarmLedger::default())),
        }
    }

    pub(super) fn accept_control(&mut self, sequence: u64, index: u32) {
        // ControlState already accepted the complete owner epoch/sequence.
        // A new owner's sequence one must replace the old owner's sequence N.
        self.control_sequence = Some(sequence);
        self.frontier = index;
    }

    pub(super) fn served(&mut self, index: u32) {
        self.last_served = Some(index);
        if self.control_sequence.is_none() {
            self.frontier = index;
        }
    }
}

/// Declared before a registered wait so cancellation first releases the wait
/// and its file pin, then wakes the scheduler to observe that exact removal.
pub(super) struct DemandWake(pub(super) Arc<Rendition>);

impl Drop for DemandWake {
    fn drop(&mut self) {
        self.0.kick();
    }
}

pub(super) struct MaterializeClock {
    pub(super) started: Instant,
    pub(super) owners: usize,
    pub(super) retry_pending: bool,
    pub(super) watchdog: Option<tokio::task::AbortHandle>,
}

impl Drop for MaterializeClock {
    fn drop(&mut self) {
        if let Some(watchdog) = &self.watchdog {
            watchdog.abort();
        }
    }
}

/// HTTP timeout may transfer the deadline to a retry; cancellation may not.
/// The last cancelled owner removes the clock synchronously, fencing its
/// sleeping watchdog from a later request for the same entry.
pub(super) struct MaterializeDemand {
    pub(super) rendition: Arc<Rendition>,
    pub(super) index: u32,
    pub(super) started: Instant,
}

impl MaterializeDemand {
    pub(super) fn retry_pending(&self) {
        let mut demands = self.rendition.demand_since.lock().expect("demand lock");
        if let Some(clock) = demands
            .get_mut(&self.index)
            .filter(|clock| clock.started == self.started)
        {
            clock.retry_pending = true;
        }
    }

    pub(super) fn expired(&self) -> bool {
        self.started.elapsed() >= self.rendition.materialize_budget
    }
}

impl Drop for MaterializeDemand {
    fn drop(&mut self) {
        let mut demands = self.rendition.demand_since.lock().expect("demand lock");
        if let Some(clock) = demands
            .get_mut(&self.index)
            .filter(|clock| clock.started == self.started)
        {
            clock.owners -= 1;
            if clock.owners == 0 && !clock.retry_pending {
                demands.remove(&self.index);
            }
        }
    }
}
