//! A fixed subpartition of the original finite role's admitted memory.
//!
//! Releasing a response slot never refunds the durable ledger reservation.
//! It only makes this same in-role partition reusable while the role is live.
use super::operations::OperationError;
use std::io;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};

// This partition accounts for requested payload capacity and the inline Rust
// owner structures. Arc-private headers, allocator rounding and HTTP/parser
// framework allocations need separate bounds in the wider memory profile;
// these sizes are not a certified whole-allocation or process RSS ceiling.
const BASE_OVERHEAD_BYTES: usize = std::mem::size_of::<ResponseBudget>();
const SLOT_OVERHEAD_BYTES: usize = std::mem::size_of::<FiniteResponseBuffer>();

struct BudgetState {
    used: usize,
    live: usize,
    closing: bool,
    failed: bool,
    metadata_live: bool,
    client_live: bool,
    client_limit: usize,
    client_used: usize,
    client_slots: usize,
}

enum ClientReservationSource {
    Lease(usize),
    ExistingSlot(usize),
}

pub(in crate::adaptive_resources::runtime) struct ResponseBudget {
    limit: usize,
    metadata_limit: usize,
    drained: tokio::sync::Notify,
    state: Mutex<BudgetState>,
}

impl ResponseBudget {
    /// The complete original in-role partition remains available to S for
    /// future counted bodies, including when no response is live at transfer.
    pub(in crate::adaptive_resources::runtime) fn limit_bytes(
        &self,
    ) -> Result<u64, OperationError> {
        u64::try_from(self.limit).map_err(|_| OperationError::State)
    }

    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current direct/copy no-metadata response builders are Linux-only; retain the same budget checks for portable fixtures."
        )
    )]
    pub(in crate::adaptive_resources::runtime) fn new(
        limit: usize,
    ) -> Result<Arc<Self>, OperationError> {
        Self::with_metadata(limit, 0)
    }

    pub(in crate::adaptive_resources::runtime) fn with_metadata(
        limit: usize,
        metadata_limit: usize,
    ) -> Result<Arc<Self>, OperationError> {
        if limit <= BASE_OVERHEAD_BYTES + SLOT_OVERHEAD_BYTES {
            return Err(OperationError::State);
        }
        Ok(Arc::new(Self {
            limit,
            metadata_limit,
            drained: tokio::sync::Notify::new(),
            state: Mutex::new(BudgetState {
                used: BASE_OVERHEAD_BYTES,
                live: 0,
                closing: false,
                failed: false,
                metadata_live: false,
                client_live: false,
                client_limit: 0,
                client_used: 0,
                client_slots: 0,
            }),
        }))
    }

    pub(in crate::adaptive_resources::runtime) fn metadata_limit_bytes(
        &self,
    ) -> Result<u64, OperationError> {
        u64::try_from(self.metadata_limit).map_err(|_| OperationError::State)
    }

    pub(in crate::adaptive_resources::runtime) async fn wait_drained(
        &self,
        deadline: std::time::Instant,
    ) -> Result<(), OperationError> {
        loop {
            let notified = self.drained.notified();
            tokio::pin!(notified);
            // Register before inspecting state so the last real owner cannot
            // disappear between the inspection and notification registration.
            notified.as_mut().enable();
            {
                let state = self.state.lock().map_err(|_| OperationError::State)?;
                if state.failed {
                    return Err(OperationError::State);
                }
                if state.live == 0
                    && !state.metadata_live
                    && !state.client_live
                    && state.client_slots == 0
                    && state.client_used == 0
                    && state.used == BASE_OVERHEAD_BYTES
                {
                    return Ok(());
                }
            }
            tokio::time::timeout_at(deadline.into(), notified)
                .await
                .map_err(|_| OperationError::Deadline)?;
        }
    }

    pub(in crate::adaptive_resources::runtime) fn reserve_metadata_partitions(
        self: &Arc<Self>,
        immutable_required: usize,
        client_limit: usize,
    ) -> Result<(FiniteMetadataLease, FiniteClientStateLease), OperationError> {
        let total = immutable_required
            .checked_add(client_limit)
            .ok_or(OperationError::State)?;
        let mut state = self.state.lock().map_err(|_| OperationError::State)?;
        if immutable_required == 0
            || total > self.metadata_limit
            || state.metadata_live
            || state.client_live
            || state.client_slots != 0
            || state.client_used != 0
            || state.closing
            || state.failed
        {
            return Err(OperationError::State);
        }
        state.metadata_live = true;
        state.client_live = true;
        state.client_limit = client_limit;
        Ok((
            FiniteMetadataLease {
                budget: Arc::clone(self),
                limit: immutable_required,
            },
            FiniteClientStateLease {
                budget: Arc::clone(self),
                limit: client_limit,
            },
        ))
    }

    #[cfg(test)]
    fn reserve_metadata(
        self: &Arc<Self>,
        required: usize,
    ) -> Result<FiniteMetadataLease, OperationError> {
        let (metadata, client) = self.reserve_metadata_partitions(required, 0)?;
        drop(client);
        Ok(metadata)
    }

    pub(in crate::adaptive_resources::runtime) fn reserve(
        self: &Arc<Self>,
        payload_capacity: usize,
    ) -> Result<ResponseSlot, OperationError> {
        if payload_capacity == 0 {
            return Err(OperationError::State);
        }
        let charge = payload_capacity
            .checked_add(SLOT_OVERHEAD_BYTES)
            .ok_or(OperationError::State)?;
        let mut state = self.state.lock().map_err(|_| OperationError::State)?;
        if state.closing || state.failed {
            return Err(OperationError::State);
        }
        let used = state
            .used
            .checked_add(charge)
            .ok_or(OperationError::State)?;
        let live = state.live.checked_add(1).ok_or(OperationError::State)?;
        if used > self.limit {
            return Err(OperationError::State);
        }
        state.used = used;
        state.live = live;
        Ok(ResponseSlot {
            budget: Arc::clone(self),
            charge,
        })
    }

    /// Stop new allocations without claiming that any existing owner is gone.
    /// Copy's authenticated native-close stage uses this fence; only the
    /// unchanged empty check below may authorize final typed settlement.
    #[cfg(target_os = "linux")]
    pub(in crate::adaptive_resources::runtime) fn stop_new_allocations(
        &self,
    ) -> Result<(), OperationError> {
        let mut state = self.state.lock().map_err(|_| OperationError::State)?;
        if state.failed {
            return Err(OperationError::State);
        }
        state.closing = true;
        Ok(())
    }

    /// Atomic with reserve: a successful close fence excludes every future
    /// allocation, while existing buffers make retirement wait/refuse.
    pub(in crate::adaptive_resources::runtime) fn begin_close_if_empty(
        &self,
    ) -> Result<(), OperationError> {
        let mut state = self.state.lock().map_err(|_| OperationError::State)?;
        if state.failed
            || state.live != 0
            || state.metadata_live
            || state.client_live
            || state.client_slots != 0
            || state.client_used != 0
            || state.used != BASE_OVERHEAD_BYTES
        {
            return Err(OperationError::State);
        }
        state.closing = true;
        Ok(())
    }

    pub(in crate::adaptive_resources::runtime) fn closed_and_empty(&self) -> bool {
        self.state.lock().is_ok_and(|state| {
            state.closing
                && !state.failed
                && state.live == 0
                && !state.metadata_live
                && !state.client_live
                && state.client_slots == 0
                && state.client_used == 0
                && state.used == BASE_OVERHEAD_BYTES
        })
    }

    fn release(&self, charge: usize) {
        // A poisoned or inconsistent counter keeps the full role charge held;
        // it can never reopen admission or manufacture a close fact.
        if let Ok(mut state) = self.state.lock() {
            match (state.used.checked_sub(charge), state.live.checked_sub(1)) {
                (Some(used), Some(live)) if used >= BASE_OVERHEAD_BYTES => {
                    state.used = used;
                    state.live = live;
                }
                _ => {
                    state.failed = true;
                    state.closing = true;
                }
            }
        }
        self.drained.notify_waiters();
    }
}

