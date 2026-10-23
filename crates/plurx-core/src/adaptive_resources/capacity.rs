//! Independent known/unknown ceilings. Missing RAM evidence must not erase
//! measured CPU or handle limits, and zero expansion needs no invented limit.
use super::{AdmissionRefusal, ResourceDimension, ResourceVector};
use crate::process::observation::{Metric, UnknownReason};

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct ResourceCapacity {
    pub cpu_ns_per_second: Metric<u64>,
    pub memory_bytes: Metric<u64>,
    pub handles: Metric<u64>,
    pub processes: Metric<u64>,
    pub hardware_slots: Metric<u64>,
    pub tuner_slots: Metric<u64>,
    pub scratch_bytes: Metric<u64>,
}
impl Default for ResourceCapacity {
    fn default() -> Self {
        Self::from(Metric::Unknown(UnknownReason::InsufficientEvidence))
    }
}
impl From<Metric<ResourceVector>> for ResourceCapacity {
    fn from(value: Metric<ResourceVector>) -> Self {
        match value {
            Metric::Known(v) => Self {
                cpu_ns_per_second: Metric::Known(v.cpu_ns_per_second),
                memory_bytes: Metric::Known(v.peak_memory_bytes),
                handles: Metric::Known(v.handles),
                processes: Metric::Known(v.processes),
                hardware_slots: Metric::Known(v.hardware_slots),
                tuner_slots: Metric::Known(v.tuner_slots),
                scratch_bytes: Metric::Known(v.scratch_bytes),
            },
            Metric::Unknown(reason) => Self {
                cpu_ns_per_second: Metric::Unknown(reason),
                memory_bytes: Metric::Unknown(reason),
                handles: Metric::Unknown(reason),
                processes: Metric::Unknown(reason),
                hardware_slots: Metric::Unknown(reason),
                tuner_slots: Metric::Unknown(reason),
                scratch_bytes: Metric::Unknown(reason),
            },
        }
    }
}
impl ResourceCapacity {
    /// Unknown RAM permits only the explicit serialized-cold producer policy.
    /// Its numeric memory remains a bounded component, never a fictitious peak.
    pub(super) fn check(
        self,
        total: ResourceVector,
        expansion: ResourceVector,
        serialized_cold: bool,
    ) -> Result<(), AdmissionRefusal> {
        for (dimension, cap, held, added) in [
            (
                ResourceDimension::Cpu,
                self.cpu_ns_per_second,
                total.cpu_ns_per_second,
                expansion.cpu_ns_per_second,
            ),
            (
                ResourceDimension::Memory,
                self.memory_bytes,
                total.peak_memory_bytes,
                expansion.peak_memory_bytes,
            ),
            (
                ResourceDimension::Handles,
                self.handles,
                total.handles,
                expansion.handles,
            ),
            (
                ResourceDimension::Processes,
                self.processes,
                total.processes,
                expansion.processes,
            ),
            (
                ResourceDimension::Hardware,
                self.hardware_slots,
                total.hardware_slots,
                expansion.hardware_slots,
            ),
            (
                ResourceDimension::Tuner,
                self.tuner_slots,
                total.tuner_slots,
                expansion.tuner_slots,
            ),
            (
                ResourceDimension::Scratch,
                self.scratch_bytes,
                total.scratch_bytes,
                expansion.scratch_bytes,
            ),
        ] {
            match cap {
                Metric::Known(limit) if held > limit => {
                    return Err(AdmissionRefusal::HardLimit(dimension))
                }
                Metric::Unknown(_)
                    if added != 0
                        && !(serialized_cold && dimension == ResourceDimension::Memory) =>
                {
                    return Err(AdmissionRefusal::InsufficientEvidence);
                }
                _ => {}
            }
        }
        Ok(())
    }
}
