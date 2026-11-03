//! Bounded node-local service hysteresis and refusal-only finite trial policy.
//! Neither a service observation nor these diagnostics grants a reservation.
use super::finite_vod::SoftwareVodRequest;
use super::NodeResourceRuntime;
use crate::adaptive_resources::{FamilyIdentity, FamilyRole, MemoryClaim, ReservationState};
use crate::cluster::clock::ContinuousDeadline;
use crate::process::observation::{Metric, UnknownReason};
use crate::resource_observation::{ServiceUnit, ServiceWindow, WorkPhase, WorkSample};
use crate::resource_policy::{GrowthState, TrialAbort, TrialBudget, TrialRefusal};
use std::time::{Duration, Instant};

// Fixed startup storage; observation never allocates or evicts a cooldown.
// Exhaustion loses expansion availability rather than forgetting a failure.
const MAX_CLASSES: usize = 16;
const FRESHNESS: Duration = Duration::from_secs(15);
type Class = ([u8; 32], [u8; 32]);

fn finite_trial_budget(memory: MemoryClaim, original_preparation_cutoff: Instant) -> TrialBudget {
    TrialBudget {
        original_deadline: original_preparation_cutoff,
        activation_allowance: Duration::ZERO,
        peer_prepare_estimate: Metric::Unknown(UnknownReason::InsufficientEvidence),
        // No authenticated positive peer fact was supplied. False means
        // no KNOWN capacity peer, never proof that no peer exists.
        known_capacity_peer: false,
        startup_bound: Metric::Unknown(UnknownReason::Unbounded),
        cleanup_bound: Metric::Unknown(UnknownReason::Unbounded),
        memory_bound_known: matches!(memory, MemoryClaim::KnownBounded { .. }),
    }
}

fn finite_trial_refusal_for(
    memory: MemoryClaim,
    original: Option<&ContinuousDeadline>,
    original_preparation_cutoff: Instant,
    now: Instant,
) -> TrialRefusal {
    if now >= original_preparation_cutoff
        || original.is_none_or(|deadline| {
            deadline.entry_remaining().is_err()
                || !deadline
                    .monotonic_cutoff()
                    .is_ok_and(|cutoff| cutoff == original_preparation_cutoff)
        })
    {
        return TrialRefusal::DeadlineBudget;
    }
    match finite_trial_budget(memory, original_preparation_cutoff).allocate(now) {
        Err(refusal) => refusal,
        // Even a future bounded pump cannot turn missing task peaks,
        // inventory or peer evidence into a trial permit through this API.
        Ok(_) => TrialRefusal::InsufficientEvidence,
    }
}

struct GrowthEntry {
    class: Class,
    family: FamilyIdentity,
    generation: u64,
    window: ServiceWindow,
    growth: GrowthState,
    last_sample: Option<WorkSample>,
    below_margin: u8,
}

/// Monotonic observations only. The count and timestamp never certify native
/// containment, complete incumbent inventory, or available capacity. Growth
/// describes only the latest observed class, never all work on this node.
#[derive(Clone, Copy, Debug)]
pub struct FinitePolicyServiceDiagnostics {
    pub accepted_samples: u64,
    pub last_sample: Option<WorkSample>,
    pub growth_refusal: Option<TrialRefusal>,
}

pub(super) struct FinitePolicyRegistry {
    entries: [Option<GrowthEntry>; MAX_CLASSES],
    accepted_samples: u64,
    latest: Option<usize>,
}

impl Default for FinitePolicyRegistry {
    fn default() -> Self {
        Self {
            entries: [const { None }; MAX_CLASSES],
            accepted_samples: 0,
            latest: None,
        }
    }
}

