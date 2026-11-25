//! Kernel-task peak claims for process-root reservations.
//!
//! A finite producer selected for a contained native domain has its total
//! task bound decided before admission: the request's native plan names the
//! exact `pids.max` (and `memory.max`) that its domain receives before any
//! child enters it, and entry fails unless the kernel reports those values.
//! With known node capacity such a reservation carries
//! [`TaskClaim::KnownBounded`].
//!
//! Every other nonzero process-root reservation (legacy and uncontained roots,
//! and every reservation reconstructed after a restart, whose ceiling is not
//! persisted) carries an unknown task peak until the original process owner
//! proves cleanup and the durable vector reaches zero.
use crate::process::containment::NativeDomainLimits;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TaskClaim {
    NoProducer,
    /// Every task of this producer, including its root, runs in its own
    /// kernel domain whose task ceiling was fixed before entry.
    KnownBounded {
        tasks: u64,
    },
    UnknownProducerPeak,
}

impl TaskClaim {
    /// Derivation without containment evidence: a nonzero process-root count
    /// is an unknown peak. Restart reconstruction uses this deliberately.
    pub(crate) fn from_processes(processes: u64) -> Self {
        if processes == 0 {
            Self::NoProducer
        } else {
            Self::UnknownProducerPeak
        }
    }

    pub(crate) fn is_active_producer(self) -> bool {
        !matches!(self, Self::NoProducer)
    }
}

/// Kernel ceilings of one producer's own delegated native domain, taken from
/// the request's native plan before admission. The ledger turns these into
/// known memory and task claims only while it also has known node capacity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ContainedProducerBound {
    memory_bytes: u64,
    tasks: u32,
}

impl ContainedProducerBound {
    #[cfg_attr(
        not(any(test, target_os = "linux")),
        allow(
            dead_code,
            reason = "Only the Linux finite native plan selects a contained domain."
        )
    )]
    pub(crate) fn from_limits(limits: NativeDomainLimits) -> Self {
        Self {
            memory_bytes: limits.memory_bytes(),
            tasks: limits.tasks(),
        }
    }

    pub(crate) fn memory_bytes(self) -> u64 {
        self.memory_bytes
    }

    pub(crate) fn tasks(self) -> u64 {
        u64::from(self.tasks)
    }
}
