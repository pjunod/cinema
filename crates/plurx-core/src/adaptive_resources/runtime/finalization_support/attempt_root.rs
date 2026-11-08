//! Fixed pre-P attempt custody. Requested storage only: allocator, executor,
//! constructor peaks and captured manager graphs remain Unknown.
use super::super::finite_vod::FiniteApplicationLease;
use super::*;
use bytes::Bytes;

pub const ATTEMPT_SLOTS: usize = 64;
pub const ENTRY_BYTES: usize = 16 * 1024;
pub const COORDINATOR_BYTES: usize = 512 * 1024;
pub const CONTROL_BYTES: usize = 256 * 1024;
pub const ENVELOPE_BYTES: usize = 256 * 1024;
pub const CLAIM_BYTES: usize = 32 * 1024;
pub const SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;
pub const REPLY_BYTES: usize = 96 * 1024;
pub const REPLY_PREVIEW_BYTES: usize = 32 * 1024;
pub const REPLY_WIRE_BYTES: usize = 256 * 1024;
// Independent ingress derivatives: one decoded envelope (256 KiB), at most
// eight bounded activation DTOs (256 KiB each), one actual activation frame
// (512 KiB), and bounded claim/recipe/preview copies. Store/cache graphs,
// allocator/executor cells and construction peaks remain Unknown.
pub const REPLY_ORIGIN_BYTES: usize = 4 * 1024 * 1024;
pub const FENCE_BYTES: usize = 96 * 1024;
const SERVICE_BYTES: usize = 64 * 1024;
const REPLY_SLOTS: usize = 64;
// The audited daemon adapter bounds actual capacities before capture. T below
// supplies destruction custody, NEVER a reachable-heap/profile/peak claim.
const DAEMON_ATTEMPT_BYTES: usize = ENTRY_BYTES
    + COORDINATOR_BYTES
    + CONTROL_BYTES
    + ENVELOPE_BYTES
    + 2 * CLAIM_BYTES
    + SNAPSHOT_BYTES
    + REPLY_BYTES
    + FENCE_BYTES;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Reserved,
    Active,
    Retiring,
    Destroying,
    Unknown,
}
struct Record {
    generation: u64,
    phase: Phase,
    original: Option<Box<dyn ActualAttempt>>,
    // Outside the object being destroyed and outside its unwinding frame.
    planning: Option<FiniteApplicationLease>,
}
trait ActualAttempt: Send + Sync {
    fn unique(&mut self) -> bool;
    fn destroy(self: Box<Self>);
}
struct Actual<T> {
    original: Arc<T>,
}
impl<T: Send + Sync + 'static> ActualAttempt for Actual<T> {
    fn unique(&mut self) -> bool {
        Arc::get_mut(&mut self.original).is_some()
    }
    fn destroy(self: Box<Self>) {
        let Self { original } = *self;
        let actual = Arc::try_unwrap(original)
            .unwrap_or_else(|_| unreachable!("private counted aliases were unique"));
        // No Weak/raw Arc escape. try_unwrap freed the actual outer header.
        drop(actual);
    }
}
struct ReplyRecord {
    generation: u64,
    phase: Phase,
    original: Option<Arc<()>>,
    bytes: Option<Bytes>,
}
struct Payload {
    identity: SupportIdentity,
    admission: Mutex<()>,
    records: [Mutex<Record>; ATTEMPT_SLOTS],
    replies: [Mutex<ReplyRecord>; REPLY_SLOTS],
    sealed: AtomicBool,
    stop: AtomicBool,
    changed: Notify,
    marker: SupportFailureMarker,
}
struct Storage {
    original: Arc<SupportPayload<Payload>>,
}
impl StoredPayload for Storage {
    fn unique(&mut self) -> bool {
        Arc::get_mut(&mut self.original).is_some()
    }
    fn destroy(self: Box<Self>) -> Option<SealIdentity> {
        let Self { original } = *self;
        let actual = Arc::try_unwrap(original)
            .unwrap_or_else(|_| unreachable!("private support aliases were unique"));
        drop(actual);
        None
    }
}