impl FinitePolicyRegistry {
    fn invalidate(&mut self, class: Class, now: Instant) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .flatten()
            .find(|entry| entry.class == class)
        {
            entry.growth.failure(now, TrialAbort::EvidenceLost);
            entry.window = ServiceWindow::default();
            entry.below_margin = 0;
        }
    }

    fn observe(
        &mut self,
        class: Class,
        family: FamilyIdentity,
        generation: u64,
        sample: WorkSample,
        now: Instant,
    ) {
        if generation == 0
            || sample.generation != generation
            || sample.unit != ServiceUnit::MediaMilliseconds
            || now
                .checked_duration_since(sample.at)
                .is_none_or(|age| age > FRESHNESS)
        {
            self.invalidate(class, now);
            return;
        }
        let slot = self
            .entries
            .iter()
            .position(|entry| entry.as_ref().is_some_and(|entry| entry.class == class))
            .or_else(|| self.entries.iter().position(Option::is_none));
        let Some(slot) = slot else {
            return;
        };
        let entry = self.entries[slot].get_or_insert_with(|| GrowthEntry {
            class,
            family,
            generation,
            window: ServiceWindow::default(),
            growth: GrowthState::default(),
            last_sample: None,
            below_margin: 0,
        });
        // A late predecessor event cannot replace the current producer or
        // manufacture a new stability timestamp.
        if entry.last_sample.is_some_and(|last| sample.at <= last.at) {
            return;
        }
        if entry.family != family || entry.generation != generation {
            entry.family = family;
            entry.generation = generation;
            entry.window = ServiceWindow::default();
            entry.below_margin = 0;
            entry.growth.observe(sample.at, false);
        }
        let Some(observation) = entry.window.observe(sample) else {
            return;
        };
        entry.last_sample = Some(sample);
        self.accepted_samples = self.accepted_samples.saturating_add(1);
        self.latest = Some(slot);
        let producing = sample.phase == WorkPhase::Producing;
        let stable = producing
            && matches!(observation.units_per_second, Metric::Known(rate) if rate >= 1250);
        entry.growth.observe(sample.at, stable);
        if producing && matches!(sample.deadline_slack_ms, Metric::Known(slack) if slack < 0) {
            entry.growth.failure(sample.at, TrialAbort::DeadlineMiss);
        }
        if producing && matches!(observation.units_per_second, Metric::Known(rate) if rate < 1250) {
            entry.below_margin = entry.below_margin.saturating_add(1);
            if entry.below_margin >= 2 {
                entry.growth.failure(sample.at, TrialAbort::ProducingMargin);
            }
        } else {
            entry.below_margin = 0;
        }
    }

    fn readiness(&self, class: Class, now: Instant) -> Result<(), TrialRefusal> {
        self.entries
            .iter()
            .flatten()
            .find(|entry| entry.class == class)
            .ok_or(TrialRefusal::InsufficientEvidence)?
            .growth
            .can_expand(now)
    }

    fn diagnostics(&self, now: Instant) -> FinitePolicyServiceDiagnostics {
        let latest = self.latest.and_then(|slot| self.entries[slot].as_ref());
        FinitePolicyServiceDiagnostics {
            accepted_samples: self.accepted_samples,
            last_sample: latest.and_then(|entry| entry.last_sample),
            growth_refusal: latest.map_or(Some(TrialRefusal::InsufficientEvidence), |entry| {
                self.readiness(entry.class, now).err()
            }),
        }
    }
}

impl NodeResourceRuntime {
    /// Refusal-only adapter before a selected software profile can be built.
    /// That backend still has an unknown producer/task peak; absent profile
    /// data cannot become a bounded memory claim or a fresh time allowance.
    pub fn finite_unprofiled_trial_refusal(
        &self,
        original: Option<&ContinuousDeadline>,
        original_preparation_cutoff: Instant,
        now: Instant,
    ) -> TrialRefusal {
        finite_trial_refusal_for(
            MemoryClaim::UnknownProducerPeak,
            original,
            original_preparation_cutoff,
            now,
        )
    }

    pub(super) fn invalidate_finite_policy_service(
        &self,
        class: Class,
        family: FamilyIdentity,
        now: Instant,
    ) {
        if let Ok(mut policy) = self.finite_policy.lock() {
            if policy
                .entries
                .iter()
                .flatten()
                .any(|entry| entry.class == class && entry.family == family)
            {
                policy.invalidate(class, now);
            }
        }
    }

    /// The original preparation deadline already excludes activation A. This
    /// returns a refusal only: current FFmpeg memory/task peaks, startup and
    /// cleanup, full incumbent inventory and a pre-Winner trial owner are not
    /// verified. This candidate refusal never constrains successful
    /// conservative admission.
    pub fn finite_trial_refusal(
        &self,
        request: &SoftwareVodRequest,
        original_preparation_cutoff: Instant,
        now: Instant,
    ) -> TrialRefusal {
        finite_trial_refusal_for(
            request.producer.memory_claim(),
            request.continuous_entry_deadline.as_ref(),
            original_preparation_cutoff,
            now,
        )
    }

    pub(super) fn observe_finite_policy_service(
        &self,
        class: Class,
        producer: (FamilyIdentity, u64, u64),
        sample: WorkSample,
        now: Instant,
    ) {
        let (family, generation, limit_epoch) = producer;
        let current = self.ledger.inner.lock().is_ok_and(|state| {
            state.epoch == limit_epoch
                && !state.invalid
                && !state.draining
                && !state.native_overage_fenced
                && state.entries.values().any(|entry| {
                    entry.family_role == Some((family, FamilyRole::Video))
                        && entry.resources.processes != 0
                        && !matches!(
                            entry.state,
                            ReservationState::UnknownHeld | ReservationState::Reaping
                        )
                })
        });
        let Ok(mut policy) = self.finite_policy.lock() else {
            return;
        };
        if current {
            policy.observe(class, family, generation, sample, now);
        } else {
            policy.invalidate(class, now);
        }
    }

    /// Nonblocking data only; poisoning loses evidence, never opens admission.
    pub fn finite_policy_service_diagnostics(
        &self,
        now: Instant,
    ) -> FinitePolicyServiceDiagnostics {
        self.finite_policy
            .try_lock()
            .map(|policy| policy.diagnostics(now))
            .unwrap_or(FinitePolicyServiceDiagnostics {
                accepted_samples: 0,
                last_sample: None,
                growth_refusal: Some(TrialRefusal::InsufficientEvidence),
            })
    }
}

#[cfg(test)]
mod tests;
