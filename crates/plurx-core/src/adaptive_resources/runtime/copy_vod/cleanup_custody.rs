//! Exclusive scheduling custody for the original copy close attempt.
//! This compact control owns no preparation, native witness or application
//! lease. Its state selects repair; it never authorizes settlement or refund.
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

const OWNED: u8 = 0;
const REPAIR_REQUESTED: u8 = 1;
const CLAIMED: u8 = 2;
const FINISHED: u8 = 3;

pub(in crate::adaptive_resources::runtime) struct CopyCleanupCustody {
    state: AtomicU8,
}

impl CopyCleanupCustody {
    pub(in crate::adaptive_resources::runtime) fn new() -> Self {
        Self {
            state: AtomicU8::new(OWNED),
        }
    }

    pub(in crate::adaptive_resources::runtime) fn requested_allocation_bytes() -> Option<usize> {
        Some(
            std::alloc::Layout::new::<[usize; 2]>()
                .extend(std::alloc::Layout::new::<Self>())
                .ok()?
                .0
                .pad_to_align()
                .size(),
        )
    }

    pub(in crate::adaptive_resources::runtime) fn request_repair(&self) {
        // Owner destruction cannot steal a live close claim or resurrect a
        // completed original operation.
        let _ = self.state.compare_exchange(
            OWNED,
            REPAIR_REQUESTED,
            Ordering::Release,
            Ordering::Relaxed,
        );
    }

    pub(in crate::adaptive_resources::runtime) fn repair_requested(&self) -> bool {
        self.state.load(Ordering::Acquire) == REPAIR_REQUESTED
    }

    pub(in crate::adaptive_resources::runtime) fn closing(&self) -> bool {
        self.state.load(Ordering::Acquire) != OWNED
    }

    pub(in crate::adaptive_resources::runtime) fn try_claim_owner(
        self: &Arc<Self>,
    ) -> Option<CopyCloseClaim> {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if !matches!(state, OWNED | REPAIR_REQUESTED) {
                return None;
            }
            match self.state.compare_exchange_weak(
                state,
                CLAIMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(CopyCloseClaim {
                        custody: Arc::clone(self),
                    })
                }
                Err(current) => state = current,
            }
        }
    }

    pub(in crate::adaptive_resources::runtime) fn try_claim_repair(
        self: &Arc<Self>,
    ) -> Option<CopyCloseClaim> {
        self.state
            .compare_exchange(
                REPAIR_REQUESTED,
                CLAIMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok()
            .map(|_| CopyCloseClaim {
                custody: Arc::clone(self),
            })
    }
}

// Intentionally not Clone. Only this claim can release CLAIMED; there is no
// timeout, revocation or replacement that could let an older guard steal it.
pub(in crate::adaptive_resources::runtime) struct CopyCloseClaim {
    custody: Arc<CopyCleanupCustody>,
}

impl CopyCloseClaim {
    pub(in crate::adaptive_resources::runtime) fn matches(
        &self,
        custody: &Arc<CopyCleanupCustody>,
    ) -> bool {
        Arc::ptr_eq(&self.custody, custody) && self.custody.state.load(Ordering::Acquire) == CLAIMED
    }

    pub(in crate::adaptive_resources::runtime) fn complete(self) {
        // Called only after the original finalizer's actual successful result.
        self.custody.state.store(FINISHED, Ordering::Release);
    }
}

impl Drop for CopyCloseClaim {
    fn drop(&mut self) {
        let _ = self.custody.state.compare_exchange(
            CLAIMED,
            REPAIR_REQUESTED,
            Ordering::Release,
            Ordering::Relaxed,
        );
    }
}
