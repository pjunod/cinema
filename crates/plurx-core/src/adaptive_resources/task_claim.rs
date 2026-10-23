//! Kernel-task peak claim derived from the durable root-process count.
//!
//! No selected producer currently has an enforced total task bound. A nonzero
//! process-root reservation therefore carries an unknown task peak until the
//! original process owner proves cleanup and the durable vector reaches zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TaskClaim {
    NoProducer,
    UnknownProducerPeak,
}

impl TaskClaim {
    pub(crate) fn from_processes(processes: u64) -> Self {
        if processes == 0 {
            Self::NoProducer
        } else {
            Self::UnknownProducerPeak
        }
    }

    pub(crate) fn is_active_producer(self) -> bool {
        matches!(self, Self::UnknownProducerPeak)
    }
}