/// One daemon rendition's admitted metadata lifetime. Place this after all
/// owned payload fields so Rust destroys them before releasing this in-role
/// slot. Its destructor never settles a journal row or refunds the ledger.
pub struct FiniteMetadataLease {
    budget: Arc<ResponseBudget>,
    limit: usize,
}

impl FiniteMetadataLease {
    pub fn validate_actual(&self, requested_bytes: usize) -> io::Result<()> {
        if requested_bytes == 0 || requested_bytes > self.limit {
            return Err(io::Error::other(
                "finite rendition metadata exceeds its admitted partition",
            ));
        }
        Ok(())
    }
}

impl Drop for FiniteMetadataLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.budget.state.lock() {
            if !state.metadata_live {
                state.failed = true;
                state.closing = true;
            } else {
                state.metadata_live = false;
            }
        }
        self.budget.drained.notify_waiters();
    }
}

/// Allocation authority for mutable finite client state. This lease is not
/// cloneable; individual slots may be shared through the real owning objects.
/// Neither top-lease nor slot Drop returns durable ledger credit.
pub struct FiniteClientStateLease {
    budget: Arc<ResponseBudget>,
    limit: usize,
}

impl FiniteClientStateLease {
    pub fn limit_bytes(&self) -> usize {
        self.limit
    }

