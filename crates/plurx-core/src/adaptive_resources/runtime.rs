//! One node-local custody/accounting owner, composed before startup cleanup.
//!
//! Reconstruction never recreates acquisition handles or physical receipts.
//! The journal remains separate from replicated Store and advisory telemetry.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Instant;

use uuid::Uuid;

use super::{
    physical_graph, AdmissionRefusal, Entry, LedgerSnapshot, ReservationBinding, ReservationState,
    ResourceLedger, ResourceVector, MAX_RESERVATIONS,
};
use crate::process::observation::{Metric, ProcessObserver, UnknownReason};
use crate::resource_custody::{
    BootIdentity, CustodyError, CustodyJournal, OpenOptions, OwnerIdentity, Phase, PlatformDomain,
    ProcessIdentity, RecoverySnapshot, Role,
};

/// Runtime authority is constructed only through core startup's retained daemon
/// lock. Callers can inspect data, but cannot extract its ledger or journal to
/// bypass write-ahead custody. No production launch/settlement API exists yet.
pub struct NodeResourceRuntime {
    // The blocking owner also retains the actual daemon lock through queue drain.
    _journal: CustodyJournal,
    owner: OwnerIdentity,
    recovered: RecoverySnapshot,
    ledger: ResourceLedger,
    recovered_bindings: BTreeMap<Uuid, ReservationBinding>,
}

impl NodeResourceRuntime {
    /// Blocking startup composition. There is deliberately no empty fallback on
    /// open or reconstruction failure, and no retry as FirstInstallation.
    pub fn open(options: OpenOptions) -> Result<Self, RuntimeOpenError> {
        let owner = options.owner.clone();
        let (journal, recovered) = CustodyJournal::open(options)?;
        // Physical hard bounds have not been observed yet. Unknown is NOT an
        // unbounded cap or zero usage; every recovered cost still counts in full.
        let (ledger, recovered_bindings) = restore_accounting(
            0,
            Metric::Unknown(UnknownReason::InsufficientEvidence),
            &recovered,
        )?;
        Ok(Self {
            _journal: journal,
            owner,
            recovered,
            ledger,
            recovered_bindings,
        })
    }

    pub fn accounting(&self) -> Result<LedgerSnapshot, AdmissionRefusal> {
        self.ledger.snapshot()
    }

    /// Complete cleanup-only diagnostic data, not launch, fencing or refund
    /// capabilities. In particular, persisted Prepared is not live Reserved.
    pub fn recovery(&self) -> &RecoverySnapshot {
        &self.recovered
    }

    pub fn owner(&self) -> &OwnerIdentity {
        &self.owner
    }

    pub fn has_recovered_obligations(&self) -> bool {
        !self.recovered_bindings.is_empty()
    }

    pub fn protected_paths(&self) -> impl Iterator<Item = &Path> {
        self.recovered.protected_paths.iter().map(|p| p.as_path())
    }

