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
}

pub(in crate::adaptive_resources::runtime) struct ResponseBudget {
    limit: usize,
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

    pub(in crate::adaptive_resources::runtime) fn new(
        limit: usize,
    ) -> Result<Arc<Self>, OperationError> {
        if limit <= BASE_OVERHEAD_BYTES + SLOT_OVERHEAD_BYTES {
            return Err(OperationError::State);
        }
        Ok(Arc::new(Self {
            limit,
            state: Mutex::new(BudgetState {
                used: BASE_OVERHEAD_BYTES,
                live: 0,
                closing: false,
                failed: false,
            }),
        }))
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

    /// Atomic with reserve: a successful close fence excludes every future
    /// allocation, while existing buffers make retirement wait/refuse.
    pub(in crate::adaptive_resources::runtime) fn begin_close_if_empty(
        &self,
    ) -> Result<(), OperationError> {
        let mut state = self.state.lock().map_err(|_| OperationError::State)?;
        if state.failed || state.live != 0 || state.used != BASE_OVERHEAD_BYTES {
            return Err(OperationError::State);
        }
        state.closing = true;
        Ok(())
    }

    pub(in crate::adaptive_resources::runtime) fn closed_and_empty(&self) -> bool {
        self.state.lock().is_ok_and(|state| {
            state.closing && !state.failed && state.live == 0 && state.used == BASE_OVERHEAD_BYTES
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
}