pub(in crate::adaptive_resources) fn requested_storage_bytes() -> Result<usize, AdmissionRefusal> {
    std::mem::size_of::<SupportState>()
        .checked_add(std::mem::size_of::<SupportPayload<Payload>>())
        .and_then(|n| n.checked_add(std::mem::size_of::<Storage>()))
        .and_then(|n| n.checked_add(SERVICE_BYTES))
        .and_then(|n| n.checked_add(8 * std::mem::size_of::<usize>()))
        .and_then(|n| n.checked_add(ATTEMPT_SLOTS * DAEMON_ATTEMPT_BYTES))
        .and_then(|n| {
            n.checked_add(
                REPLY_SLOTS
                    * (REPLY_BYTES
                        + REPLY_PREVIEW_BYTES
                        + REPLY_WIRE_BYTES
                        + REPLY_ORIGIN_BYTES
                        + 8 * std::mem::size_of::<usize>()),
            )
        })
        .ok_or(AdmissionRefusal::ArithmeticOverflow)
}

/// Unique shutdown authority; retain outside every captured manager graph.
#[derive(Default)]
pub struct AttemptRootLifecycle {
    lease: Option<FinalizationSupportLease>,
    client: Option<AttemptRootClient>,
}
#[derive(Clone)]
pub struct AttemptRootClient {
    payload: SupportPayloadRef<Payload>,
}
/// Original generation is paid before allocation is possible. Losing an
/// unallocated permit is the sole no-allocation path; no task/Arc exists yet.
pub struct AttemptPermit {
    client: AttemptRootClient,
    index: usize,
    generation: u64,
    armed: bool,
}
impl Drop for AttemptPermit {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Ok(mut row) = self.client.payload.records[self.index].lock() {
            if row.generation == self.generation
                && row.phase == Phase::Reserved
                && row.original.is_none()
            {
                row.phase = Phase::Idle;
                return;
            }
        }
        self.client.payload.marker.mark();
    }
}
/// No Arc, Weak, raw pointer, unwrap or mutable-payload accessor escapes.
pub struct AttemptRef<T> {
    actual: Option<Arc<T>>,
    client: AttemptRootClient,
    index: usize,
    generation: u64,
}
impl<T> Clone for AttemptRef<T> {
    fn clone(&self) -> Self {
        Self {
            actual: Some(Arc::clone(self.actual.as_ref().expect("live alias"))),
            client: self.client.clone(),
            index: self.index,
            generation: self.generation,
        }
    }
}
impl<T> Deref for AttemptRef<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.actual.as_deref().expect("live alias")
    }
}
impl<T> Drop for AttemptRef<T> {
    fn drop(&mut self) {
        drop(self.actual.take());
        self.client.payload.changed.notify_one();
    }
}
impl<T> AttemptRef<T> {
    pub fn same_original(&self, other: &Self) -> bool {
        self.client.payload.identity == other.client.payload.identity
            && self.index == other.index
            && self.generation == other.generation
    }
    pub fn mark_unknown(&self) {
        self.client.payload.marker.mark();
    }
    pub fn retain_planning_application(
        &self,
        lease: FiniteApplicationLease,
    ) -> Result<(), FiniteApplicationLease> {
        let Ok(mut row) = self.client.payload.records[self.index].lock() else {
            return Err(lease);
        };
        if row.generation != self.generation || row.phase != Phase::Active || row.planning.is_some()
        {
            return Err(lease);
        }
        row.planning = Some(lease);
        Ok(())
    }
    /// Daemon has destroyed rich/control payloads, positively joined original
    /// tasks and completed StoreClose. This only requests structural retirement;
    /// core itself destroys the actual object/header before permitting reuse.
    pub fn retire(&self) -> Result<(), AdmissionRefusal> {
        let mut row = self.client.payload.records[self.index]
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        if row.generation != self.generation || row.phase != Phase::Active {
            return Err(AdmissionRefusal::InvalidTransition);
        }
        row.phase = Phase::Retiring;
        self.client.payload.changed.notify_one();
        Ok(())
    }
}
impl AttemptPermit {
    pub fn install<T: Send + Sync + 'static>(
        mut self,
        make: impl FnOnce() -> T,
    ) -> Result<AttemptRef<T>, AdmissionRefusal> {
        if std::mem::size_of::<T>()
            .checked_add(std::mem::size_of::<Actual<T>>())
            .and_then(|n| n.checked_add(2 * std::mem::size_of::<usize>()))
            .is_none_or(|n| n > ENTRY_BYTES)
        {
            return Err(AdmissionRefusal::InvalidBundle);
        }
        // Pin failure outside a constructor which may allocate/panic.
        let mut pending = DestructionPending {
            marker: self.client.payload.marker.clone(),
            armed: true,
        };
        {
            let mut row = self.client.payload.records[self.index]
                .lock()
                .map_err(|_| AdmissionRefusal::Unavailable)?;
            if row.generation != self.generation || row.phase != Phase::Reserved {
                return Err(AdmissionRefusal::InvalidTransition);
            }
            row.phase = Phase::Active;
        }
        self.armed = false;
        let actual = Arc::new(make());
        let result = AttemptRef {
            actual: Some(Arc::clone(&actual)),
            client: self.client.clone(),
            index: self.index,
            generation: self.generation,
        };
        self.client.payload.records[self.index]
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?
            .original = Some(Box::new(Actual { original: actual }));
        pending.armed = false;
        Ok(result)
    }
}
impl AttemptRootClient {
    pub fn reserve(&self) -> Result<AttemptPermit, AdmissionRefusal> {
        let _admission = self
            .payload
            .admission
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        if self.payload.sealed.load(Ordering::Acquire)
            || self.payload.marker.ledger.load(Ordering::Acquire)
        {
            return Err(AdmissionRefusal::Unavailable);
        }
        for (index, record) in self.payload.records.iter().enumerate() {
            let mut row = record.lock().map_err(|_| AdmissionRefusal::Unavailable)?;
            if row.phase != Phase::Idle {
                continue;
            }
            row.generation = row
                .generation
                .checked_add(1)
                .ok_or(AdmissionRefusal::IdentityExhausted)?;
            row.phase = Phase::Reserved;
            return Ok(AttemptPermit {
                client: self.clone(),
                index,
                generation: row.generation,
                armed: true,
            });
        }
        Err(AdmissionRefusal::Unavailable)
    }
    pub fn seal(&self) {
        match self.payload.admission.lock() {
            Ok(_admission) => self.payload.sealed.store(true, Ordering::Release),
            Err(_) => self.payload.marker.mark(),
        }
    }
    pub fn held(&self) -> usize {
        self.payload
            .records
            .iter()
            .filter(|record| record.lock().map_or(true, |row| row.phase != Phase::Idle))
            .count()
    }
    pub fn reserve_reply(&self) -> Result<ReplyPermit, AdmissionRefusal> {
        let _admission = self
            .payload
            .admission
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        if self.payload.sealed.load(Ordering::Acquire)
            || self.payload.marker.ledger.load(Ordering::Acquire)
        {
            return Err(AdmissionRefusal::Unavailable);
        }
        for (index, record) in self.payload.replies.iter().enumerate() {
            let mut row = record.lock().map_err(|_| AdmissionRefusal::Unavailable)?;
            if row.phase != Phase::Idle {
                continue;
            }
            row.generation = row
                .generation
                .checked_add(1)
                .ok_or(AdmissionRefusal::IdentityExhausted)?;
            row.phase = Phase::Reserved;
            let mut pending = DestructionPending {
                marker: self.payload.marker.clone(),
                armed: true,
            };
            let original = Arc::new(());
            let alias = Arc::clone(&original);
            row.original = Some(original);
            row.phase = Phase::Active;
            pending.armed = false;
            return Ok(ReplyPermit {
                client: self.clone(),
                index,
                generation: row.generation,
                alias: Some(alias),
                can_fork: true,
            });
        }
        Err(AdmissionRefusal::Unavailable)
    }
}
/// One separately admitted DTO and serialized backing. Core retains the actual
/// original Bytes allocation; caller cannot assert final destruction by Drop.
pub struct ReplyPermit {
    client: AttemptRootClient,
    index: usize,
    generation: u64,
    alias: Option<Arc<()>>,
    can_fork: bool,
}
impl ReplyPermit {
    /// Exactly two audited holders: the origin response and its original
    /// activation. Core keeps the original token/header until both die.
    pub fn fork_origin(&mut self) -> Result<Self, AdmissionRefusal> {
        if !self.can_fork {
            return Err(AdmissionRefusal::InvalidTransition);
        }
        self.can_fork = false;
        Ok(Self {
            client: self.client.clone(),
            index: self.index,
            generation: self.generation,
            alias: Some(Arc::clone(
                self.alias
                    .as_ref()
                    .ok_or(AdmissionRefusal::InvalidTransition)?,
            )),
            can_fork: false,
        })
    }
    pub fn publish_serialized(self, bytes: Vec<u8>) -> Result<Bytes, AdmissionRefusal> {
        if bytes.is_empty()
            || bytes
                .capacity()
                .checked_add(8 * std::mem::size_of::<usize>())
                .is_none_or(|n| n > REPLY_WIRE_BYTES)
        {
            drop(bytes);
            return Err(AdmissionRefusal::InvalidBundle);
        }
        let mut pending = DestructionPending {
            marker: self.client.payload.marker.clone(),
            armed: true,
        };
        let actual = Bytes::from(bytes);
        let result = actual.clone();
        let mut row = self.client.payload.replies[self.index]
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        if row.generation != self.generation || row.phase != Phase::Active || row.bytes.is_some() {
            return Err(AdmissionRefusal::ReceiptMismatch);
        }
        row.bytes = Some(actual);
        pending.armed = false;
        Ok(result)
    }
}
impl Drop for ReplyPermit {
    fn drop(&mut self) {
        // Owned payloads precede this final field. A destructor panic is not
        // physical closure even when Rust then drops this alias while unwinding.
        if std::thread::panicking() {
            self.client.payload.marker.mark();
        }
        drop(self.alias.take());
        self.client.payload.changed.notify_one();
    }
}
fn replies_held(payload: &Payload) -> usize {
    payload
        .replies
        .iter()
        .filter(|record| record.lock().map_or(true, |row| row.phase != Phase::Idle))
        .count()
}

