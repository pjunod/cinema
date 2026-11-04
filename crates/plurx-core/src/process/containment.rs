//! Owned native descendant containment. Kernel limits and task extinction are
//! separate facts; neither is evidence that a whole playback graph is bounded.

use std::io;

/// Finite kernel limits configured before any child enters a domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeDomainLimits {
    memory_bytes: u64,
    tasks: u32,
}

impl NativeDomainLimits {
    pub fn new(memory_bytes: u64, tasks: u32) -> io::Result<Self> {
        // The platform factory also validates the actual kernel page size.
        if memory_bytes == 0 || tasks == 0 || memory_bytes > i64::MAX as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "finite positive native limits required",
            ));
        }
        Ok(Self {
            memory_bytes,
            tasks,
        })
    }

    pub fn memory_bytes(self) -> u64 {
        self.memory_bytes
    }
    pub fn tasks(self) -> u32 {
        self.tasks
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{
    NativeDomainCreateFailure, NativeDomainExtinction, NativeDomainIdentity, NativeDomainRoot,
    NativeFactoryCleanupSummary, OwnedNativeDomain,
};

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "Fixture assertions fail the test on invalid setup."
)]
mod tests {
    use super::*;

    #[test]
    fn native_limits_refuse_zero_and_nonfinite_values() {
        assert!(NativeDomainLimits::new(0, 1).is_err());
        assert!(NativeDomainLimits::new(4096, 0).is_err());
        assert!(NativeDomainLimits::new(u64::MAX, 1).is_err());
        assert_eq!(NativeDomainLimits::new(65536, 2).unwrap().tasks(), 2);
    }
}