    /// Blocking final-owner shutdown, not request cancellation or settlement.
    /// Charges remain durable; joining the DB owner grants no physical refund.
    pub fn drain(self) -> Result<(), CustodyError> {
        self._journal.drain()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeOpenError {
    #[error("{0}")]
    Custody(#[from] CustodyError),
    #[error("resource custody accounting reconstruction unavailable")]
    Accounting(AdmissionRefusal),
}

impl From<AdmissionRefusal> for RuntimeOpenError {
    fn from(refusal: AdmissionRefusal) -> Self {
        Self::Accounting(refusal)
    }
}

/// Actual process instance is shared with GraphIdentity. Native process and boot
/// observations remain diagnostic: missing continuity is never replaced by a
/// random host identity, configured node ID or sampled wall clock.
pub(crate) fn daemon_owner(node_id: String, cluster_id: String) -> OwnerIdentity {
    let (owner_id, boot) = physical_graph::daemon_identity();
    let creation_marker = match ProcessObserver::new(std::process::id())
        .snapshot(Instant::now())
        .counters
    {
        Metric::Known(counters) => Some(counters.identity.start_marker),
        Metric::Unknown(_) => None,
    };
    let os_boot = match boot {
        Metric::Known(value) => BootIdentity::Known {
            value,
            provider: match std::env::consts::OS {
                "linux" => "linux_kernel_boot_id",
                "macos" => "macos_kernel_bootsessionuuid",
                _ => "unsupported",
            }
            .into(),
            version: 1,
        },
        Metric::Unknown(_) => BootIdentity::Unknown {
            reason: "native boot observation unavailable".into(),
        },
    };
    OwnerIdentity {
        owner_id,
        node_id,
        cluster_id,
        os_boot,
        daemon_process: ProcessIdentity {
            version: 1,
            pid: std::process::id(),
            creation_marker,
            // This scopes the marker's interpretation; it is NOT a positively
            // identified host or namespace continuity provider.
            domain: "local_process_observation_v1".into(),
        },
        platform_domain: PlatformDomain {
            version: 1,
            platform: std::env::consts::OS.into(),
            host_identity: None,
            namespace: None,
            continuity_provider: None,
        },
    }
}

fn role_key(role: Role) -> u8 {
    match role {
        Role::Video => 0,
        Role::Audio => 1,
        Role::Companion => 2,
    }
}

/// Runs after complete journal validation, before publishing the runtime. Fresh
/// local bindings replace disk IDs as accounting identity. No ResourceReservation,
/// RetainedRole, CleanupAuthority or graph acquisition capability is returned.
fn restore_accounting(
    epoch: u64,
    cap: Metric<ResourceVector>,
    recovered: &RecoverySnapshot,
) -> Result<(ResourceLedger, BTreeMap<Uuid, ReservationBinding>), AdmissionRefusal> {
    let invalid = AdmissionRefusal::Unavailable;
    if recovered.journal_id.is_nil()
        || recovered.installation_id.is_nil()
        || recovered.obligations.len() > MAX_RESERVATIONS
    {
        return Err(invalid);
    }
    let mut total = ResourceVector::default();
    let mut ids = BTreeSet::new();
    let mut graphs = BTreeSet::new();
    let mut trials = BTreeSet::new();
    let mut edges = BTreeSet::new();
    let mut referenced = BTreeSet::new();
    let mut roles = BTreeMap::new();
    for record in &recovered.obligations {
        if !record.cleanup_only
            || record.phase == Phase::Settled
            || record.obligation_id.is_nil()
            || record.graph_id.is_nil()
            || record.owner_id.is_nil()
            || !ids.insert(record.obligation_id)
            || !graphs.insert(record.graph_id)
        {
            return Err(invalid);
        }
        total = total.checked_add(record.resources).ok_or(invalid)?;
        if let Some(trial) = record.trial_group_id {
            if trial.is_nil() {
                return Err(invalid);
            }
            trials.insert(trial);
        }
    }
    let declared_trials: BTreeSet<_> = recovered.trial_groups.iter().copied().collect();
    if total != recovered.total
        || trials != declared_trials
        || declared_trials.len() != recovered.trial_groups.len()
        || trials.len() > 1
    {
        return Err(invalid);
    }
    for edge in &recovered.families {
        if edge.family_id.is_nil()
            || !ids.contains(&edge.obligation_id)
            || !edges.insert((edge.family_id, role_key(edge.role)))
        {
            return Err(invalid);
        }
        if roles
            .insert(edge.obligation_id, edge.role)
            .is_some_and(|existing| existing != edge.role)
        {
            return Err(invalid);
        }
        referenced.insert(edge.obligation_id);
    }
    if referenced != ids {
        return Err(invalid);
    }

    let ledger = ResourceLedger::new(epoch, cap)?;
    let mut bindings = BTreeMap::new();
    // No I/O/await or OS query occurs under this short private accounting lock.
    // The ledger is not visible until the complete reconstruction succeeds.
    {
        let mut state = ledger.inner.lock().map_err(|_| invalid)?;
        let mut trial_members = BTreeSet::new();
        for record in &recovered.obligations {
            let id = state.next_reservation;
            state.next_reservation = id.checked_add(1).ok_or(invalid)?;
            let binding = ReservationBinding {
                ledger: ledger.id,
                reservation: id,
                allocation_epoch: record.allocation_epoch,
            };
            state.entries.insert(
                id,
                Entry {
                    binding,
                    // Full multi-family edges remain in recovered.families.
                    // No single live family is allowed to adopt old custody.
                    family_role: None,
                    resources: record.resources,
                    state: ReservationState::UnknownHeld,
                    custody: None,
                },
            );
            bindings.insert(record.obligation_id, binding);
            if record.trial_group_id.is_some() {
                trial_members.insert(id);
            }
        }
        state.used = total;
        state.trial = (!trial_members.is_empty()).then_some(trial_members);
    }
    Ok((ledger, bindings))
}

#[cfg(test)]
mod tests;