impl AttemptRootLifecycle {
    pub async fn start(
        &mut self,
        runtime: &Arc<NodeResourceRuntime>,
        deadline: Instant,
    ) -> Result<AttemptRootClient, AdmissionRefusal> {
        fn future_size<F>(_: fn(SupportPayloadRef<Payload>) -> F) -> usize {
            std::mem::size_of::<F>()
        }
        if future_size(run_root) > SERVICE_BYTES {
            return Err(AdmissionRefusal::InvalidBundle);
        }
        if self.lease.is_some() {
            return Err(AdmissionRefusal::InvalidTransition);
        }
        let executor =
            tokio::runtime::Handle::try_current().map_err(|_| AdmissionRefusal::Unavailable)?;
        self.lease = Some(
            runtime
                .reserve_observed_support(FinalizationSupportProfile::attempt()?, deadline)
                .await?,
        );
        let lease = self.lease.as_mut().expect("reserved root");
        let identity = lease.identity;
        let marker = lease.marker.clone();
        let payload = lease.activate_registered(
            executor,
            move || Payload {
                identity,
                admission: Mutex::new(()),
                records: std::array::from_fn(|_| {
                    Mutex::new(Record {
                        generation: 0,
                        phase: Phase::Idle,
                        original: None,
                        planning: None,
                    })
                }),
                replies: std::array::from_fn(|_| {
                    Mutex::new(ReplyRecord {
                        generation: 0,
                        phase: Phase::Idle,
                        original: None,
                        bytes: None,
                    })
                }),
                sealed: AtomicBool::new(false),
                stop: AtomicBool::new(false),
                changed: Notify::new(),
                marker,
            },
            run_root,
            |original| Storage { original },
        )?;
        let client = AttemptRootClient { payload };
        self.client = Some(client.clone());
        Ok(client)
    }
    pub fn begin_drain(&self) {
        if let Some(client) = &self.client {
            client.seal();
        }
    }
    pub async fn shutdown(&mut self, deadline: Instant) -> Result<(), AdmissionRefusal> {
        if let Some(client) = self.client.as_ref() {
            client.seal();
            // Keep the same original service and publication alive while
            // transport aliases drain. Cancellation or expiry leaves this
            // exact lease/client available for a later shutdown observer.
            tokio::time::timeout_at(deadline.into(), async {
                while client.held() != 0 || replies_held(&client.payload) != 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .map_err(|_| AdmissionRefusal::Unavailable)?;
            client.payload.stop.store(true, Ordering::Release);
            client.payload.changed.notify_one();
            // Drop only this external alias. The same original lease and JH
            // survive cancellation/timeout and are retried below independently.
            self.client = None;
        }
        if self.lease.is_none() {
            return Ok(());
        }
        let lease = self
            .lease
            .as_mut()
            .ok_or(AdmissionRefusal::InvalidTransition)?;
        tokio::time::timeout_at(deadline.into(), lease.join_original())
            .await
            .map_err(|_| AdmissionRefusal::Unavailable)??;
        lease.try_close()?;
        self.lease = None;
        Ok(())
    }
}
async fn run_root(payload: SupportPayloadRef<Payload>) {
    let mut actor_failure = DestructionPending {
        marker: payload.marker.clone(),
        armed: true,
    };
    loop {
        let changed = payload.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        for record in &payload.records {
            let original = {
                let mut row = record.lock().expect("original root record");
                if row.phase != Phase::Retiring
                    || !row.original.as_mut().is_some_and(|actual| actual.unique())
                {
                    continue;
                }
                row.phase = Phase::Destroying;
                row.original.take().expect("original actual payload")
            };
            let mut pending = DestructionPending {
                marker: payload.marker.clone(),
                armed: true,
            };
            // No record/census/ledger mutex while T drops. The original M lease
            // remains in the root record if this destructor unwinds.
            original.destroy();
            let planning = {
                let mut row = record.lock().expect("same original root record");
                if row.phase != Phase::Destroying {
                    row.phase = Phase::Unknown;
                    payload.marker.mark();
                    return;
                }
                row.planning.take()
            };
            drop(planning);
            // Header + actual T + original M fence have now really left.
            record.lock().expect("same original generation").phase = Phase::Idle;
            pending.armed = false;
        }
        if payload.marker.ledger.load(Ordering::Acquire) {
            return;
        }
        for record in &payload.replies {
            let token = {
                let mut row = record.lock().expect("original response record");
                if row.phase == Phase::Active {
                    if !row
                        .original
                        .as_mut()
                        .is_some_and(|original| Arc::get_mut(original).is_some())
                    {
                        continue;
                    }
                    row.phase = Phase::Retiring;
                    row.original.take()
                } else {
                    None
                }
            };
            if let Some(token) = token {
                // The actual original opaque token's outer Arc header is gone
                // before its record can become reusable; no Drop receipt.
                Arc::try_unwrap(token)
                    .unwrap_or_else(|_| unreachable!("opaque response aliases were unique"));
            }
            let original = {
                let mut row = record.lock().expect("original response record");
                if row.phase != Phase::Retiring {
                    continue;
                }
                let Some(original) = row.bytes.take() else {
                    row.phase = Phase::Idle;
                    continue;
                };
                original
            };
            // bytes 1.12.1: refusal returns the SAME original Bytes; success is
            // no-copy and frees Shared/promotable header before BytesMut return.
            match original.try_into_mut() {
                Ok(actual) => {
                    drop(actual); // actual backing is gone before slot reuse
                    record.lock().expect("original response record").phase = Phase::Idle;
                }
                Err(original) => {
                    record.lock().expect("same original response record").bytes = Some(original)
                }
            }
        }
        if payload.stop.load(Ordering::Acquire) {
            actor_failure.armed = false;
            return;
        }
        // Timer only schedules another real try_into_mut uniqueness operation;
        // elapsed time and missing notifications never supply destruction proof.
        tokio::select! { _ = &mut changed => {}, _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {} }
    }
}

/// Actual journal plus fixed-capacity root for bookkeeping unit tests only.
/// This is absent from shipping builds and supplies no native cleanup evidence.
#[cfg(feature = "fixtures")]
pub async fn bookkeeping_fixture() -> (
    tempfile::TempDir,
    Arc<NodeResourceRuntime>,
    AttemptRootLifecycle,
    AttemptRootClient,
) {
    use crate::resource_custody::{DaemonLockGuard, InstallationContext, OpenOptions};
    let root = tempfile::tempdir().expect("protected fixture TMPDIR");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.path().join("attempt.lock"))
        .expect("fixture lock");
    let runtime = Arc::new(
        NodeResourceRuntime::open(OpenOptions {
            data_dir: std::fs::canonicalize(root.path()).expect("canonical fixture"),
            excluded_roots: Vec::new(),
            installation: InstallationContext::FirstInstallation,
            owner: super::super::daemon_owner("attempt-fixture".into(), "fixture-cluster".into()),
            daemon_lock: DaemonLockGuard::from_held_lock(file),
            queue_capacity: 4,
        })
        .expect("actual journal"),
    );
    runtime
        .ledger
        .observe_capacity(
            Metric::Known(ResourceVector {
                peak_memory_bytes: 1024 * 1024 * 1024,
                ..ResourceVector::default()
            })
            .into(),
            Instant::now(),
        )
        .expect("explicit fixture requested storage capacity");
    let mut lifecycle = AttemptRootLifecycle {
        lease: Some(
            runtime
                .reserve_support(
                    FinalizationSupportProfile::attempt().expect("fixed profile"),
                    runtime.ledger.snapshot().expect("ledger").limit_epoch,
                )
                .expect("fixed root admission"),
        ),
        ..Default::default()
    };
    let lease = lifecycle.lease.as_mut().expect("actual original root");
    let identity = lease.identity;
    let marker = lease.marker.clone();
    let payload = lease
        .activate_registered(
            tokio::runtime::Handle::current(),
            move || Payload {
                identity,
                admission: Mutex::new(()),
                records: std::array::from_fn(|_| {
                    Mutex::new(Record {
                        generation: 0,
                        phase: Phase::Idle,
                        original: None,
                        planning: None,
                    })
                }),
                replies: std::array::from_fn(|_| {
                    Mutex::new(ReplyRecord {
                        generation: 0,
                        phase: Phase::Idle,
                        original: None,
                        bytes: None,
                    })
                }),
                sealed: AtomicBool::new(false),
                stop: AtomicBool::new(false),
                changed: Notify::new(),
                marker,
            },
            run_root,
            |original| Storage { original },
        )
        .expect("actual fixed root allocation");
    let client = AttemptRootClient { payload };
    lifecycle.client = Some(client.clone());
    (root, runtime, lifecycle, client)
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::super::super::finite_lifetime::FiniteAppState;
    use super::*;
    struct ActualDrop {
        dropped: Arc<AtomicBool>,
        app: Arc<FiniteAppState>,
        panic: bool,
    }
    impl Drop for ActualDrop {
        fn drop(&mut self) {
            assert!(
                !self.app.released(),
                "original planning fence survives actual object/header destruction"
            );
            self.dropped.store(true, Ordering::Release);
            assert!(!self.panic, "actual payload destructor panic");
        }
    }
    #[tokio::test]
    async fn original_entry_alias_blocks_actual_destruction_planning_release_and_generation_reuse()
    {
        let (_dir, runtime, mut root, client) = bookkeeping_fixture().await;
        let used = runtime.accounting().expect("actual paid baseline").used;
        let app = FiniteAppState::with_application_limit(128);
        let planning = app
            .take_application_lease(128)
            .expect("same original planning lease");
        app.seal();
        let dropped = Arc::new(AtomicBool::new(false));
        let original = client
            .reserve()
            .expect("paid before actual allocation")
            .install(|| ActualDrop {
                dropped: Arc::clone(&dropped),
                app: Arc::clone(&app),
                panic: false,
            })
            .expect("actual original Arc");
        let index = original.index;
        let generation = original.generation;
        original
            .retain_planning_application(planning)
            .unwrap_or_else(|_| panic!("original fence retained outside actual object"));
        let escaped = original.clone();
        original
            .retire()
            .expect("original terminal structural retirement requested");
        drop(original);
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(!dropped.load(Ordering::Acquire));
        assert!(!app.released());
        let mut other = Vec::new();
        for _ in 1..ATTEMPT_SLOTS {
            other.push(client.reserve().expect("other fixed slots"));
        }
        assert!(
            client.reserve().is_err(),
            "alias prevents original generation reuse"
        );
        assert_eq!(
            runtime.accounting().expect("same structural hold").used,
            used
        );
        drop(escaped);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !app.released() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actual final destruction");
        assert!(dropped.load(Ordering::Acquire));
        let replacement = client.reserve().expect("actual destruction permits reuse");
        assert_eq!(replacement.index, index);
        assert!(replacement.generation > generation);
        drop((replacement, other, client));
        root.shutdown(Instant::now() + std::time::Duration::from_secs(1))
            .await
            .expect("original root join and actual support destruction");
        assert!(runtime
            .accounting()
            .expect("released")
            .attempt_support
            .is_none());
    }
    #[tokio::test]
    async fn actual_entry_destructor_panic_keeps_original_planning_fence_and_unknown_support() {
        let (_dir, runtime, mut root, client) = bookkeeping_fixture().await;
        let used = runtime.accounting().expect("paid baseline").used;
        let app = FiniteAppState::with_application_limit(128);
        let planning = app.take_application_lease(128).expect("original fence");
        app.seal();
        let dropped = Arc::new(AtomicBool::new(false));
        let original = client
            .reserve()
            .expect("paid slot")
            .install(|| ActualDrop {
                dropped: Arc::clone(&dropped),
                app: Arc::clone(&app),
                panic: true,
            })
            .expect("actual payload");
        original
            .retain_planning_application(planning)
            .unwrap_or_else(|_| panic!("original fence retained"));
        original.retire().expect("retirement requested");
        drop(original);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !dropped.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actual destructor entered");
        assert!(!app.released());
        assert!(client.reserve().is_err());
        let snapshot = runtime.accounting().expect("retained charge");
        assert_eq!(snapshot.used, used);
        assert_eq!(
            snapshot
                .attempt_support
                .expect("original support remains")
                .phase,
            FinalizationSupportPhase::UnknownHeld
        );
        assert!(root.shutdown(Instant::now()).await.is_err());
    }
    #[tokio::test]
    async fn preallocation_cancellation_and_seal_refuse_all_new_attempt_and_reply_allocations() {
        let (_dir, _runtime, mut root, client) = bookkeeping_fixture().await;
        let unallocated = client.reserve().expect("paid no-allocation permit");
        assert_eq!(client.held(), 1);
        drop(unallocated);
        assert_eq!(client.held(), 0);
        let replies: Vec<_> = (0..REPLY_SLOTS)
            .map(|_| client.reserve_reply().expect("separate copy admission"))
            .collect();
        assert!(client.reserve_reply().is_err());
        client.seal();
        assert!(client.reserve().is_err());
        assert!(client.reserve_reply().is_err());
        drop((replies, client));
        root.shutdown(Instant::now() + std::time::Duration::from_secs(1))
            .await
            .expect("actual no-attempt root shutdown");
    }
    #[tokio::test]
    async fn cancelled_shutdown_observer_retries_original_join_and_actual_support_close() {
        let (_dir, runtime, mut root, client) = bookkeeping_fixture().await;
        drop(client);
        let identity = root.lease.as_ref().expect("original lease").identity;
        {
            let first = root.shutdown(Instant::now() + std::time::Duration::from_secs(1));
            tokio::pin!(first);
            std::future::poll_fn(|cx| {
                assert!(
                    first.as_mut().poll(cx).is_pending(),
                    "original service has not run its stop yet"
                );
                Poll::Ready(())
            })
            .await;
        } // Cancel only this observer, retaining original root lease/handle.
        assert!(root.client.is_none());
        assert_eq!(
            root.lease.as_ref().expect("original retained").identity,
            identity
        );
        root.shutdown(Instant::now() + std::time::Duration::from_secs(1))
            .await
            .expect("same original continuation");
        assert!(runtime
            .accounting()
            .expect("actual released support")
            .attempt_support
            .is_none());
    }
    #[tokio::test]
    async fn serialized_reply_clones_keep_original_header_and_backing_charged_until_actual_unique_drop(
    ) {
        let (_dir, runtime, mut root, client) = bookkeeping_fixture().await;
        let before = runtime.accounting().expect("paid root").used;
        let permit = client.reserve_reply().expect("independent response copy");
        let bytes = permit
            .publish_serialized(vec![42; 1024])
            .expect("actual serialized backing");
        let escaped = bytes.slice(1..4);
        drop(bytes);
        let other: Vec<_> = (1..REPLY_SLOTS)
            .map(|_| client.reserve_reply().expect("other fixed copy records"))
            .collect();
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        assert!(
            client.reserve_reply().is_err(),
            "timer cannot certify absent backing aliases"
        );
        assert_eq!(
            runtime.accounting().expect("same original hold").used,
            before
        );
        drop(escaped);
        let replacement = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if let Ok(permit) = client.reserve_reply() {
                    break permit;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actual unique header/backing destruction permits reuse");
        drop((other, replacement, client));
        root.shutdown(Instant::now() + std::time::Duration::from_secs(1))
            .await
            .expect("actual final root release");
    }
    #[tokio::test]
    async fn shutdown_waits_for_actual_response_backing_and_retry_keeps_original_lease() {
        let (_dir, runtime, mut root, client) = bookkeeping_fixture().await;
        let identity = root.lease.as_ref().expect("same original root").identity;
        let bytes = client
            .reserve_reply()
            .expect("paid response copy")
            .publish_serialized(vec![42; 1024])
            .expect("actual serialized backing");
        drop(client);
        {
            let first = root.shutdown(Instant::now() + std::time::Duration::from_secs(1));
            tokio::pin!(first);
            std::future::poll_fn(|cx| {
                assert!(
                    first.as_mut().poll(cx).is_pending(),
                    "held transport backing waits within the shutdown allowance"
                );
                Poll::Ready(())
            })
            .await;
        } // Cancel this observer while the SAME original actor keeps custody.
        assert!(root.client.is_some());
        assert_eq!(
            root.lease
                .as_ref()
                .expect("same lease after cancellation")
                .identity,
            identity
        );
        assert!(
            root.shutdown(Instant::now()).await.is_err(),
            "expiry cannot certify backing destruction"
        );
        assert_eq!(
            root.lease
                .as_ref()
                .expect("same lease after expiry")
                .identity,
            identity
        );
        assert!(runtime
            .accounting()
            .expect("original charge retained")
            .attempt_support
            .is_some());
        let retry = root.shutdown(Instant::now() + std::time::Duration::from_secs(1));
        tokio::pin!(retry);
        std::future::poll_fn(|cx| {
            assert!(
                retry.as_mut().poll(cx).is_pending(),
                "retry still waits on the actual original response"
            );
            Poll::Ready(())
        })
        .await;
        drop(bytes);
        retry.await.expect(
            "actual response backing destruction permits original root closure within the deadline",
        );
        assert!(runtime
            .accounting()
            .expect("actual support released")
            .attempt_support
            .is_none());
    }

    #[tokio::test]
    async fn origin_payload_destructor_panic_keeps_actual_original_token_and_unknown_charge() {
        struct PanicPayload;
        impl Drop for PanicPayload {
            fn drop(&mut self) {
                panic!("actual origin payload destructor panic");
            }
        }
        struct OriginOwner {
            _actual: PanicPayload,
            _funding: ReplyPermit,
        }
        let (_dir, runtime, mut root, client) = bookkeeping_fixture().await;
        let before = runtime.accounting().expect("actual paid root").used;
        let permit = client
            .reserve_reply()
            .expect("paid before original payload construction");
        let index = permit.index;
        let owner = OriginOwner {
            _actual: PanicPayload,
            _funding: permit,
        };
        assert!(
            tokio::spawn(async move {
                drop(owner);
            })
            .await
            .is_err(),
            "actual destructor unwound"
        );
        assert!(
            client.reserve_reply().is_err(),
            "unknown original root refuses new copies"
        );
        let snapshot = runtime.accounting().expect("original custody retained");
        assert_eq!(snapshot.used, before);
        assert_eq!(
            snapshot.attempt_support.expect("original held").phase,
            FinalizationSupportPhase::UnknownHeld
        );
        assert!(
            client.payload.replies[index]
                .lock()
                .expect("original record")
                .original
                .is_some(),
            "original token/header stays in core after failed payload destruction"
        );
        assert!(root.shutdown(Instant::now()).await.is_err());
    }

    #[tokio::test]
    async fn ordinary_and_attempt_support_keep_distinct_original_release_authority() {
        let (_dir, runtime, mut attempt, client) = bookkeeping_fixture().await;
        let attempt_identity = attempt
            .lease
            .as_ref()
            .expect("original attempt lease")
            .identity;
        let mut ordinary = runtime
            .reserve_fixture_support()
            .expect("separate ordinary structural support");
        let snapshot = runtime.accounting().expect("both additive charges");
        assert!(snapshot.finalization_support.is_some());
        assert!(snapshot.attempt_support.is_some());
        assert_ne!(ordinary.identity.kind, attempt_identity.kind);
        ordinary
            .try_close()
            .expect("only actual ordinary never-allocation proof");
        let remaining = runtime.accounting().expect("attempt unchanged");
        assert!(remaining.finalization_support.is_none());
        assert!(remaining.attempt_support.is_some());
        drop(client);
        attempt
            .shutdown(Instant::now() + std::time::Duration::from_secs(1))
            .await
            .expect("only original attempt actual close");
        assert!(runtime
            .accounting()
            .expect("both released")
            .attempt_support
            .is_none());
    }
}
