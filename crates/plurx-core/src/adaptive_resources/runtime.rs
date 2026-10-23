//! One node-local custody/accounting owner, composed before startup cleanup.
//!
//! Reconstruction never recreates acquisition handles or physical receipts.
//! The journal remains separate from replicated Store and advisory telemetry.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::sync::Arc;
use std::sync::Mutex;
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

#[cfg(unix)]
pub mod finite_vod;
mod operations;
#[cfg(target_os = "linux")]
pub(crate) mod recovery;
#[cfg(unix)]
mod response_memory;
#[cfg(unix)]
pub(super) use operations::bootstrap::retirement::RetainedFiniteClosure;
#[cfg(unix)]
pub(super) use operations::finite_cancel::FiniteCancellationClosure;
#[cfg(unix)]
pub(super) use operations::finite_retirement::FiniteRetirementClosure;
#[cfg(unix)]
pub use operations::source_files::RetainedSourceLease;

/// Nonclone, nonserde, same-live-operation acquisition capability. Only the
/// retained operation bridge constructs it AFTER exact durable arm commit.
pub(super) struct DurableArm {
    binding: ReservationBinding,
    generation: Uuid,
}
impl DurableArm {
    /// Unit fixtures exercise physical adapter bounds without claiming a journal
    /// commit. Absent from every production build; never recovery evidence.
    #[cfg(test)]
    pub(super) fn adapter_fixture(binding: ReservationBinding, generation: Uuid) -> Self {
        Self {
            binding,
            generation,
        }
    }

    pub(super) fn matches(&self, binding: ReservationBinding, generation: Uuid) -> bool {
        self.binding == binding && self.generation == generation
    }
}

/// Runtime authority is constructed only through core startup's retained daemon
/// lock. Callers can inspect data, but cannot extract its ledger or journal to
/// bypass write-ahead custody. Unsupported physical/recovery backends still
/// prohibit production migration, independently of the private operation book.
pub struct NodeResourceRuntime {
    // The blocking owner also retains the actual daemon lock through queue drain.
    journal: CustodyJournal,
    owner: OwnerIdentity,
    recovered: RecoverySnapshot,
    ledger: ResourceLedger,
    /// Advisory cost summaries only. The journal and ledger never consult
    /// these records for admission, physical closure or refund.
    #[cfg(target_os = "linux")]
    history: Arc<Mutex<crate::resource_history::ResourceHistory>>,
    recovered_bindings: BTreeMap<Uuid, ReservationBinding>,
    // Reopened native S owners remain under their complete restored holds.
    // Import alone cannot authorize serving or old-owner journal settlement.
    #[cfg(target_os = "linux")]
    recovered_s: Mutex<BTreeMap<Uuid, Arc<recovery::ImportedRetainedS>>>,
    #[cfg(target_os = "linux")]
    recovered_r: Mutex<BTreeMap<Uuid, Arc<recovery::ImportedActiveR>>>,
    #[cfg(target_os = "linux")]
    recovered_adoptions: Mutex<BTreeMap<Uuid, Arc<tokio::sync::Mutex<recovery::RecoveryAdoption>>>>,
    operations: Mutex<operations::Operations>,
    // Immutable actual daemon configuration, not a path authorized by audit JSON.
    rendition_root: Option<PathBuf>,
}

impl NodeResourceRuntime {
    /// Blocking startup composition. There is deliberately no empty fallback on
    /// open or reconstruction failure, and no retry as FirstInstallation.
    pub fn open(options: OpenOptions) -> Result<Self, RuntimeOpenError> {
        #[cfg(target_os = "linux")]
        let history_directory = options.data_dir.clone();
        let owner = options.owner.clone();
        let (journal, recovered) = CustodyJournal::open(options)?;
        #[cfg(target_os = "linux")]
        let history = {
            let unix_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
                .unwrap_or(0);
            // The journal-restored installation identity persists across daemon
            // restarts; a fresh daemon owner UUID would discard every prior.
            Arc::new(Mutex::new(crate::resource_history::ResourceHistory::open(
                &history_directory,
                recovered.installation_id,
                unix_ms,
            )))
        };
        // Physical hard bounds have not been observed yet. Unknown is NOT an
        // unbounded cap or zero usage; every recovered cost still counts in full.
        let (ledger, recovered_bindings) = restore_accounting(
            0,
            Metric::Unknown(UnknownReason::InsufficientEvidence),
            &recovered,
        )?;
        Ok(Self {
            journal,
            owner,
            recovered,
            ledger,
            #[cfg(target_os = "linux")]
            history,
            recovered_bindings,
            #[cfg(target_os = "linux")]
            recovered_s: Mutex::new(BTreeMap::new()),
            #[cfg(target_os = "linux")]
            recovered_r: Mutex::new(BTreeMap::new()),
            #[cfg(target_os = "linux")]
            recovered_adoptions: Mutex::new(BTreeMap::new()),
            operations: Mutex::new(operations::Operations::default()),
            rendition_root: None,
        })
    }