    pub fn reserve(&self, payload_bytes: usize) -> io::Result<Arc<FiniteClientSlot>> {
        self.budget
            .reserve_client_slot(payload_bytes, ClientReservationSource::Lease(self.limit))
    }
}

impl ResponseBudget {
    fn reserve_client_slot(
        self: &Arc<Self>,
        payload_bytes: usize,
        source: ClientReservationSource,
    ) -> io::Result<Arc<FiniteClientSlot>> {
        let charge = FiniteClientSlot::required_bytes(payload_bytes)
            .ok_or_else(|| io::Error::other("finite client-state size overflow"))?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("finite client-state budget unavailable"))?;
        let source_live = match source {
            ClientReservationSource::Lease(limit) => {
                state.metadata_live && state.client_live && state.client_limit == limit
            }
            // The borrowed original slot cannot drop while this method runs.
            // Its charge is still part of this SAME bounded partition, even
            // if the rendition and both top leases have already disappeared.
            ClientReservationSource::ExistingSlot(charge) => {
                state.client_slots > 0 && state.client_used >= charge && state.client_limit > 0
            }
        };
        if !source_live || state.failed || state.closing {
            return Err(io::Error::other("finite client-state owner is closed"));
        }
        let used = state
            .client_used
            .checked_add(charge)
            .ok_or_else(|| io::Error::other("finite client-state size overflow"))?;
        let slots = state
            .client_slots
            .checked_add(1)
            .ok_or_else(|| io::Error::other("finite client-state count overflow"))?;
        if used > state.client_limit {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "finite client-state memory capacity is full",
            ));
        }
        state.client_used = used;
        state.client_slots = slots;
        drop(state);
        Ok(Arc::new(FiniteClientSlot {
            budget: Arc::clone(self),
            payload_bytes,
            charge,
        }))
    }
}

impl Drop for FiniteClientStateLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.budget.state.lock() {
            if !state.client_live || state.client_limit != self.limit {
                state.failed = true;
                state.closing = true;
            } else {
                state.client_live = false;
            }
        }
        self.budget.drained.notify_waiters();
    }
}

/// Keep the same Arc slot after all payload fields in every retaining owner.
/// Its last Drop only reuses bytes inside the original admitted partition.
pub struct FiniteClientSlot {
    budget: Arc<ResponseBudget>,
    payload_bytes: usize,
    charge: usize,
}

impl FiniteClientSlot {
    pub fn required_bytes(payload_bytes: usize) -> Option<usize> {
        payload_bytes
            .checked_add(std::mem::size_of::<Self>())?
            .checked_add(2 * std::mem::size_of::<usize>())
    }
    /// Reserve a DISTINCT allocation before cloning an escaping control result
    /// or serializing its response. The original slot keeps the same role's
    /// partition alive after its rendition drops; this grants no native entry
    /// or durable credit, and total outstanding bytes remain bounded.
    pub fn reserve_related(&self, payload_bytes: usize) -> io::Result<Arc<FiniteClientSlot>> {
        self.budget.reserve_client_slot(
            payload_bytes,
            ClientReservationSource::ExistingSlot(self.charge),
        )
    }

    pub fn validate_actual(&self, actual_payload_bytes: usize) -> io::Result<()> {
        if actual_payload_bytes > self.payload_bytes {
            return Err(io::Error::other(
                "finite client-state allocation exceeds its reserved slot",
            ));
        }
        Ok(())
    }
}

