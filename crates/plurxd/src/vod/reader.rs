use super::*;

/// The copy recipe one rendition serves, minus `start_seconds` — exactly the
/// cache's key discipline (plan §2.4).
#[derive(Debug, Clone)]
pub(super) struct Recipe {
    pub(super) retained_logical: Option<super::retained_manifest::LogicalOutput>,
    pub(super) measured_candidate: Option<RetainedCandidateBinding>,
    pub(super) file: MediaFile,
    pub(super) audio_index: Option<i64>,
    pub(super) aac: bool,
    pub(super) audio_delivery: Option<plurx_core::playback::audio::AudioDelivery>,
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
    /// Admission purpose belongs to this consumer, not the cached recipe.
    pub(super) admission_priority: crate::admission::Priority,
    /// Voluntary Auto trials must not borrow the incumbent's permit.
    pub(super) nonpreemptive_trial: bool,
    /// A parent authority, cold controlled child or verified retained-output
    /// attachment keeps control identity without producing or an eviction window.
    pub(super) authority_only: bool,
    /// A durable prepared commit has moved playback to another incarnation.
    /// Keep cached bytes and admitted GETs protected while this reader drains,
    /// but do not encode ahead for its obsolete playback anchor.
    pub(super) draining_after_prepared_commit: bool,
    /// Current playback anchor, owned by accepted control once available.
    /// Before control arrives, successful media commits are the fallback.
    pub(super) frontier: u32,
    /// Optional target work stays at its append boundary while ordinary
    /// control continues to report the incumbent playback anchor.
    pub(super) preparation_frontier: Option<u32>,

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
    pub(super) _finite_slot: Option<super::client_state::ClientSlot>,
}

impl Reader {
    pub(super) fn new(frontier: u32) -> Self {
        Self::with_priority(frontier, crate::admission::Priority::Live)
    }

