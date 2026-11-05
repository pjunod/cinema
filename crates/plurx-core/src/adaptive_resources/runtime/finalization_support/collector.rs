//! Independently funded ordinary finite-R services. No complete-peak/trial authority.
use super::super::finite_vod::{
    FiniteCloseIdentity, FiniteCloseTicket, FiniteCloseWorkspace, FiniteVodError,
};
use super::super::operations::{OperationError, OwnedPreparation};
use super::*;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU8, AtomicUsize};
mod gates;
pub use gates::{BorrowedBuildGuard, BuildGate, BuildGateDirectory, BuildGuard};
use gates::{Directory, GateCell, Mode, OriginalGate};

pub const ORDINARY_COLLECTOR_SLOTS: usize = 256;
pub const DAEMON_CLOSING_ENVELOPE_BYTES: usize = 16 * 1024;
const N: usize = ORDINARY_COLLECTOR_SLOTS;
const STARTING: u8 = 0;
const READY: u8 = 1;
const FAILED: u8 = 2;
const STOPPED: u8 = 3;
const MAX_SERVICE_FRAME: usize = 64 * 1024;

struct Control {
    seal: SealIdentity,
    phase: AtomicU8,
    admission_closed: AtomicBool,
    allocated: AtomicBool,
    stop: AtomicBool,
    failed: AtomicBool,
    route_ready: AtomicBool,
    changed: Notify,
    marker: SupportFailureMarker,
}
impl Control {
    fn ready(&self) -> bool {
        self.phase.load(Ordering::Acquire) == READY
            && !self.admission_closed.load(Ordering::Acquire)
            && !self.failed.load(Ordering::Acquire)
    }
    fn fail_deferred(&self) {
        self.failed.store(true, Ordering::Release);
        self.phase.store(FAILED, Ordering::Release);
        self.marker.mark();
    }
    fn fail(&self) {
        self.fail_deferred();
        self.changed.notify_waiters();
    }
    async fn wait_ready(&self, deadline: std::time::Instant) -> Result<(), AdmissionRefusal> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.ready() {
                return Ok(());
            }
            if matches!(self.phase.load(Ordering::Acquire), FAILED | STOPPED)
                || self.admission_closed.load(Ordering::Acquire)
            {
                return Err(AdmissionRefusal::Unavailable);
            }
            tokio::time::timeout_at(deadline.into(), changed)
                .await
                .map_err(|_| AdmissionRefusal::Unavailable)?;
        }
    }
}
struct ActorFailure {
    control: Arc<Control>,
    armed: bool,
}
impl Drop for ActorFailure {
    fn drop(&mut self) {
        if self.armed && self.control.allocated.load(Ordering::Acquire) {
            self.control.fail();
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Reserved,
    Bound,
    Completed,
    Terminal,
    Unknown,
}
#[derive(Clone, Copy)]
struct FixedKey {
    bytes: [u8; 255],
    len: u8,
}
impl FixedKey {
    fn new(key: &str) -> Result<Self, AdmissionRefusal> {
        if key.is_empty() || key.len() > 255 {
            return Err(AdmissionRefusal::InvalidBundle);
        }
        let mut result = Self {
            bytes: [0; 255],
            len: key.len() as u8,
        };
        result.bytes[..key.len()].copy_from_slice(key.as_bytes());
        Ok(result)
    }
    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)])
            .expect("validated fixed UTF-8 key")
    }
}
struct Metadata {
    key: Option<FixedKey>,
    phase: Phase,
    generation: u64,
    gate: Arc<OriginalGate>,
    original: Option<OwnedPreparation>,
    identity: Option<FiniteCloseIdentity>,
    grant_attempted: bool,
}
struct Record {
    meta: Mutex<Metadata>,
    ticket: tokio::sync::Mutex<Option<FiniteCloseTicket>>,
    leases: AtomicUsize,
    dropping: AtomicUsize,
    close: AtomicBool,
    registered: AtomicBool,
    changed: Notify,
}
struct Payload {
    seal: SealIdentity,
    runtime: Arc<NodeResourceRuntime>,
    records: Box<[Record]>,
    #[cfg(test)]
    exhaust_on_destroy: AtomicBool,
}
#[cfg(test)]
impl Drop for Payload {
    fn drop(&mut self) {
        if self.exhaust_on_destroy.load(Ordering::Acquire) {
            self.runtime
                .affected_owners
                .with_runtime_mutation(|inventory| inventory.exhaust_epoch_for_support_fixture())
                .expect("actual payload destructor outside census locks");
        }
    }
}
#[derive(Clone)]
pub struct OrdinaryPoolClient {
    payload: SupportPayloadRef<Payload>,
    control: Arc<Control>,
    directory: Arc<Directory>,
}
impl OrdinaryPoolClient {
    fn find_gate(&self, key: &str) -> Result<Option<BuildGate>, AdmissionRefusal> {
        for record in &self.payload.records {
            let meta = record
                .meta
                .lock()
                .map_err(|_| AdmissionRefusal::Unavailable)?;
            if meta.key.is_some_and(|stored| stored.as_str() == key) {
                return Ok(Some(BuildGate {
                    inner: Arc::clone(&meta.gate),
                }));
            }
        }
        Ok(None)
    }
    pub fn reserve(&self, key: &str) -> Result<OrdinarySlot, AdmissionRefusal> {
        let fixed = FixedKey::new(key)?;
        let mut state = self
            .directory
            .state
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        if !self.control.ready()
            || !matches!(&state.mode, Mode::Running(client) if client.payload.seal == self.payload.seal)
        {
            return Err(AdmissionRefusal::Unavailable);
        }
        let mut candidate = None;
        let mut same_key = false;
        for (index, record) in self.payload.records.iter().enumerate() {
            let mut meta = record
                .meta
                .lock()
                .map_err(|_| AdmissionRefusal::Unavailable)?;
            let role_drained = matches!(meta.phase, Phase::Idle | Phase::Terminal)
                && meta.original.is_none()
                && record.leases.load(Ordering::Acquire) == 0
                && record.dropping.load(Ordering::Acquire) == 0;
            if meta.key.is_some_and(|stored| stored.as_str() == key) {
                if !role_drained {
                    return Err(AdmissionRefusal::LedgerFull);
                }
                // Keeping this exact mutex preserves exclusion even while the
                // new builder (or an old legacy guard) holds that same gate.
                candidate = Some(index);
                same_key = true;
                break;
            }
            if candidate.is_none() && role_drained && Arc::get_mut(&mut meta.gate).is_some() {
                candidate = Some(index);
            }
        }
        let index = candidate.ok_or(AdmissionRefusal::LedgerFull)?;
        let record = &self.payload.records[index];
        let mut meta = record
            .meta
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        if !same_key && Arc::get_mut(&mut meta.gate).is_none() {
            return Err(AdmissionRefusal::LedgerFull);
        }
        let generation = meta
            .generation
            .checked_add(1)
            .ok_or(AdmissionRefusal::IdentityExhausted)?;
        let adopted = if same_key {
            None
        } else {
            state.legacy.get(key).and_then(std::sync::Weak::upgrade)
        };
        let displaced = adopted.map(|original| std::mem::replace(&mut meta.gate, original));
        let old_weak = state.legacy.remove(key);
        meta.key = Some(fixed);
        meta.generation = generation;
        meta.phase = Phase::Reserved;
        meta.grant_attempted = false;
        meta.identity = None;
        meta.original = None;
        record.close.store(false, Ordering::Release);
        record.registered.store(false, Ordering::Release);
        record.leases.store(1, Ordering::Release);
        drop(meta);
        drop(state);
        drop(old_weak);
        drop(displaced);
        Ok(OrdinarySlot {
            payload: self.payload.clone(),
            control: Arc::clone(&self.control),
            directory: Arc::clone(&self.directory),
            index,
            generation,
            key: fixed,
        })
    }
    pub fn seal_admissions(&self) {
        self.control.admission_closed.store(true, Ordering::Release);
    }
    pub fn request_all_close(&self) {
        for record in &self.payload.records {
            record.close.store(true, Ordering::Release);
            record.changed.notify_waiters();
        }
    }
}
pub struct OrdinarySlot {
    payload: SupportPayloadRef<Payload>,
    control: Arc<Control>,
    directory: Arc<Directory>,
    index: usize,
    generation: u64,
    key: FixedKey,
}
impl Clone for OrdinarySlot {
    fn clone(&self) -> Self {
        let record = &self.payload.records[self.index];
        if record
            .leases
            .try_update(Ordering::AcqRel, Ordering::Acquire, |v| v.checked_add(1))
            .is_err()
        {
            self.control.fail();
        }
        Self {
            payload: self.payload.clone(),
            control: Arc::clone(&self.control),
            directory: Arc::clone(&self.directory),
            index: self.index,
            generation: self.generation,
            key: self.key,
        }
    }
}
impl Drop for OrdinarySlot {
    fn drop(&mut self) {
        let record = &self.payload.records[self.index];
        if record.leases.fetch_sub(1, Ordering::AcqRel) == 1 {
            record.close.store(true, Ordering::Release);
            record.changed.notify_waiters();
        }
    }
}
impl OrdinarySlot {
    pub fn key(&self) -> &str {
        self.key.as_str()
    }
    pub fn request_close(&self) {
        let record = &self.payload.records[self.index];
        record.close.store(true, Ordering::Release);
        record.changed.notify_waiters();
    }
    pub fn cancel_unbound(&self) -> Result<(), AdmissionRefusal> {
        let mut meta = self.payload.records[self.index]
            .meta
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        if meta.generation != self.generation
            || meta.grant_attempted
            || meta.original.is_some()
            || meta.identity.is_some()
            || meta.phase != Phase::Reserved
        {
            return Err(AdmissionRefusal::InvalidTransition);
        }
        meta.phase = Phase::Terminal;
        drop(meta);
        self.payload.records[self.index].changed.notify_waiters();
        Ok(())
    }
    pub fn registration_ready(&self) -> bool {
        self.payload.records[self.index]
            .registered
            .load(Ordering::Acquire)
    }
    pub async fn wait_registered(
        &self,
        deadline: std::time::Instant,
    ) -> Result<(), AdmissionRefusal> {
        let record = &self.payload.records[self.index];
        loop {
            let changed = record.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.registration_ready() {
                return Ok(());
            }
            if self.control.failed.load(Ordering::Acquire) {
                return Err(AdmissionRefusal::Unavailable);
            }
            tokio::time::timeout_at(deadline.into(), changed)
                .await
                .map_err(|_| AdmissionRefusal::Unavailable)?;
        }
    }
    pub(in crate::adaptive_resources::runtime) fn wake_failure(&self) {
        self.control.changed.notify_waiters();
    }
    /// Observation only; Native close authority remains in the stored ticket.
    pub async fn wait_terminal(
        &self,
        deadline: std::time::Instant,
    ) -> Result<(), AdmissionRefusal> {
        let record = &self.payload.records[self.index];
        loop {
            let changed = record.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let meta = record
                    .meta
                    .lock()
                    .map_err(|_| AdmissionRefusal::Unavailable)?;
                if meta.generation != self.generation {
                    return Err(AdmissionRefusal::ReceiptMismatch);
                }
                if meta.phase == Phase::Terminal {
                    return Ok(());
                }
            }
            if self.control.failed.load(Ordering::Acquire) {
                return Err(AdmissionRefusal::Unavailable);
            }
            tokio::time::timeout_at(deadline.into(), changed)
                .await
                .map_err(|_| AdmissionRefusal::Unavailable)?;
        }
    }
    pub fn begin_envelope_drop(&self) -> EnvelopeDropGuard {
        if self.payload.records[self.index]
            .dropping
            .try_update(Ordering::AcqRel, Ordering::Acquire, |v| v.checked_add(1))
            .is_err()
        {
            self.control.fail();
        }
        EnvelopeDropGuard {
            slot: self.clone(),
            armed: true,
        }
    }
    pub(in crate::adaptive_resources::runtime) fn grant(
        &self,
        runtime: &NodeResourceRuntime,
    ) -> Result<Grant<'_>, AdmissionRefusal> {
        let directory = self
            .directory
            .state
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        let mut meta = self.payload.records[self.index]
            .meta
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        if self.payload.seal.support.ledger != runtime.ledger.id
            || self.payload.seal.support.owner != runtime.owner.owner_id
            || !self.control.ready()
            || !matches!(&directory.mode, Mode::Running(client) if client.payload.seal == self.payload.seal)
            || meta.generation != self.generation
            || meta.phase != Phase::Reserved
            || meta.grant_attempted
            || self.payload.records[self.index]
                .close
                .load(Ordering::Acquire)
        {
            return Err(AdmissionRefusal::Unavailable);
        }
        meta.grant_attempted = true;
        Ok(Grant {
            _directory: directory,
            meta,
            control: &self.control,
            bound: false,
            known_unaccepted: false,
        })
    }
}
pub struct EnvelopeDropGuard {
    slot: OrdinarySlot,
    armed: bool,
}
impl EnvelopeDropGuard {
    pub fn finish(mut self) {
        self.slot.payload.records[self.slot.index]
            .dropping
            .fetch_sub(1, Ordering::AcqRel);
        self.armed = false;
    }
}
impl Drop for EnvelopeDropGuard {
    fn drop(&mut self) {
        if self.armed {
            self.slot.control.fail();
        }
    }
}
pub(in crate::adaptive_resources::runtime) struct Grant<'a> {
    _directory: std::sync::MutexGuard<'a, gates::DirectoryState>,
    meta: std::sync::MutexGuard<'a, Metadata>,
    control: &'a Control,
    bound: bool,
    known_unaccepted: bool,
}
impl Grant<'_> {
    pub(in crate::adaptive_resources::runtime) fn bind(&mut self, owner: &OwnedPreparation) {
        self.meta.identity = Some(owner.finite_identity(0));
        self.meta.original = Some(owner.clone());
        self.meta.phase = Phase::Bound;
        self.bound = true;
    }
    pub(in crate::adaptive_resources::runtime) fn no_grant(&mut self) {
        self.meta.grant_attempted = false;
        self.known_unaccepted = true;
    }
}
impl Drop for Grant<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() || (!self.bound && !self.known_unaccepted) {
            self.meta.phase = Phase::Unknown;
            self.control.fail_deferred();
        }
    }
}
pub struct OrdinaryCloseWork {
    slot: OrdinarySlot,
}
impl OrdinaryCloseWork {
    pub fn key(&self) -> &str {
        self.slot.key()
    }
    pub fn slot(&self) -> OrdinarySlot {
        self.slot.clone()
    }
}
pub struct CoreServicePort {
    payload: SupportPayloadRef<Payload>,
    control: Arc<Control>,
    directory: Arc<Directory>,
    index: usize,
}
fn finite_state() -> FiniteVodError {
    OperationError::State.into()
}
impl CoreServicePort {
    fn check(&self, work: &OrdinaryCloseWork, meta: &Metadata) -> Result<(), AdmissionRefusal> {
        if work.slot.index != self.index
            || work.slot.payload.seal != self.payload.seal
            || meta.generation != work.slot.generation
        {
            return Err(AdmissionRefusal::ReceiptMismatch);
        }
        Ok(())
    }
    pub async fn wait_work(&self) -> Option<OrdinaryCloseWork> {
        let record = &self.payload.records[self.index];
        loop {
            let changed = record.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let control_changed = self.control.changed.notified();
            tokio::pin!(control_changed);
            control_changed.as_mut().enable();
            if self.control.failed.load(Ordering::Acquire) {
                return None;
            }
            {
                let mut meta = match record.meta.lock() {
                    Ok(meta) => meta,
                    Err(_) => {
                        self.control.fail();
                        return None;
                    }
                };
                if record.close.load(Ordering::Acquire)
                    && meta.phase == Phase::Reserved
                    && !meta.grant_attempted
                {
                    // Core's grant latch and the same metadata lock prove R
                    // never existed. Observers may retain this terminal slot.
                    meta.phase = Phase::Terminal;
                    drop(meta);
                    record.changed.notify_waiters();
                    continue;
                }
                if record.close.load(Ordering::Acquire) && meta.phase == Phase::Bound {
                    if record
                        .leases
                        .try_update(Ordering::AcqRel, Ordering::Acquire, |v| v.checked_add(1))
                        .is_err()
                    {
                        self.control.fail();
                        return None;
                    }
                    return Some(OrdinaryCloseWork {
                        slot: OrdinarySlot {
                            payload: self.payload.clone(),
                            control: Arc::clone(&self.control),
                            directory: Arc::clone(&self.directory),
                            index: self.index,
                            generation: meta.generation,
                            key: meta.key.expect("bound slot key"),
                        },
                    });
                }
                if self.control.stop.load(Ordering::Acquire)
                    && matches!(meta.phase, Phase::Idle | Phase::Terminal)
                {
                    return None;
                }
            }
            tokio::select! { _ = changed => (), _ = control_changed => () }
        }
    }
    pub fn mark_abandon_registered(
        &self,
        work: &OrdinaryCloseWork,
    ) -> Result<(), AdmissionRefusal> {
        let record = &self.payload.records[self.index];
        {
            let meta = record
                .meta
                .lock()
                .map_err(|_| AdmissionRefusal::Unavailable)?;
            self.check(work, &meta)?;
        }
        record.registered.store(true, Ordering::Release);
        record.changed.notify_waiters();
        Ok(())
    }
    pub fn gate(&self, work: &OrdinaryCloseWork) -> Result<BuildGate, AdmissionRefusal> {
        let meta = self.payload.records[self.index]
            .meta
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        self.check(work, &meta)?;
        Ok(BuildGate {
            inner: Arc::clone(&meta.gate),
        })
    }
    pub async fn has_ticket(&self, work: &OrdinaryCloseWork) -> Result<bool, AdmissionRefusal> {
        let record = &self.payload.records[self.index];
        {
            let meta = record
                .meta
                .lock()
                .map_err(|_| AdmissionRefusal::Unavailable)?;
            self.check(work, &meta)?;
        }
        Ok(record.ticket.lock().await.is_some())
    }
    pub async fn install_ticket(
        &self,
        work: &OrdinaryCloseWork,
        ticket: FiniteCloseTicket,
    ) -> Result<(), FiniteVodError> {
        let record = &self.payload.records[self.index];
        let mut held = record.ticket.lock().await;
        let original = {
            let mut meta = record.meta.lock().map_err(|_| finite_state())?;
            self.check(work, &meta).map_err(|_| finite_state())?;
            if meta.phase != Phase::Bound
                || meta.identity != Some(ticket.original_identity())
                || held.is_some()
            {
                return Err(finite_state());
            }
            *held = Some(ticket);
            meta.original.take()
        };
        drop(original);
        Ok(())
    }
    pub async fn prepare_original_ticket(
        &self,
        work: &OrdinaryCloseWork,
        deadline: std::time::Instant,
    ) -> Result<(), FiniteVodError> {
        let original = {
            let meta = self.payload.records[self.index]
                .meta
                .lock()
                .map_err(|_| finite_state())?;
            self.check(work, &meta).map_err(|_| finite_state())?;
            meta.original.clone().ok_or_else(finite_state)?
        };
        let ticket = self
            .payload
            .runtime
            .prepare_collector_ticket(&original, deadline)
            .await?;
        let result = self.install_ticket(work, ticket).await;
        drop(original);
        result
    }
    pub async fn wait_application_release(
        &self,
        work: &OrdinaryCloseWork,
        deadline: std::time::Instant,
    ) -> Result<(), FiniteVodError> {
        let record = &self.payload.records[self.index];
        {
            let meta = record.meta.lock().map_err(|_| finite_state())?;
            self.check(work, &meta).map_err(|_| finite_state())?;
        }
        record
            .ticket
            .lock()
            .await
            .as_mut()
            .ok_or_else(finite_state)?
            .wait_application_release(deadline)
            .await
    }
    pub async fn finish_retirement(
        &self,
        work: &OrdinaryCloseWork,
        deadline: std::time::Instant,
    ) -> Result<(), FiniteVodError> {
        let record = &self.payload.records[self.index];
        let mut held = record.ticket.lock().await;
        {
            let meta = record.meta.lock().map_err(|_| finite_state())?;
            self.check(work, &meta).map_err(|_| finite_state())?;
        }
        let ticket = held.as_mut().ok_or_else(finite_state)?;
        let mut workspace = FiniteCloseWorkspace::new();
        ticket
            .finish_retirement_in(&mut workspace, deadline)
            .await?;
        let ticket = held.take().ok_or_else(finite_state)?;
        let completed = match ticket.into_completed() {
            Ok(completed) => completed,
            Err(ticket) => {
                *held = Some(ticket);
                return Err(finite_state());
            }
        };
        let mut meta = record.meta.lock().map_err(|_| finite_state())?;
        if meta.identity != Some(completed.original_identity()) {
            self.control.fail();
            return Err(finite_state());
        }
        meta.phase = Phase::Completed;
        Ok(())
    }
    pub fn complete(&self, work: &OrdinaryCloseWork) -> Result<(), AdmissionRefusal> {
        let record = &self.payload.records[self.index];
        let mut meta = record
            .meta
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        self.check(work, &meta)?;
        if meta.phase != Phase::Completed || meta.original.is_some() {
            return Err(AdmissionRefusal::InvalidTransition);
        }
        meta.phase = Phase::Terminal;
        drop(meta);
        record.changed.notify_waiters();
        Ok(())
    }
}
struct ConcreteStorage<F> {
    original: Arc<SupportPayload<Payload>>,
    directory: Arc<Directory>,
    control: Arc<Control>,
    _service: PhantomData<fn() -> F>,
}
impl<F> StoredPayload for ConcreteStorage<F> {
    fn unique(&mut self) -> bool {
        if self.control.phase.load(Ordering::Acquire) != STOPPED
            || self.control.failed.load(Ordering::Acquire)
        {
            return false;
        }
        let Ok(state) = self.directory.state.try_lock() else {
            return false;
        };
        let Some(payload) = Arc::get_mut(&mut self.original) else {
            return false;
        };
        if !matches!(state.mode, Mode::Closing(seal) if seal == payload.actual.seal) {
            return false;
        }
        for record in &mut payload.actual.records {
            let Ok(meta) = record.meta.get_mut() else {
                return false;
            };
            if !matches!(meta.phase, Phase::Idle | Phase::Terminal)
                || meta.original.is_some()
                || record.leases.load(Ordering::Acquire) != 0
                || record.dropping.load(Ordering::Acquire) != 0
                || record.ticket.get_mut().is_some()
            {
                return false;
            }
        }
        payload.actual.records.iter_mut().all(|record| {
            record
                .meta
                .get_mut()
                .is_ok_and(|meta| Arc::get_mut(&mut meta.gate).is_some())
        })
    }
    fn additional_requested() -> Option<usize> {
        let per_record = std::mem::size_of::<Record>()
            .checked_add(std::mem::size_of::<OriginalGate>())?
            .checked_add(128)?
            .checked_add(std::mem::size_of::<F>())?
            .checked_add(DAEMON_CLOSING_ENVELOPE_BYTES)?;
        per_record
            .checked_mul(N)?
            .checked_add(std::mem::size_of::<F>())?
            .checked_add(std::mem::size_of::<Record>())
    }
    fn destroy(self: Box<Self>) -> Option<SealIdentity> {
        let Self {
            original,
            directory,
            control,
            ..
        } = *self;
        let actual = Arc::try_unwrap(original)
            .unwrap_or_else(|_| unreachable!("sealed concrete payload unique"));
        let seal = actual.actual.seal;
        drop(actual);
        drop(directory);
        drop(control);
        Some(seal)
    }
}
async fn run_collector<F: Future<Output = ()>>(
    mut services: [Pin<Box<F>>; N],
    control: Arc<Control>,
    mut failure: ActorFailure,
    payload: SupportPayloadRef<Payload>,
) {
    let mut done = [false; N];
    let mut cursor = 0;
    std::future::poll_fn(|cx| {
        for offset in 0..N {
            let index = (cursor + offset) % N;
            if !done[index] && services[index].as_mut().poll(cx).is_ready() {
                done[index] = true;
                if !control.stop.load(Ordering::Acquire) {
                    control.fail();
                    return Poll::Ready(());
                }
            }
        }
        cursor = (cursor + 1) % N;
        if done.iter().all(|done| *done) {
            return Poll::Ready(());
        }
        if control.route_ready.load(Ordering::Acquire)
            && !control.failed.load(Ordering::Acquire)
            && control
                .phase
                .compare_exchange(STARTING, READY, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            control.changed.notify_waiters();
        }
        Poll::Pending
    })
    .await;
    let drained = payload.records.iter().all(|record| {
        record.meta.lock().is_ok_and(|meta| {
            matches!(meta.phase, Phase::Idle | Phase::Terminal) && meta.original.is_none()
        }) && record
            .ticket
            .try_lock()
            .is_ok_and(|ticket| ticket.is_none())
    });
    if control.stop.load(Ordering::Acquire) && !control.failed.load(Ordering::Acquire) && drained {
        failure.armed = false;
        control.phase.store(STOPPED, Ordering::Release);
        control.changed.notify_waiters();
    }
}
fn collector_frame<F: Future<Output = ()>>() -> usize {
    fn size<A, B, C, D, R: Future<Output = ()>>(_: fn(A, B, C, D) -> R) -> usize {
        registered_future_size::<R>()
    }
    size(run_collector::<F>)
}
fn requested<F: Future<Output = ()>>() -> Result<usize, AdmissionRefusal> {
    if std::mem::size_of::<F>() > MAX_SERVICE_FRAME {
        return Err(AdmissionRefusal::HardLimit(
            super::super::super::ResourceDimension::Memory,
        ));
    }
    std::mem::size_of::<SupportState>()
        .checked_add(std::mem::size_of::<SupportPayload<Payload>>())
        .and_then(|v| v.checked_add(std::mem::size_of::<ConcreteStorage<F>>()))
        .and_then(|v| v.checked_add(collector_frame::<F>()))
        .and_then(|v| v.checked_add(8 * std::mem::size_of::<usize>()))
        .and_then(|v| v.checked_add(ConcreteStorage::<F>::additional_requested()?))
        .and_then(|v| v.checked_add(collector_frame::<F>()))
        .ok_or(AdmissionRefusal::ArithmeticOverflow)
}
/// Unique external shutdown custody. Never store this in the captured Shared graph.
pub struct OrdinaryFinalizerLifecycle {
    directory: BuildGateDirectory,
    lease: Option<FinalizationSupportLease>,
    client: Option<OrdinaryPoolClient>,
    control: Option<Arc<Control>>,
    starting: bool,
}
impl OrdinaryFinalizerLifecycle {
    pub fn new(directory: BuildGateDirectory) -> Self {
        Self {
            directory,
            lease: None,
            client: None,
            control: None,
            starting: false,
        }
    }
    pub async fn start<R, F>(
        &mut self,
        runtime: &Arc<NodeResourceRuntime>,
        roots: Arc<R>,
        service: fn(CoreServicePort, Arc<R>) -> F,
        deadline: std::time::Instant,
    ) -> Result<OrdinaryPoolClient, AdmissionRefusal>
    where
        R: Send + Sync + 'static,
        F: Future<Output = ()> + Send + 'static,
    {
        if let Some(client) = &self.client {
            client.control.wait_ready(deadline).await?;
            return Ok(client.clone());
        }
        if self.lease.is_some() {
            return Err(AdmissionRefusal::InvalidTransition);
        }
        {
            let state = self
                .directory
                .inner
                .state
                .lock()
                .map_err(|_| AdmissionRefusal::Unavailable)?;
            if !matches!(state.mode, Mode::Legacy) {
                return Err(AdmissionRefusal::InvalidTransition);
            }
        }
        self.starting = true;
        let profile = FinalizationSupportProfile::ordinary(requested::<F>()?)?;
        let lease = match runtime.reserve_observed_support(profile, deadline).await {
            Ok(lease) => lease,
            Err(error) => {
                self.starting = false;
                return Err(error);
            }
        };
        self.lease = Some(lease);
        let executor = match tokio::runtime::Handle::try_current() {
            Ok(executor) => executor,
            Err(_) => {
                self.lease
                    .as_mut()
                    .expect("reserved root lease")
                    .try_close()?;
                self.lease = None;
                self.starting = false;
                return Err(AdmissionRefusal::Unavailable);
            }
        };
        let client = match self.activate(runtime, roots, service, executor) {
            Ok(client) => client,
            Err(error) => {
                if self.lease.as_ref().is_some_and(|lease| !lease.armed) {
                    self.lease = None;
                    self.starting = false;
                }
                return Err(error);
            }
        };
        client.control.wait_ready(deadline).await?;
        Ok(client)
    }
    fn activate<R, F>(
        &mut self,
        runtime: &Arc<NodeResourceRuntime>,
        roots: Arc<R>,
        service: fn(CoreServicePort, Arc<R>) -> F,
        executor: tokio::runtime::Handle,
    ) -> Result<OrdinaryPoolClient, AdmissionRefusal>
    where
        R: Send + Sync + 'static,
        F: Future<Output = ()> + Send + 'static,
    {
        let lease = self
            .lease
            .as_mut()
            .ok_or(AdmissionRefusal::InvalidTransition)?;
        let seal = match self.directory.inner.identity(lease.identity) {
            Ok(seal) => seal,
            Err(error) => {
                lease.try_close()?;
                return Err(error);
            }
        };
        let control = Arc::new(Control {
            seal,
            phase: AtomicU8::new(STARTING),
            admission_closed: AtomicBool::new(false),
            allocated: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            route_ready: AtomicBool::new(false),
            changed: Notify::new(),
            marker: lease.marker.clone(),
        });
        self.control = Some(Arc::clone(&control));
        let failure = ActorFailure {
            control: Arc::clone(&control),
            armed: true,
        };
        let directory = Arc::clone(&self.directory.inner);
        let storage_directory = Arc::clone(&directory);
        let storage_control = Arc::clone(&control);
        let factory_control = Arc::clone(&control);
        let actor_control = Arc::clone(&control);
        let actor_directory = Arc::clone(&directory);
        let factory_runtime = Arc::clone(runtime);
        let payload = lease.activate_registered(
            executor,
            move || {
                factory_control.allocated.store(true, Ordering::Release);
                let mut records = Vec::with_capacity(N);
                for _ in 0..N {
                    records.push(Record {
                        meta: Mutex::new(Metadata {
                            key: None,
                            phase: Phase::Idle,
                            generation: 0,
                            gate: Arc::new(tokio::sync::Mutex::new(GateCell {
                                drops: Arc::clone(&directory.drops),
                            })),
                            original: None,
                            identity: None,
                            grant_attempted: false,
                        }),
                        ticket: tokio::sync::Mutex::new(None),
                        leases: AtomicUsize::new(0),
                        dropping: AtomicUsize::new(0),
                        close: AtomicBool::new(false),
                        registered: AtomicBool::new(false),
                        changed: Notify::new(),
                    });
                }
                Payload {
                    seal,
                    runtime: factory_runtime,
                    records: records.into_boxed_slice(),
                    #[cfg(test)]
                    exhaust_on_destroy: AtomicBool::new(false),
                }
            },
            move |payload| {
                let services = std::array::from_fn(|index| {
                    Box::pin(service(
                        CoreServicePort {
                            payload: payload.clone(),
                            control: Arc::clone(&actor_control),
                            directory: Arc::clone(&actor_directory),
                            index,
                        },
                        Arc::clone(&roots),
                    ))
                });
                run_collector(services, actor_control, failure, payload)
            },
            move |original| ConcreteStorage::<F> {
                original,
                directory: storage_directory,
                control: storage_control,
                _service: PhantomData,
            },
        )?;
        let client = OrdinaryPoolClient {
            payload,
            control: Arc::clone(&control),
            directory: Arc::clone(&self.directory.inner),
        };
        self.client = Some(client.clone());
        self.directory.inner.publish(client.clone())?;
        control.route_ready.store(true, Ordering::Release);
        control.changed.notify_waiters();
        for record in &client.payload.records {
            record.changed.notify_waiters();
        }
        Ok(client)
    }
    pub fn begin_drain(&self) {
        if let Some(control) = &self.control {
            control.admission_closed.store(true, Ordering::Release);
        }
    }
    pub async fn shutdown(&mut self, deadline: std::time::Instant) -> Result<(), AdmissionRefusal> {
        let control = self
            .control
            .as_ref()
            .ok_or(AdmissionRefusal::InvalidTransition)?;
        self.directory.inner.seal(control.seal)?;
        control.stop.store(true, Ordering::Release);
        control.changed.notify_waiters();
        if let Some(client) = &self.client {
            client.request_all_close();
        }
        tokio::time::timeout_at(
            deadline.into(),
            self.lease
                .as_mut()
                .ok_or(AdmissionRefusal::InvalidTransition)?
                .join_original(),
        )
        .await
        .map_err(|_| AdmissionRefusal::Unavailable)??;
        drop(self.client.take());
        let released = self
            .lease
            .as_mut()
            .ok_or(AdmissionRefusal::InvalidTransition)?
            .try_close_released()?;
        self.directory.inner.reopen(released)
    }
}
impl Drop for OrdinaryFinalizerLifecycle {
    fn drop(&mut self) {
        if self.lease.as_ref().is_some_and(|lease| lease.armed) {
            if let Some(control) = &self.control {
                control.fail();
            }
        }
    }
}
#[cfg(all(test, unix))]
mod tests;