    /// Bind the real configured output root before this runtime is shared.
    /// This does not open/create/adopt the directory or authorize uncharged IO.
    pub fn open_with_rendition_root(
        options: OpenOptions,
        root: PathBuf,
    ) -> Result<Self, RuntimeOpenError> {
        let mut runtime = Self::open(options)?;
        runtime.rendition_root = Some(root);
        #[cfg(target_os = "linux")]
        runtime.begin_recovered_s_imports(Instant::now() + std::time::Duration::from_secs(30))?;
        Ok(runtime)
    }

    pub fn accounting(&self) -> Result<LedgerSnapshot, AdmissionRefusal> {
        self.ledger.snapshot()
    }

    /// Refresh actual node/filesystem headroom before compiling an ordinary
    /// finite demand. This exposes no ledger, journal or caller-supplied cap.
    /// Physical reads happen outside the ledger lock; reservation still checks
    /// the newest sample and every live claim atomically. Samples expire in one
    /// second and never rewrite an existing allocation's custody generation.
    pub fn observe_admission_capacity(&self) -> Result<u64, AdmissionRefusal> {
        let root = self
            .rendition_root
            .as_deref()
            .ok_or(AdmissionRefusal::Unavailable)?;
        let sample = crate::resource_limits::admission::snapshot(root);
        self.ledger
            .observe_capacity(sample.capacity, sample.measured_at)
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

    #[cfg(not(target_os = "linux"))]
    pub fn has_unreconciled_custody(&self) -> bool {
        self.has_recovered_obligations()
    }

    /// Complete only the supported exact recovery operations before ordinary
    /// cleanup/serving. Unknown old domains remain fully held and refuse startup.
    pub async fn reconcile_startup_recovery(&self) -> Result<(), RuntimeOpenError> {
        #[cfg(target_os = "linux")]
        self.reconcile_recovered_s_imports(Instant::now() + std::time::Duration::from_secs(30))
            .await?;
        Ok(())
    }

    pub fn protected_paths(&self) -> impl Iterator<Item = &Path> {
        self.recovered.protected_paths.iter().map(|p| p.as_path())
    }

    /// Blocking final-owner shutdown, not request cancellation or settlement.
    /// Charges remain durable; joining the DB owner grants no physical refund.
    pub fn drain(self) -> Result<(), CustodyError> {
        self.journal.drain()
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
    #[cfg(target_os = "linux")]
    let pid_namespace = crate::resource_custody::recovery::current_pid_namespace_token();
    #[cfg(not(target_os = "linux"))]
    let pid_namespace: Option<String> = None;
    #[cfg(target_os = "linux")]
    let continuity_provider = pid_namespace
        .as_ref()
        .map(|_| crate::resource_custody::recovery::PID_NAMESPACE_PROVIDER.to_owned());
    #[cfg(not(target_os = "linux"))]
    let continuity_provider: Option<String> = None;
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
            namespace: pid_namespace,
            continuity_provider,
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
    let mut asset_ids = BTreeSet::new();
    let mut assets = BTreeSet::new();
    for asset in &recovered.retained_assets {
        if !ids.contains(&asset.obligation_id)
            || referenced.contains(&asset.obligation_id)
            || !asset_ids.insert(asset.obligation_id)
            || !assets.insert(asset.custody.asset_id())
            || asset.custody.target_obligation_id().uuid() != asset.obligation_id
        {
            return Err(invalid);
        }
        let record = recovered
            .obligations
            .iter()
            .find(|r| r.obligation_id == asset.obligation_id)
            .ok_or(invalid)?;
        let crate::resource_custody::ObligationInventory::TypedV2 { manifest, .. } =
            &record.inventory
        else {
            return Err(invalid);
        };
        use crate::resource_custody::codec_v2::{self as v2, RetainedAssetsDocument};
        match &asset.custody {
            RetainedAssetsDocument::V1(custody) => {
                v2::validate_retained_assets(manifest, custody).map_err(|_| invalid)?;
                v2::validate_retained_resources(manifest, record.resources).map_err(|_| invalid)?;
            }
            RetainedAssetsDocument::V2(custody) => {
                // CustodyJournal::open already replayed the original producer
                // fold and matched the full inventory to this compact ref.
                v2::validate_retained_assets_v2(manifest, custody).map_err(|_| invalid)?;
            }
        }
    }
    referenced.extend(asset_ids);
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
                // This is the fresh process-local ledger generation. Keep the
                // prior allocation epoch in the durable journal record; the
                // new ledger identity already makes old capabilities stale.
                allocation_epoch: epoch,
            };
            state.entries.insert(
                id,
                Entry {
                    binding,
                    // Full multi-family edges remain in recovered.families.
                    // No single live family is allowed to adopt old custody.
                    family_role: None,
                    resources: record.resources,
                    memory_claim: record.memory_claim,
                    state: ReservationState::UnknownHeld,
                    custody: None,
                    durable: Some(record.obligation_id),
                },
            );
            bindings.insert(record.obligation_id, binding);
            if record.trial_group_id.is_some() {
                trial_members.insert(id);
            }
        }
        state.used = total;
        // Conflicting restored producer claims keep complete custody and
        // accounting, while refusing every new expansion until reconciled.
        if state.memory_claims_fit(&[]).is_err() {
            state.invalid = true;
        }
        state.trial = (!trial_members.is_empty()).then_some(trial_members);
    }
    Ok((ledger, bindings))
}

#[cfg(test)]
mod tests;