impl Drop for FiniteClientSlot {
    fn drop(&mut self) {
        if let Ok(mut state) = self.budget.state.lock() {
            match (
                state.client_used.checked_sub(self.charge),
                state.client_slots.checked_sub(1),
            ) {
                (Some(used), Some(slots)) => {
                    state.client_used = used;
                    state.client_slots = slots;
                }
                _ => {
                    state.failed = true;
                    state.closing = true;
                }
            }
        }
        self.budget.drained.notify_waiters();
    }
}

pub(in crate::adaptive_resources::runtime) struct ResponseSlot {
    budget: Arc<ResponseBudget>,
    charge: usize,
}
impl Drop for ResponseSlot {
    fn drop(&mut self) {
        self.budget.release(self.charge);
    }
}

/// A reserved external parser workspace in the SAME response partition. It
/// owns no dummy payload allocation and cannot return ledger credit. Callers
/// must bound their allocations and keep this lease until those values drop.
pub struct FiniteResponseWorkspace {
    _slot: ResponseSlot,
}

/// No raw Vec escape. The complete allocation remains charged until this
/// owner (possibly inside `Bytes::from_owner`) is actually dropped.
pub struct FiniteResponseBuffer {
    bytes: Option<Vec<u8>>,
    slot: Option<ResponseSlot>,
    capacity: usize,
}
impl FiniteResponseBuffer {
    /// Fill this already charged allocation directly from the original source
    /// descriptor. Positional IO never changes its shared file offset and does
    /// not create a second heap payload or another native handle.
    #[cfg(target_os = "linux")]
    pub(in crate::adaptive_resources::runtime) fn read_at(
        &mut self,
        file: &std::fs::File,
        offset: u64,
        requested: usize,
    ) -> io::Result<usize> {
        use std::os::unix::fs::FileExt;

        if requested == 0 || requested > self.capacity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid charged read",
            ));
        }
        let bytes = self.bytes.as_mut().expect("live response buffer");
        bytes.resize(requested, 0);
        let result = file.read_at(bytes, offset);
        match result {
            Ok(read) => {
                bytes.truncate(read);
                Ok(read)
            }
            Err(error) => {
                bytes.clear();
                Err(error)
            }
        }
    }

    pub fn reserve_metadata_workspace(&self, bytes: usize) -> io::Result<FiniteResponseWorkspace> {
        let owner = self
            .slot
            .as_ref()
            .ok_or_else(|| io::Error::other("finite payload owner unavailable"))?;
        let slot = owner
            .budget
            .reserve(bytes)
            .map_err(|error| io::Error::other(error.to_string()))?;
        Ok(FiniteResponseWorkspace { _slot: slot })
    }
    /// Payload plus this core owner's fixed metadata for a chosen number of
    /// simultaneously outstanding bodies. HTTP/allocator framework overhead
    /// must be budgeted separately by the caller's broader profile.
    pub fn required_partition_bytes(
        payload_capacity: usize,
        simultaneous_buffers: usize,
    ) -> Option<u64> {
        if payload_capacity == 0 || simultaneous_buffers == 0 {
            return None;
        }
        let one = payload_capacity.checked_add(SLOT_OVERHEAD_BYTES)?;
        let total = one
            .checked_mul(simultaneous_buffers)?
            .checked_add(BASE_OVERHEAD_BYTES)?;
        u64::try_from(total).ok()
    }

    pub(in crate::adaptive_resources::runtime) fn allocate(
        slot: ResponseSlot,
        capacity: usize,
    ) -> Result<Self, OperationError> {
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(capacity).map_err(|_| {
            OperationError::Physical(io::Error::other("finite response allocation failed"))
        })?;
        if bytes.capacity() > capacity {
            return Err(OperationError::State);
        }
        Ok(Self {
            bytes: Some(bytes),
            slot: Some(slot),
            capacity,
        })
    }

    pub fn try_extend_from_slice(&mut self, data: &[u8]) -> io::Result<()> {
        let bytes = self.bytes.as_mut().expect("live response buffer");
        if bytes
            .len()
            .checked_add(data.len())
            .is_none_or(|next| next > self.capacity)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "finite response buffer overflow",
            ));
        }
        bytes.extend_from_slice(data);
        Ok(())
    }
}
impl Deref for FiniteResponseBuffer {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.bytes.as_deref().expect("live response buffer")
    }
}
impl DerefMut for FiniteResponseBuffer {
    fn deref_mut(&mut self) -> &mut [u8] {
        self.bytes.as_deref_mut().expect("live response buffer")
    }
}
impl AsRef<[u8]> for FiniteResponseBuffer {
    fn as_ref(&self) -> &[u8] {
        self
    }
}
impl AsMut<[u8]> for FiniteResponseBuffer {
    fn as_mut(&mut self) -> &mut [u8] {
        self
    }
}
impl Drop for FiniteResponseBuffer {
    fn drop(&mut self) {
        // The physical payload is freed before the same-role slot is reusable.
        drop(self.bytes.take());
        drop(self.slot.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_partition_fits_and_excess_refuses() {
        let budget = ResponseBudget::new(BASE_OVERHEAD_BYTES + SLOT_OVERHEAD_BYTES + 32).unwrap();
        let slot = budget.reserve(32).unwrap();
        assert!(budget.reserve(1).is_err());
        drop(slot);
        assert!(budget.reserve(33).is_err());
    }

    #[test]
    fn owned_payload_keeps_slot_until_drop() {
        let budget = ResponseBudget::new(BASE_OVERHEAD_BYTES + SLOT_OVERHEAD_BYTES + 64).unwrap();
        let mut body = FiniteResponseBuffer::allocate(budget.reserve(64).unwrap(), 64).unwrap();
        body.try_extend_from_slice(&[7; 64]).unwrap();
        assert_eq!(body.as_ref().len(), 64);
        assert!(budget.begin_close_if_empty().is_err());
        drop(body);
        budget.begin_close_if_empty().unwrap();
    }

    #[test]
    fn close_fence_excludes_future_reservations() {
        let budget = ResponseBudget::new(BASE_OVERHEAD_BYTES + SLOT_OVERHEAD_BYTES + 8).unwrap();
        let slot = budget.reserve(8).unwrap();
        assert!(budget.begin_close_if_empty().is_err());
        drop(slot);
        budget.begin_close_if_empty().unwrap();
        assert!(budget.closed_and_empty());
        assert!(budget.reserve(1).is_err());
    }
    #[test]
    fn parser_workspace_keeps_partition_held_after_payload_drop() {
        let budget =
            ResponseBudget::new(BASE_OVERHEAD_BYTES + 2 * SLOT_OVERHEAD_BYTES + 128).unwrap();
        let body = FiniteResponseBuffer::allocate(budget.reserve(64).unwrap(), 64).unwrap();
        let workspace = body.reserve_metadata_workspace(64).unwrap();
        assert!(body.reserve_metadata_workspace(1).is_err());
        drop(body);
        assert!(budget.begin_close_if_empty().is_err());
        drop(workspace);
        budget.begin_close_if_empty().unwrap();
        assert!(budget.closed_and_empty());
    }
    #[test]
    fn metadata_lifetime_blocks_settlement_without_spending_response_capacity() {
        let response_limit = BASE_OVERHEAD_BYTES + SLOT_OVERHEAD_BYTES + 64;
        let budget = ResponseBudget::with_metadata(response_limit, 256).unwrap();
        assert!(budget.reserve_metadata(257).is_err());
        let metadata = budget.reserve_metadata(192).unwrap();
        assert!(metadata.validate_actual(192).is_ok());
        assert!(metadata.validate_actual(193).is_err());
        assert!(budget.reserve_metadata(1).is_err());
        let body = FiniteResponseBuffer::allocate(budget.reserve(64).unwrap(), 64).unwrap();
        assert!(budget.reserve(1).is_err());
        drop(body);
        assert!(budget.begin_close_if_empty().is_err());
        drop(metadata);
        budget.begin_close_if_empty().unwrap();
        assert!(budget.closed_and_empty());
        assert!(budget.reserve_metadata(1).is_err());
    }

    #[tokio::test]
    async fn allocation_drain_waits_for_both_real_metadata_and_body_owners() {
        use std::future::Future;
        use std::task::{Context, Poll, Waker};
        let budget =
            ResponseBudget::with_metadata(BASE_OVERHEAD_BYTES + SLOT_OVERHEAD_BYTES + 64, 256)
                .unwrap();
        let metadata = budget.reserve_metadata(128).unwrap();
        let body = FiniteResponseBuffer::allocate(budget.reserve(64).unwrap(), 64).unwrap();
        let wait =
            budget.wait_drained(std::time::Instant::now() + std::time::Duration::from_secs(10));
        tokio::pin!(wait);
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(wait.as_mut().poll(&mut context), Poll::Pending));
        drop(metadata);
        assert!(matches!(wait.as_mut().poll(&mut context), Poll::Pending));
        drop(body);
        assert!(matches!(
            wait.as_mut().poll(&mut context),
            Poll::Ready(Ok(()))
        ));
        // A drain notification itself cannot grant physical settlement or
        // close the budget against a subsequent in-flight response race.
        assert!(!budget.closed_and_empty());
        budget.begin_close_if_empty().unwrap();
        assert!(budget.closed_and_empty());
    }
    #[test]
    fn client_partition_refusal_is_atomic_and_does_not_spend_response_credit() {
        let response_limit = BASE_OVERHEAD_BYTES + SLOT_OVERHEAD_BYTES + 64;
        let client_limit = FiniteClientSlot::required_bytes(80).unwrap();
        let budget = ResponseBudget::with_metadata(response_limit, 128 + client_limit).unwrap();
        assert!(budget
            .reserve_metadata_partitions(129, client_limit)
            .is_err());
        let (metadata, clients) = budget
            .reserve_metadata_partitions(128, client_limit)
            .unwrap();
        assert!(budget.reserve_metadata_partitions(1, 1).is_err());
        let client = clients.reserve(80).unwrap();
        assert!(client.validate_actual(80).is_ok());
        assert!(client.validate_actual(81).is_err());
        assert_eq!(
            clients.reserve(1).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        let body = FiniteResponseBuffer::allocate(budget.reserve(64).unwrap(), 64).unwrap();
        drop(client);
        let replacement = clients.reserve(80).unwrap();
        drop((body, replacement, clients, metadata));
        budget.begin_close_if_empty().unwrap();
        assert!(budget.closed_and_empty());
    }

    #[tokio::test]
    async fn escaped_client_alias_blocks_drain_after_rendition_owners_drop() {
        use std::future::Future;
        use std::task::{Context, Poll, Waker};
        let client_limit = FiniteClientSlot::required_bytes(64).unwrap();
        let budget = ResponseBudget::with_metadata(
            BASE_OVERHEAD_BYTES + SLOT_OVERHEAD_BYTES + 64,
            128 + client_limit,
        )
        .unwrap();
        let (metadata, clients) = budget
            .reserve_metadata_partitions(128, client_limit)
            .unwrap();
        let client = clients.reserve(64).unwrap();
        let session_alias = Arc::clone(&client);
        drop((client, metadata));
        assert!(clients.reserve(1).is_err());
        drop(clients);
        assert!(budget.begin_close_if_empty().is_err());
        assert!(budget
            .reserve_metadata_partitions(128, client_limit)
            .is_err());
        let wait =
            budget.wait_drained(std::time::Instant::now() + std::time::Duration::from_secs(10));
        tokio::pin!(wait);
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(wait.as_mut().poll(&mut context), Poll::Pending));
        drop(session_alias);
        assert!(matches!(
            wait.as_mut().poll(&mut context),
            Poll::Ready(Ok(()))
        ));
        assert!(!budget.closed_and_empty());
        budget.begin_close_if_empty().unwrap();
        assert!(budget.closed_and_empty());
    }
    #[test]
    fn related_control_copies_remain_bounded_after_top_leases_drop() {
        let one = FiniteClientSlot::required_bytes(80).unwrap();
        let budget = ResponseBudget::with_metadata(
            BASE_OVERHEAD_BYTES + SLOT_OVERHEAD_BYTES + 64,
            128 + 2 * one,
        )
        .unwrap();
        let (metadata, clients) = budget.reserve_metadata_partitions(128, 2 * one).unwrap();
        let original = clients.reserve(80).unwrap();
        drop((metadata, clients));
        let copy = original.reserve_related(80).unwrap();
        assert_eq!(
            original.reserve_related(1).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(budget.begin_close_if_empty().is_err());
        drop(original);
        let body = copy.reserve_related(80).unwrap();
        assert!(budget.begin_close_if_empty().is_err());
        drop((copy, body));
        budget.begin_close_if_empty().unwrap();
        assert!(budget.closed_and_empty());
    }
}