    pub(super) fn with_priority(
        frontier: u32,
        admission_priority: crate::admission::Priority,
    ) -> Self {
        Self {
            admission_priority,
            nonpreemptive_trial: false,
            authority_only: false,
            draining_after_prepared_commit: false,
            frontier,
            preparation_frontier: None,

            control_sequence: None,
            last_served: None,
            marker_prewarm: Arc::new(StdMutex::new(MarkerPrewarmLedger::default())),
            _finite_slot: None,
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn with_priority_finite(
        key: &str,
        frontier: u32,
        admission_priority: crate::admission::Priority,
        lease: &plurx_core::adaptive_resources::runtime::finite_vod::FiniteClientStateLease,
    ) -> std::io::Result<(String, Self)> {
        use plurx_core::segplan::MAX_TIMELINE_ANNOTATIONS;
        let records_bytes = MAX_TIMELINE_ANNOTATIONS
            .checked_mul(std::mem::size_of::<MarkerPrewarmRecord>())
            .ok_or_else(|| std::io::Error::other("finite marker records overflow"))?;
        let projected = key
            .len()
            .checked_add(std::mem::size_of::<StdMutex<MarkerPrewarmLedger>>())
            .and_then(|bytes| bytes.checked_add(records_bytes))
            .and_then(|bytes| bytes.checked_add(2 * std::mem::size_of::<usize>()))
            .ok_or_else(|| std::io::Error::other("finite reader size overflow"))?;
        let slot = super::client_state::ClientSlot(lease.reserve(projected)?);
        let mut stored_key = String::new();
        stored_key
            .try_reserve_exact(key.len())
            .map_err(std::io::Error::other)?;
        stored_key.push_str(key);
        let mut ledger = MarkerPrewarmLedger::default();
        ledger
            .records
            .try_reserve_exact(MAX_TIMELINE_ANNOTATIONS)
            .map_err(std::io::Error::other)?;
        let actual = stored_key
            .capacity()
            .checked_add(std::mem::size_of::<StdMutex<MarkerPrewarmLedger>>())
            .and_then(|bytes| {
                bytes.checked_add(
                    ledger
                        .records
                        .capacity()
                        .checked_mul(std::mem::size_of::<MarkerPrewarmRecord>())?,
                )
            })
            .and_then(|bytes| bytes.checked_add(2 * std::mem::size_of::<usize>()))
            .ok_or_else(|| std::io::Error::other("finite reader capacity overflow"))?;
        slot.validate_actual(actual)?;
        ledger._finite_slot = Some(slot.clone());
        Ok((
            stored_key,
            Self {
                admission_priority,
                nonpreemptive_trial: false,
                authority_only: false,
                draining_after_prepared_commit: false,
                frontier,
                preparation_frontier: None,
                control_sequence: None,
                last_served: None,
                marker_prewarm: Arc::new(StdMutex::new(ledger)),
                _finite_slot: Some(slot),
            },
        ))
    }

    pub(super) fn may_request_predecessor_handoff(&self) -> bool {
        !self.draining_after_prepared_commit
            && self.admission_priority == crate::admission::Priority::Speculative
            && !self.nonpreemptive_trial
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
    /// What the deadline found when it expired. Recorded once, so the parked
    /// waiters and every retry inside the same episode get the same answer.
    pub(super) verdict: Option<MaterializeVerdict>,
    pub(super) _finite_slot: Option<super::client_state::ClientSlot>,
}

/// Which phase an expired materialize deadline actually caught.
///
/// The deadline used to call every expiry a producer failure, including one
/// where the encoder pool had refused the permit and no producer ever ran,
/// which left a failure record that could not name the slow phase. The clock
/// and its budget are unchanged; only the verdict reads the rendition's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MaterializeVerdict {
    /// The rendition is waiting for an encoder permit the pool refused; no
    /// producer is running for the entry.
    Capacity(String),
    /// A producer ran (or none was started for another reason) and the entry
    /// never arrived from it.
    Producer(String),
    /// The producer delivered the entry's fragment; publication did not make
    /// it servable.
    Publication(String),
    Finite(Arc<crate::waitpool::FiniteWaitFailure>),
}

impl MaterializeVerdict {
    pub(super) fn message(&self) -> &str {
        match self {
            Self::Capacity(message) | Self::Producer(message) | Self::Publication(message) => {
                message
            }
            Self::Finite(failure) => failure.message(),
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn into_finite(
        self,
        slot: &super::client_state::ClientSlot,
    ) -> std::io::Result<Self> {
        use crate::waitpool::{FiniteFailureKind, FiniteWaitFailure};
        let (kind, message) = match self {
            Self::Capacity(message) => (FiniteFailureKind::Capacity, message),
            Self::Producer(message) => (FiniteFailureKind::Producer, message),
            Self::Publication(message) => (FiniteFailureKind::Publication, message),
            Self::Finite(failure) => return Ok(Self::Finite(failure)),
        };
        FiniteWaitFailure::new(
            kind,
            message,
            MATERIALIZE_EXPIRY_RETAINED,
            Arc::clone(&slot.0),
        )
        .map(Self::Finite)
    }

    pub(super) fn wait_outcome(&self) -> WaitOutcome {
        match self.clone() {
            Self::Capacity(message) => WaitOutcome::CapacityUnavailable(message),
            Self::Producer(message) => WaitOutcome::ProducerFailed(message),
            Self::Publication(message) => WaitOutcome::PublicationFailed(message),
            Self::Finite(failure) => WaitOutcome::FiniteMaterialization(failure),
        }
    }

    pub(super) fn into_error(self) -> VodError {
        match self {
            Self::Capacity(reason) => VodError::CapacityUnavailable {
                reason,
                retry_after: MATERIALIZE_EXPIRY_RETAINED,
            },
            Self::Producer(message) => VodError::ProducerFailed(message),
            Self::Publication(message) => VodError::PublicationFailed(message),
            Self::Finite(failure) => VodError::FiniteMaterialization(failure),
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) const FINITE_VERDICT_BYTES: usize = 512;

/// Format into a pre-reserved string. A producer refusal may contain variable
/// diagnostics; a long message is visibly truncated at a UTF-8 boundary
/// without changing its phase or growing the retained allocation.
#[cfg(target_os = "linux")]
struct BoundedVerdict(String, bool);

#[cfg(target_os = "linux")]
impl BoundedVerdict {
    fn new() -> std::io::Result<Self> {
        let mut value = String::new();
        value
            .try_reserve_exact(FINITE_VERDICT_BYTES)
            .map_err(std::io::Error::other)?;
        if value.capacity() > FINITE_VERDICT_BYTES {
            return Err(std::io::Error::other(
                "finite verdict allocation exceeded its slot",
            ));
        }
        Ok(Self(value, false))
    }
}

#[cfg(target_os = "linux")]
impl std::fmt::Write for BoundedVerdict {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        if self.1 {
            return Ok(());
        }
        if self
            .0
            .len()
            .checked_add(value.len())
            .is_some_and(|length| length <= FINITE_VERDICT_BYTES)
        {
            self.0.push_str(value);
            return Ok(());
        }
        // Preserve the original phase/classification and a useful diagnostic
        // prefix. Truncating text cannot turn capacity into producer failure.
        const SUFFIX: &str = "…";
        let max_prefix = FINITE_VERDICT_BYTES - SUFFIX.len();
        let mut retained = self.0.len().min(max_prefix);
        while !self.0.is_char_boundary(retained) {
            retained -= 1;
        }
        self.0.truncate(retained);
        let mut added = value.len().min(max_prefix - retained);
        while !value.is_char_boundary(added) {
            added -= 1;
        }
        self.0.push_str(&value[..added]);
        self.0.push_str(SUFFIX);
        self.1 = true;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl Rendition {
    pub(super) fn finite_materialize_verdict_bounded(
        &self,
        index: u32,
        belief: Producer,
    ) -> std::io::Result<MaterializeVerdict> {
        use std::fmt::Write as _;
        let mut text = BoundedVerdict::new()?;
        macro_rules! append {
            ($($args:tt)*) => {
                write!(&mut text, $($args)*)
                    .map_err(|_| std::io::Error::other("finite verdict exceeds client-state slot"))?
            };
        }
        if index == INIT_DEMAND_INDEX {
            append!("materializing {INIT_NAME}");
        } else {
            append!("materializing seg{index:05}.m4s");
        }
        append!(
            " exceeded the {:.1}s producer deadline",
            self.materialize_budget.as_secs_f64()
        );
        let producer_present =
            matches!(belief, Producer::Running { .. } | Producer::Stopped { .. });
        if !producer_present {
            if let Some(refusal) = self
                .recipe
                .encoding
                .as_ref()
                .filter(|encoding| encoding.is_waiting())
                .and_then(|encoding| encoding.last_refusal())
            {
                append!(" waiting for an encoder permit; no producer ran for it ({refusal})");
                return Ok(MaterializeVerdict::Capacity(text.0));
            }
        }
        let epoch = self.gen_epoch.load(Relaxed);
        let received = *self
            .fragment_received
            .lock()
            .expect("fragment receipt lock");
        let received = received
            .filter(|(at, _)| *at == epoch)
            .map(|(_, entry)| entry);
        if index != INIT_DEMAND_INDEX && received == Some(index) {
            append!(": the producer delivered it, but publication did not complete");
            return Ok(MaterializeVerdict::Publication(text.0));
        }
        match belief {
            Producer::Running { positioned_at, .. } => {
                append!(": the producer started at seg{positioned_at:05}.m4s did not deliver it; ");
            }
            Producer::Stopped {
                positioned_at,
                reason,
                ..
            } => {
                append!(
                    ": the producer started at seg{positioned_at:05}.m4s is stopped ({reason:?}); "
                );
            }
            Producer::Absent { .. } => {
                append!(": no producer is running for it");
                return Ok(MaterializeVerdict::Producer(text.0));
            }
        }
        if let Some(entry) = received {
            append!("its last delivered fragment was seg{entry:05}.m4s");
        } else {
            append!("it delivered no fragment");
        }
        Ok(MaterializeVerdict::Producer(text.0))
    }
}

/// How long an expired episode keeps answering with its verdict before an
/// abandoned clock is forgotten and a later GET starts a new episode.
pub(super) const MATERIALIZE_EXPIRY_RETAINED: Duration = PENDING_RETRY_AFTER.saturating_mul(2);

impl Drop for MaterializeClock {
    fn drop(&mut self) {
        // Cancel this original obsolete episode. Its future keeps the actual
        // client slot until Tokio destroys it; abort itself grants no credit.
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

    /// `None` while the episode's budget runs; afterwards the verdict the
    /// watchdog recorded, or -- when a request observes expiry before the
    /// watchdog task has run -- the same classification read now, from the
    /// same awaited producer state the watchdog reads. A contended
    /// `try_belief` used to stand in for it and read as "no producer", which
    /// answered capacity over a producer that was running whenever the
    /// encoding also had a permit wait in flight.
    pub(super) async fn expired_verdict(&self) -> Option<MaterializeVerdict> {
        if self.started.elapsed() < self.rendition.materialize_budget {
            return None;
        }
        let recorded = self
            .rendition
            .demand_since
            .lock()
            .expect("demand lock")
            .get(&self.index)
            .filter(|clock| clock.started == self.started)
            .and_then(|clock| clock.verdict.clone());
        if recorded.is_some() {
            return recorded;
        }
        let belief = self.rendition.producer_belief().await;
        #[cfg(target_os = "linux")]
        if self.rendition.finite_client_lease.is_some() {
            // The awaited belief precedes this short lock. Construct at most
            // one charged payload even when many retries observe expiry before
            // the registered deadline task is scheduled.
            let mut clocks = self.rendition.demand_since.lock().expect("demand lock");
            let clock = clocks
                .get_mut(&self.index)
                .filter(|clock| clock.started == self.started)?;
            if let Some(verdict) = &clock.verdict {
                return Some(verdict.clone());
            }
            let verdict = self
                .rendition
                .finite_materialize_verdict_bounded(self.index, belief)
                .and_then(|verdict| {
                    verdict.into_finite(clock._finite_slot.as_ref().ok_or_else(|| {
                        std::io::Error::other("finite demand has no original client slot")
                    })?)
                });
            return match verdict {
                Ok(verdict) => {
                    clock.verdict = Some(verdict.clone());
                    Some(verdict)
                }
                Err(error) => {
                    tracing::warn!(target: "plurxd::vodserve", %error,
                        "finite materialization verdict remains unavailable");
                    None
                }
            };
        }
        Some(self.rendition.materialize_verdict(self.index, belief))
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

#[cfg(all(test, target_os = "linux"))]
#[test]
fn finite_deadline_diagnostic_bounds_unicode_without_changing_capacity_class() {
    use std::fmt::Write as _;
    let mut text = BoundedVerdict::new().unwrap();
    write!(
        &mut text,
        "encoder permit unavailable: {}",
        "🎞".repeat(1024)
    )
    .unwrap();
    write!(&mut text, "ignored after truncation").unwrap();
    let verdict = MaterializeVerdict::Capacity(text.0);
    assert!(matches!(&verdict, MaterializeVerdict::Capacity(_)));
    assert!(verdict.message().starts_with("encoder permit unavailable:"));
    assert!(verdict.message().ends_with('…'));
    assert!(verdict.message().len() <= FINITE_VERDICT_BYTES);
    if let MaterializeVerdict::Capacity(message) = verdict {
        assert_eq!(message.capacity(), FINITE_VERDICT_BYTES);
    }
}
