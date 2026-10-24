//! Narrow, cleanup-only recovery selection. These values are minted from the
//! validated journal under its original installation lock; none is a receipt.
use super::*;
use crate::resource_custody::codec_v2::scratch::bootstrap::BootstrapFold;
use crate::resource_custody::codec_v2::{self as v2, scratch::bootstrap::FrozenScratchV3};
use rusqlite::Connection;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::sync::Mutex;

pub(crate) const PID_NAMESPACE_PROVIDER: &str = "linux_kernel_pid_namespace_v1";

/// The nsfs device/inode of the kernel PID namespace, not the textual target
/// of a caller-provided path. Only the fixed runtime and this recovery module
/// use it; an unavailable proc observation leaves custody unrecoverable.
pub(crate) fn pid_namespace_token(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    let metadata = std::fs::metadata(format!("/proc/{pid}/ns/pid")).ok()?;
    let device = metadata.dev();
    let inode = metadata.ino();
    (device != 0 && inode != 0).then(|| format!("{device}:{inode}"))
}

pub(crate) fn current_pid_namespace_token() -> Option<String> {
    // A host-mounted /proc can resolve numeric PIDs in another namespace.
    // Require this proc view to report our own namespace-local numeric PID.
    let pid = std::process::id();
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let (prefix, _) = stat.rsplit_once(')')?;
    let (reported_pid, _) = prefix.split_once('(')?;
    (reported_pid.trim().parse::<u32>().ok()? == pid)
        .then(|| pid_namespace_token(pid))
        .flatten()
}

fn old_process_exited(pid: u32, creation_marker: u64) -> bool {
    if pid == 0 || creation_marker == 0 {
        return false;
    }
    let path = format!("/proc/{pid}/stat");
    let mut stat = String::new();
    match std::fs::File::open(path) {
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        Ok(file) => {
            if file.take(16 * 1024).read_to_string(&mut stat).is_err() {
                return false;
            }
            let Some((prefix, rest)) = stat.rsplit_once(')') else {
                return false;
            };
            let Some((reported_pid, _)) = prefix.split_once('(') else {
                return false;
            };
            if reported_pid.trim().parse::<u32>().ok() != Some(pid) {
                return false;
            }
            rest.split_whitespace()
                .nth(19)
                .and_then(|value| value.parse::<u64>().ok())
                .is_some_and(|marker| marker != creation_marker)
        }
    }
}

/// Positive exit of the old daemon's own task/reader domain on the same Linux
/// boot. This says nothing about producer children that could have survived it.
pub(crate) struct PositivePreviousOwnerExit {
    journal_id: Uuid,
    installation_id: Uuid,
    old_owner: OwnerIdentity,
    current_owner: OwnerIdentity,
}

impl PositivePreviousOwnerExit {
    pub(crate) fn owner_id(&self) -> Uuid {
        self.old_owner.owner_id
    }
    pub(crate) fn closing_owner_id(&self) -> Uuid {
        self.current_owner.owner_id
    }
    pub(crate) fn matches(
        &self,
        recovered: &RecoverySnapshot,
        current: &OwnerIdentity,
        old_owner_id: Uuid,
    ) -> bool {
        self.journal_id == recovered.journal_id
            && self.installation_id == recovered.installation_id
            && self.old_owner.owner_id == old_owner_id
            && self.current_owner.owner_id == current.owner_id
            && self.current_owner.node_id == current.node_id
            && self.current_owner.cluster_id == current.cluster_id
            && self.current_owner.os_boot == current.os_boot
            && self.current_owner.daemon_process == current.daemon_process
            && self.current_owner.platform_domain == current.platform_domain
            && recovered.owners.iter().any(|owner| {
                owner.owner_id == self.old_owner.owner_id
                    && owner.node_id == self.old_owner.node_id
                    && owner.cluster_id == self.old_owner.cluster_id
                    && owner.os_boot == self.old_owner.os_boot
                    && owner.daemon_process == self.old_owner.daemon_process
                    && owner.platform_domain == self.old_owner.platform_domain
            })
    }
}

/// The trusted caller's OpenOptions contains the *held* installation daemon
/// lock, not a pathname supplied by a recovery request. Unknown boot, unknown
/// creation marker, changed boot, inaccessible proc, and a live old process all
/// leave the original obligations held. A missing PID alone never proves exit.
#[cfg(target_os = "linux")]
pub(crate) fn prove_previous_daemon_exit(
    options: &OpenOptions,
    recovered: &RecoverySnapshot,
    old_owner_id: Uuid,
) -> Result<PositivePreviousOwnerExit, CustodyError> {
    let unavailable =
        || CustodyError::new(ErrorKind::IdentityMismatch, "previous daemon exit unproven");
    let old = recovered
        .owners
        .iter()
        .find(|owner| owner.owner_id == old_owner_id)
        .ok_or_else(unavailable)?;
    let current = &options.owner;
    let (
        BootIdentity::Known {
            value: old_boot,
            provider: old_provider,
            version: old_version,
        },
        BootIdentity::Known {
            value: current_boot,
            provider: current_provider,
            version: current_version,
        },
    ) = (&old.os_boot, &current.os_boot)
    else {
        return Err(unavailable());
    };
    if old_boot.is_nil()
        || old_boot != current_boot
        || *old_version != 1
        || *current_version != 1
        || old_provider != "linux_kernel_boot_id"
        || current_provider != old_provider
        || old.node_id != current.node_id
        || old.cluster_id != current.cluster_id
        || old.platform_domain.platform != "linux"
        || current.platform_domain.platform != "linux"
        || old.platform_domain.continuity_provider.as_deref() != Some(PID_NAMESPACE_PROVIDER)
        || current.platform_domain.continuity_provider.as_deref() != Some(PID_NAMESPACE_PROVIDER)
        || old.platform_domain.namespace.is_none()
        || old.platform_domain.namespace.as_ref() != current.platform_domain.namespace.as_ref()
        || current.platform_domain.namespace.as_ref() != current_pid_namespace_token().as_ref()
        || old.daemon_process.version != 1
        || current.daemon_process.version != 1
        || old.daemon_process.domain != "local_process_observation_v1"
        || current.daemon_process.domain != old.daemon_process.domain
        || old.daemon_process.pid == 0
        || current.daemon_process.pid == 0
        || current
            .daemon_process
            .creation_marker
            .is_none_or(|marker| marker == 0)
        || old
            .daemon_process
            .creation_marker
            .is_none_or(|marker| marker == 0)
        || old.owner_id == current.owner_id
    {
        return Err(unavailable());
    }
    // The lock is an owned member of OpenOptions; retaining this reference
    // through the check makes the old daemon's lock ownership incompatible.
    let _held_installation_lock = &options.daemon_lock;
    if !old_process_exited(
        old.daemon_process.pid,
        old.daemon_process.creation_marker.ok_or_else(unavailable)?,
    ) {
        return Err(unavailable());
    }
    Ok(PositivePreviousOwnerExit {
        journal_id: recovered.journal_id,
        installation_id: recovered.installation_id,
        old_owner: old.clone(),
        current_owner: current.clone(),
    })
}

/// A positive observation that one committed fixed software root cannot still
/// own a process on this kernel/PID namespace. It closes no scratch object and
/// grants no ledger credit; the full old R remains held until native cleanup.
#[must_use]
pub(crate) struct PositivePreviousRootExit {
    journal_id: Uuid,
    installation_id: Uuid,
    obligation_id: Uuid,
    graph_id: Uuid,
    old_owner: OwnerIdentity,
    current_owner: OwnerIdentity,
    root_member_id: Uuid,
    pid: u32,
    creation_marker: u64,
    pid_namespace: String,
    executable_binding: String,
    previous_holder: Option<RecoveredRHolder>,
    previous_holder_exit: Option<PositivePreviousOwnerExit>,
}

fn exact_latest_r_holder(
    holders: &[RecoveredRHolder],
    obligation_id: Uuid,
    expected: &RecoveredRHolder,
) -> bool {
    holders
        .iter()
        .filter(|holder| holder.obligation_id == obligation_id)
        .max_by_key(|holder| holder.adoption_sequence)
        == Some(expected)
}

impl PositivePreviousRootExit {
    pub(crate) fn root_member_id(&self) -> Uuid {
        self.root_member_id
    }
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }
    pub(crate) fn creation_marker(&self) -> u64 {
        self.creation_marker
    }
    pub(crate) fn effective_previous_owner_id(&self) -> Uuid {
        self.previous_holder
            .as_ref()
            .map_or(self.old_owner.owner_id, |holder| holder.current_owner_id)
    }
    pub(crate) fn matches(
        &self,
        recovered: &RecoverySnapshot,
        current: &OwnerIdentity,
        obligation_id: Uuid,
        graph_id: Uuid,
        root_member_id: Uuid,
    ) -> bool {
        self.journal_id == recovered.journal_id
            && self.installation_id == recovered.installation_id
            && self.obligation_id == obligation_id
            && self.graph_id == graph_id
            && self.root_member_id == root_member_id
            && self.current_owner.owner_id == current.owner_id
            && self.current_owner.node_id == current.node_id
            && self.current_owner.cluster_id == current.cluster_id
            && self.current_owner.os_boot == current.os_boot
            && self.current_owner.daemon_process == current.daemon_process
            && self.current_owner.platform_domain == current.platform_domain
            && self.pid_namespace == current.platform_domain.namespace.as_deref().unwrap_or("")
            && current.platform_domain.continuity_provider.as_deref()
                == Some(PID_NAMESPACE_PROVIDER)
            && self.pid != 0
            && self.creation_marker != 0
            && !self.executable_binding.is_empty()
            && recovered.owners.iter().any(|owner| {
                owner.owner_id == self.old_owner.owner_id
                    && owner.node_id == self.old_owner.node_id
                    && owner.cluster_id == self.old_owner.cluster_id
                    && owner.os_boot == self.old_owner.os_boot
                    && owner.daemon_process == self.old_owner.daemon_process
                    && owner.platform_domain == self.old_owner.platform_domain
            })
            && recovered.obligations.iter().any(|record| {
                record.obligation_id == obligation_id
                    && record.graph_id == graph_id
                    && record.owner_id == self.old_owner.owner_id
            })
            && match (&self.previous_holder, &self.previous_holder_exit) {
                (None, None) => !recovered
                    .recovered_r_holders
                    .iter()
                    .any(|holder| holder.obligation_id == obligation_id),
                (Some(expected), Some(exit)) => {
                    exact_latest_r_holder(&recovered.recovered_r_holders, obligation_id, expected)
                        && expected.current_owner_id == exit.owner_id()
                        && exit.matches(recovered, current, expected.current_owner_id)
                }
                _ => false,
            }
    }
}

fn fixed_executable_binding(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("software-avc-executable-sha256:") else {
        return false;
    };
    hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn fixed_root_task_relation(
    root: &PlannedMember,
    slot: &v2::MemberSlot,
    task: &v2::VodOutputTask,
    graph_id: v2::Id,
) -> bool {
    slot.owned_by.map(v2::Id::uuid) == Some(root.member_id)
        && slot.depends_on.len() == 2
        && slot.depends_on.iter().any(|id| id.uuid() == root.member_id)
        && task.root_member_id.uuid() == root.member_id
        && task.graph_id == graph_id
        && matches!(&task.actor, v2::VodActor::VodGenerationV1)
}

fn fixed_root_namespace_matches(
    expected: &str,
    old: &OwnerIdentity,
    current: &OwnerIdentity,
) -> bool {
    old.platform_domain.continuity_provider.as_deref() == Some(PID_NAMESPACE_PROVIDER)
        && current.platform_domain.continuity_provider.as_deref() == Some(PID_NAMESPACE_PROVIDER)
        && old.platform_domain.namespace.as_deref() == Some(expected)
        && current.platform_domain.namespace.as_deref() == Some(expected)
        && current_pid_namespace_token().as_deref() == Some(expected)
}

/// Select one old R only from the complete, strictly validated journal image.
/// The old daemon proof was minted while the installation lock was held; this
/// constructor retains that lock and checks a separate, committed ROOT identity.
/// An armed-but-unobserved launch is never selected.
pub(crate) fn prove_previous_fixed_root_exit(
    options: &OpenOptions,
    recovered: &RecoverySnapshot,
    daemon_exit: &PositivePreviousOwnerExit,
    previous_holder_exit: Option<PositivePreviousOwnerExit>,
    obligation_id: Uuid,
) -> Result<PositivePreviousRootExit, CustodyError> {
    let unavailable = || {
        CustodyError::new(
            ErrorKind::IdentityMismatch,
            "previous fixed root exit unproven",
        )
    };
    if !daemon_exit.matches(recovered, &options.owner, daemon_exit.owner_id()) {
        return Err(unavailable());
    }
    let previous_holder = recovered
        .recovered_r_holders
        .iter()
        .filter(|holder| holder.obligation_id == obligation_id)
        .max_by_key(|holder| holder.adoption_sequence)
        .cloned();
    match (&previous_holder, &previous_holder_exit) {
        (None, None) => {}
        (Some(holder), Some(exit))
            if holder.current_owner_id == exit.owner_id()
                && exit.matches(recovered, &options.owner, holder.current_owner_id) => {}
        _ => return Err(unavailable()),
    }
    let _held_installation_lock = &options.daemon_lock;
    let record = recovered
        .obligations
        .iter()
        .find(|record| {
            record.obligation_id == obligation_id && record.owner_id == daemon_exit.owner_id()
        })
        .ok_or_else(unavailable)?;
    if !matches!(
        record.phase,
        Phase::AcquisitionPossible | Phase::Sealed | Phase::UnknownHeld
    ) {
        return Err(unavailable());
    }
    let ObligationInventory::TypedV2 { manifest, members } = &record.inventory else {
        return Err(unavailable());
    };
    if manifest.graph_id.uuid() != record.graph_id
        || manifest.owner_id.uuid() != record.owner_id
        || manifest.members.len() != members.len()
        || !(5..=6).contains(&manifest.members.len())
    {
        return Err(unavailable());
    }
    let mut roots = manifest.members.iter().filter_map(|member| match member {
        v2::MemberPlan::LegacyV1 { planned }
            if planned.kind == MemberKind::Root && planned.adapter == Adapter::ProcessRoot =>
        {
            Some(planned)
        }
        _ => None,
    });
    let root = roots.next().ok_or_else(unavailable)?;
    if roots.next().is_some()
        || root.maximum_cardinality != 1
        || root.domain != "local_process_observation_v1"
        || root.owned_by.is_some()
        || !root.depends_on.is_empty()
    {
        return Err(unavailable());
    }
    let RecoveryMethod::TrustedFixedSoftwareSameBootV1 {
        pid_namespace,
        executable_binding,
    } = &root.recovery
    else {
        return Err(unavailable());
    };
    if !fixed_executable_binding(executable_binding)
        || !fixed_root_namespace_matches(pid_namespace, &daemon_exit.old_owner, &options.owner)
    {
        return Err(unavailable());
    }
    let root_record = members
        .iter()
        .find(|member| member.envelope.planned.member_id() == root.member_id)
        .ok_or_else(unavailable)?;
    let Some(v2::Observed::LegacyV1 { identity }) = &root_record.envelope.observed else {
        return Err(unavailable());
    };
    let process = identity.process.as_ref().ok_or_else(unavailable)?;
    let creation_marker = process.creation_marker.ok_or_else(unavailable)?;
    if root_record.phase != "acquired"
        || identity.member_id != root.member_id
        || identity.adapter != Adapter::ProcessRoot
        || identity.domain != root.domain
        || process.domain != root.domain
        || process.pid == 0
        || creation_marker == 0
        || process.pid == daemon_exit.old_owner.daemon_process.pid
        || identity.object_identity != format!("{}:{}", process.pid, creation_marker)
    {
        return Err(unavailable());
    }
    let mut task = None;
    let mut task_slot = None;
    let mut scratch = None;
    let mut scratch_slot = None;
    let mut media_sources = 0_usize;
    let mut executable_sources = 0_usize;
    let mut root_copy_acquisition = None;
    for member in &manifest.members {
        match member {
            v2::MemberPlan::LegacyV1 { planned } if planned.member_id == root.member_id => {}
            v2::MemberPlan::VodOutputTaskV1 {
                slot,
                task: planned,
            } if task.is_none() => {
                if !fixed_root_task_relation(root, slot, planned, manifest.graph_id) {
                    return Err(unavailable());
                }
                task = Some(planned);
                task_slot = Some(slot);
            }
            v2::MemberPlan::ScratchAssetsV3 { slot, plan } if scratch.is_none() => {
                if slot.owned_by.is_some()
                    || !slot.depends_on.is_empty()
                    || plan.filesystem_domain.daemon_owner_id.uuid() != record.owner_id
                    || plan.filesystem_domain.os_boot != daemon_exit.old_owner.os_boot
                    || !matches!(
                        &plan.filesystem_domain.mount_namespace,
                        v2::NativeValue::Known { .. }
                    )
                    || !matches!(
                        &plan.outputs.naming,
                        v2::scratch::OutputNamingV1::FiniteVodMetadataV2 { .. }
                    )
                    || plan.bootstrap().authority.installation_id.uuid()
                        != recovered.installation_id
                {
                    return Err(unavailable());
                }
                scratch = Some(plan);
                scratch_slot = Some(slot);
            }
            v2::MemberPlan::SourceV1 { slot, source } => {
                let v2::SourceRecovery::LocalSourceHandlesV1 {
                    owner_id,
                    graph_id,
                    filesystem_domain,
                } = &source.recovery;
                if slot.owned_by.is_some()
                    || !slot.depends_on.is_empty()
                    || owner_id.uuid() != record.owner_id
                    || *graph_id != manifest.graph_id
                    || *filesystem_domain != source.filesystem_domain
                    || source.filesystem_domain.daemon_owner_id.uuid() != record.owner_id
                    || source.filesystem_domain.os_boot != daemon_exit.old_owner.os_boot
                    || source.planned_copies.len() != 2
                {
                    return Err(unavailable());
                }
                let [parent, child] = source.planned_copies.as_slice() else {
                    return Err(unavailable());
                };
                if !matches!(&parent.owner, v2::CopyOwner::DaemonSourceCell { owner_id, graph_id, member_id }
                    if owner_id.uuid() == record.owner_id && *graph_id == manifest.graph_id && *member_id == source.member_id)
                    || parent.parent_copy_id.is_some()
                    || !matches!(&child.owner, v2::CopyOwner::OwnedRoot { graph_id, root_member_id }
                        if *graph_id == manifest.graph_id && root_member_id.uuid() == root.member_id)
                    || child.parent_copy_id != Some(parent.copy_id)
                    || root_copy_acquisition.is_some_and(|id| id != child.acquisition_operation_id)
                {
                    return Err(unavailable());
                }
                root_copy_acquisition = Some(child.acquisition_operation_id);
                if source.source_association.recipe_source_binding == *executable_binding
                    && source.source_association.media_file_id == *executable_binding
                {
                    if slot.protected_paths.len() != 1 || slot.protected_paths[0] != root.locator {
                        return Err(unavailable());
                    }
                    executable_sources += 1;
                } else {
                    if task.is_some_and(|task: &v2::VodOutputTask| {
                        source.source_association != task.source_association
                    }) {
                        return Err(unavailable());
                    }
                    media_sources += 1;
                }
            }
            _ => return Err(unavailable()),
        }
    }
    let task = task.ok_or_else(unavailable)?;
    let task_slot = task_slot.ok_or_else(unavailable)?;
    let scratch = scratch.ok_or_else(unavailable)?;
    let scratch_slot = scratch_slot.ok_or_else(unavailable)?;
    if executable_sources != 1
        || !(1..=2).contains(&media_sources)
        || !task_slot.depends_on.contains(&scratch_slot.member_id)
        || !manifest
            .members
            .iter()
            .filter_map(|member| match member {
                v2::MemberPlan::SourceV1 { source, .. } => Some(source),
                _ => None,
            })
            .all(|source| {
                source.filesystem_domain == scratch.filesystem_domain
                    && (source.source_association.recipe_source_binding == *executable_binding
                        || source.source_association == task.source_association)
            })
        || task.asset_id != scratch.asset_id
        || task.generation_epoch != scratch.generation_epoch
        || task.source_association.recipe_source_binding != scratch.rendition_id
        || root_copy_acquisition.is_none()
        || !old_process_exited(process.pid, creation_marker)
    {
        return Err(unavailable());
    }
    Ok(PositivePreviousRootExit {
        journal_id: recovered.journal_id,
        installation_id: recovered.installation_id,
        obligation_id,
        graph_id: record.graph_id,
        old_owner: daemon_exit.old_owner.clone(),
        current_owner: options.owner.clone(),
        root_member_id: root.member_id,
        pid: process.pid,
        creation_marker,
        pid_namespace: pid_namespace.clone(),
        executable_binding: executable_binding.clone(),
        previous_holder,
        previous_holder_exit,
    })
}

/// Exact producer-R replay selected under a separately proven old daemon and
/// fixed-root exit. Native import must still re-open and retain every required
/// directory under this R's full restored charge before adoption or cleanup.
pub(crate) struct ActiveFiniteRImportClaim {
    obligation_id: Uuid,
    graph_id: Uuid,
    original_owner_id: Uuid,
    old_owner_id: Uuid,
    journal_id: Uuid,
    installation_id: Uuid,
    resources: crate::adaptive_resources::ResourceVector,
    sequence: i64,
    manifest: v2::Manifest,
    scratch_member_id: v2::Id,
    plan: v2::scratch::bootstrap::ScratchPlanV3,
    state_head: v2::scratch::AssetEventRefV1,
    fold: Mutex<Option<BootstrapFold>>,
}

impl ActiveFiniteRImportClaim {
    pub(crate) fn obligation_id(&self) -> Uuid {
        self.obligation_id
    }
    pub(crate) fn graph_id(&self) -> Uuid {
        self.graph_id
    }
    pub(crate) fn old_owner_id(&self) -> Uuid {
        self.old_owner_id
    }
    pub(crate) fn original_owner_id(&self) -> Uuid {
        self.original_owner_id
    }
    pub(crate) fn journal_id(&self) -> Uuid {
        self.journal_id
    }
    pub(crate) fn installation_id(&self) -> Uuid {
        self.installation_id
    }
    pub(crate) fn resources(&self) -> crate::adaptive_resources::ResourceVector {
        self.resources
    }
    pub(crate) fn sequence(&self) -> i64 {
        self.sequence
    }
    pub(crate) fn manifest(&self) -> &v2::Manifest {
        &self.manifest
    }
    pub(crate) fn scratch_member_id(&self) -> v2::Id {
        self.scratch_member_id
    }
    pub(crate) fn plan(&self) -> &v2::scratch::bootstrap::ScratchPlanV3 {
        &self.plan
    }
    pub(crate) fn state_head(&self) -> &v2::scratch::AssetEventRefV1 {
        &self.state_head
    }
    pub(crate) fn with_fold<T>(
        &self,
        f: impl FnOnce(&BootstrapFold) -> T,
    ) -> Result<T, CustodyError> {
        let guard = self.fold.lock().map_err(|_| codec::invalid())?;
        Ok(f(guard.as_ref().ok_or_else(codec::invalid)?))
    }
    pub(crate) fn take_fold(&self) -> Result<BootstrapFold, CustodyError> {
        let mut guard = self.fold.lock().map_err(|_| codec::invalid())?;
        guard.take().ok_or_else(codec::invalid)
    }
    pub(crate) fn fold_consumed(&self) -> bool {
        self.fold.lock().is_ok_and(|guard| guard.is_none())
    }
}

pub(crate) fn select_active_finite_r(
    connection: &Connection,
    recovered: &RecoverySnapshot,
    exit: &PositivePreviousRootExit,
    current: &OwnerIdentity,
) -> Result<ActiveFiniteRImportClaim, CustodyError> {
    use v2::scratch::{bootstrap::BootstrapPhase, ScratchLifecycleV1};
    let unavailable = || {
        CustodyError::new(
            ErrorKind::InvalidCommand,
            "active fixed R import unavailable",
        )
    };
    if !exit.matches(
        recovered,
        current,
        exit.obligation_id,
        exit.graph_id,
        exit.root_member_id,
    ) {
        return Err(unavailable());
    }
    let record = recovered
        .obligations
        .iter()
        .find(|record| {
            record.obligation_id == exit.obligation_id && record.graph_id == exit.graph_id
        })
        .ok_or_else(unavailable)?;
    if !matches!(
        record.phase,
        Phase::AcquisitionPossible | Phase::Sealed | Phase::UnknownHeld
    ) || record.sequence < 1
        || record.owner_id != exit.old_owner.owner_id
    {
        return Err(unavailable());
    }
    let ObligationInventory::TypedV2 { manifest, members } = &record.inventory else {
        return Err(unavailable());
    };
    let mut scratch = manifest.members.iter().filter_map(|member| match member {
        v2::MemberPlan::ScratchAssetsV3 { slot, plan } => Some((slot, plan)),
        _ => None,
    });
    let (slot, plan) = scratch.next().ok_or_else(unavailable)?;
    if scratch.next().is_some() {
        return Err(unavailable());
    }
    let member = members
        .iter()
        .find(|member| member.envelope.planned.member_id() == slot.member_id.uuid())
        .ok_or_else(unavailable)?;
    let Some(v2::Observed::ScratchAssetsV3 { state }) = &member.envelope.observed else {
        return Err(unavailable());
    };
    let previous_holder = recovered
        .recovered_r_holders
        .iter()
        .filter(|holder| holder.obligation_id == record.obligation_id)
        .max_by_key(|holder| holder.adoption_sequence);
    if previous_holder.is_some_and(|holder| {
        holder.original_r_sequence != record.sequence
            || state.asset.head.as_ref() != Some(&holder.head)
            || holder.root_member_id != exit.root_member_id
            || holder.root_pid != exit.pid
            || holder.root_creation_marker != exit.creation_marker
            || holder.current_owner_id != exit.effective_previous_owner_id()
    }) {
        return Err(unavailable());
    }
    if member.phase != "acquired"
        || state.asset.lifecycle != ScratchLifecycleV1::Open
        || state.bootstrap.phase != BootstrapPhase::SinkReady
        || state.bootstrap.parent.is_none()
        || state.bootstrap.current.is_some()
        || state.bootstrap.possible_child.is_some()
        || state.asset.directory.is_none()
        || state.asset.totals.pending_jobs != 0
        || state.asset.totals.readers != 0
        || state.asset.head.is_none()
        || state.asset.revision == 0
    {
        return Err(unavailable());
    }
    let fold = super::transaction::scratch::bootstrap::restore(
        connection,
        record.obligation_id,
        record.graph_id,
        slot.member_id,
        plan,
        Some(state),
    )?;
    if fold.state() != *state
        || fold.files().len() > codec::ACTIVE_LIMIT
        || fold.files().iter().any(|file| {
            file.temporary || file.published.is_none() || !file.held_copy_ids.is_empty()
        })
    {
        return Err(unavailable());
    }
    Ok(ActiveFiniteRImportClaim {
        obligation_id: record.obligation_id,
        graph_id: record.graph_id,
        original_owner_id: record.owner_id,
        old_owner_id: exit.effective_previous_owner_id(),
        journal_id: recovered.journal_id,
        installation_id: recovered.installation_id,
        resources: record.resources,
        sequence: record.sequence,
        manifest: manifest.clone(),
        scratch_member_id: slot.member_id,
        plan: plan.clone(),
        state_head: state.asset.head.clone().ok_or_else(unavailable)?,
        fold: Mutex::new(Some(fold)),
    })
}

/// Only this private selector can construct a retained import claim. It
/// replays the original producer fold, verifies the frozen reference and
/// target's later head, and accepts only a dormant read-only S with no old
/// reader lease to reconstruct. The full S charge remains in UnknownHeld.
pub(crate) struct RetainedSImportClaim {
    obligation_id: Uuid,
    original_owner_id: Uuid,
    old_owner_id: Uuid,
    journal_id: Uuid,
    installation_id: Uuid,
    resources: crate::adaptive_resources::ResourceVector,
    current_head: v2::scratch::AssetEventRefV1,
    inventory_sha256: String,
    plan: v2::scratch::bootstrap::ScratchPlanV3,
    full: FrozenScratchV3,
    target: Mutex<Option<BootstrapFold>>,
    manifest: v2::Manifest,
    member_id: v2::Id,
    graph_id: Uuid,
    sequence: i64,
    media_sources: Vec<v2::SourceAssociation>,
    media_source_member_count: usize,
}

impl RetainedSImportClaim {
    pub(crate) fn obligation_id(&self) -> Uuid {
        self.obligation_id
    }
    pub(crate) fn old_owner_id(&self) -> Uuid {
        self.old_owner_id
    }
    pub(crate) fn original_owner_id(&self) -> Uuid {
        self.original_owner_id
    }
    pub(crate) fn current_head(&self) -> &v2::scratch::AssetEventRefV1 {
        &self.current_head
    }
    pub(crate) fn inventory_sha256(&self) -> &str {
        &self.inventory_sha256
    }
    pub(crate) fn journal_id(&self) -> Uuid {
        self.journal_id
    }
    pub(crate) fn installation_id(&self) -> Uuid {
        self.installation_id
    }
    pub(crate) fn resources(&self) -> crate::adaptive_resources::ResourceVector {
        self.resources
    }
    pub(crate) fn plan(&self) -> &v2::scratch::bootstrap::ScratchPlanV3 {
        &self.plan
    }
    pub(crate) fn full(&self) -> &FrozenScratchV3 {
        &self.full
    }
    pub(crate) fn graph_id(&self) -> Uuid {
        self.graph_id
    }
    pub(crate) fn sequence(&self) -> i64 {
        self.sequence
    }
    pub(crate) fn member_id(&self) -> v2::Id {
        self.member_id
    }
    pub(crate) fn manifest(&self) -> &v2::Manifest {
        &self.manifest
    }
    pub(crate) fn media_sources(&self) -> &[v2::SourceAssociation] {
        &self.media_sources
    }
    pub(crate) fn media_source_member_count(&self) -> usize {
        self.media_source_member_count
    }
    /// Move the replayed current target fold into the native adapter only
    /// after durable adoption. The source frozen fold is inventory evidence.
    pub(crate) fn take_target_for_runtime(&self) -> Result<BootstrapFold, CustodyError> {
        let mut target = self.target.lock().map_err(|_| {
            CustodyError::new(ErrorKind::Conflict, "recovered S target fold unavailable")
        })?;
        let current = target
            .as_ref()
            .ok_or_else(|| {
                CustodyError::new(ErrorKind::Conflict, "recovered S target fold already taken")
            })?
            .state();
        if current.asset.lifecycle != v2::scratch::ScratchLifecycleV1::RetainedReadOnly
            || current.bootstrap.phase != v2::scratch::bootstrap::BootstrapPhase::SinkReady
            || current.asset.totals.readers != 0
            || current.asset.totals.pending_jobs != 0
            || current.bootstrap.possible_child.is_some()
            || current.bootstrap.parent.is_some()
            || current.asset.directory.is_some()
        {
            return Err(CustodyError::new(
                ErrorKind::Conflict,
                "recovered S target is not dormant",
            ));
        }
        target.take().ok_or_else(|| {
            CustodyError::new(ErrorKind::Conflict, "recovered S target fold already taken")
        })
    }
}

pub(crate) fn select_dormant_retained_s(
    connection: &Connection,
    recovered: &RecoverySnapshot,
    exit: &PositivePreviousOwnerExit,
    obligation_id: Uuid,
) -> Result<RetainedSImportClaim, CustodyError> {
    use v2::scratch::{bootstrap::BootstrapPhase, ScratchLifecycleV1};
    let unavailable =
        || CustodyError::new(ErrorKind::InvalidCommand, "retained S recovery unavailable");
    if !exit.matches(recovered, &exit.current_owner, exit.owner_id()) {
        return Err(unavailable());
    }
    let record = recovered
        .obligations
        .iter()
        .find(|record| record.obligation_id == obligation_id)
        .ok_or_else(unavailable)?;
    let effective_owner = recovered
        .recovered_s_holders
        .iter()
        .filter(|holder| holder.obligation_id == obligation_id)
        .max_by_key(|holder| holder.adoption_sequence)
        .map_or(record.owner_id, |holder| holder.current_owner_id);
    if effective_owner != exit.owner_id() {
        return Err(unavailable());
    }
    if !matches!(record.phase, Phase::Sealed | Phase::UnknownHeld) {
        return Err(unavailable());
    }
    let asset = recovered
        .retained_assets
        .iter()
        .find(|asset| asset.obligation_id == obligation_id)
        .ok_or_else(unavailable)?;
    let v2::RetainedAssetsDocument::V2(custody) = &asset.custody else {
        return Err(unavailable());
    };
    let ObligationInventory::TypedV2 { manifest, members } = &record.inventory else {
        return Err(unavailable());
    };
    let [v2::MemberPlan::ScratchAssetsV3 { slot, plan }] = manifest.members.as_slice() else {
        return Err(unavailable());
    };
    let [member] = members.as_slice() else {
        return Err(unavailable());
    };
    let Some(v2::Observed::ScratchAssetsV3 { state }) = &member.envelope.observed else {
        return Err(unavailable());
    };
    if member.phase != "acquired"
        || member.envelope.planned != manifest.members[0]
        || custody.target_obligation_id.uuid() != obligation_id
        || custody.target_owner_id.uuid() != record.owner_id
        || custody.transfer.target_member_id != slot.member_id
        || state.asset.lifecycle != ScratchLifecycleV1::RetainedReadOnly
        || state.bootstrap.phase != BootstrapPhase::SinkReady
        || state.bootstrap.parent.is_none()
        || state.asset.directory.is_none()
        || state.asset.totals.readers != 0
    {
        return Err(unavailable());
    }
    let v2::TransferIdentity::ScratchAssetsV3 { snapshot, .. } = &custody.transfer.identity else {
        return Err(unavailable());
    };
    // The cache key omits the original media object's version. Read that
    // identity from the immutable producer manifest, before replaying its
    // source fold; the current scanner cannot manufacture an old association.
    let producer_manifest_json: String = connection.query_row(
        "SELECT manifest_json FROM obligations WHERE obligation_id=?1 AND graph_id=?2",
        rusqlite::params![
            custody.producer_obligation_id.uuid().as_bytes(),
            custody.producer_graph_id.uuid().as_bytes(),
        ],
        |r| r.get(0),
    )?;
    let v2::ManifestDocument::V2(producer_manifest) = v2::decode_manifest(&producer_manifest_json)?
    else {
        return Err(unavailable());
    };
    if producer_manifest.graph_id != custody.producer_graph_id {
        return Err(unavailable());
    }
    let mut task_associations =
        producer_manifest
            .members
            .iter()
            .filter_map(|member| match member {
                v2::MemberPlan::VodOutputTaskV1 { task, .. } if task.asset_id == plan.asset_id => {
                    Some(&task.source_association)
                }
                _ => None,
            });
    let task_association = task_associations.next().ok_or_else(unavailable)?;
    if task_associations.next().is_some()
        || task_association.recipe_source_binding != plan.rendition_id
    {
        return Err(unavailable());
    }
    let mut media_sources = Vec::new();
    let mut media_source_member_count = 0_usize;
    for member in &producer_manifest.members {
        let v2::MemberPlan::SourceV1 { source, .. } = member else {
            continue;
        };
        if source.source_association.recipe_source_binding != plan.rendition_id {
            continue;
        }
        if &source.source_association != task_association {
            return Err(unavailable());
        }
        media_source_member_count = media_source_member_count
            .checked_add(1)
            .ok_or_else(unavailable)?;
        if !media_sources.contains(&source.source_association) {
            media_sources.try_reserve(1).map_err(|_| unavailable())?;
            media_sources.push(source.source_association.clone());
        }
    }
    if media_sources.len() != 1 || media_source_member_count == 0 {
        return Err(unavailable());
    }
    let current_head = state.asset.head.clone().ok_or_else(unavailable)?;
    if recovered
        .recovered_s_holders
        .iter()
        .filter(|holder| holder.obligation_id == obligation_id)
        .max_by_key(|holder| holder.adoption_sequence)
        .is_some_and(|holder| {
            holder.frozen_digest != snapshot.inventory_sha256
                || holder.original_s_sequence > record.sequence
        })
    {
        return Err(unavailable());
    }
    let original = super::transaction::scratch::bootstrap::restore(
        connection,
        custody.producer_obligation_id.uuid(),
        custody.producer_graph_id.uuid(),
        custody.transfer.source_member_id,
        plan,
        Some(&snapshot.state),
    )?;
    let full = original.frozen_snapshot(plan)?;
    v2::validate_retained_resources_v2(manifest, custody, &full, record.resources)?;
    let target = super::transaction::scratch::bootstrap::restore(
        connection,
        obligation_id,
        record.graph_id,
        slot.member_id,
        plan,
        Some(state),
    )?;
    if target.state() != *state
        || target.files() != full.files.as_slice()
        || !full.readers.is_empty()
        || target.files().len() > codec::ACTIVE_LIMIT
    {
        return Err(unavailable());
    }
    Ok(RetainedSImportClaim {
        obligation_id,
        original_owner_id: record.owner_id,
        old_owner_id: exit.owner_id(),
        journal_id: recovered.journal_id,
        installation_id: recovered.installation_id,
        resources: record.resources,
        current_head,
        inventory_sha256: snapshot.inventory_sha256.clone(),
        plan: plan.clone(),
        full,
        target: Mutex::new(Some(target)),
        manifest: manifest.clone(),
        member_id: slot.member_id,
        graph_id: record.graph_id,
        sequence: record.sequence,
        media_sources,
        media_source_member_count,
    })
}

#[cfg(test)]
mod namespace_tests {
    use super::*;
    use crate::adaptive_resources::ResourceVector;

    #[test]
    fn prior_r_exit_proof_cannot_follow_a_drifted_latest_holder() {
        let obligation_id = Uuid::new_v4();
        let first = RecoveredRHolder {
            obligation_id,
            adoption_sequence: 4,
            operation_id: Uuid::new_v4(),
            previous_owner_id: Uuid::new_v4(),
            current_owner_id: Uuid::new_v4(),
            original_r_sequence: 2,
            head: v2::scratch::AssetEventRefV1 {
                operation_id: v2::Id::new(Uuid::new_v4()).unwrap(),
                revision: 1,
            },
            root_member_id: Uuid::new_v4(),
            root_pid: 123,
            root_creation_marker: 456,
        };
        assert!(exact_latest_r_holder(
            &[first.clone()],
            obligation_id,
            &first
        ));
        let newer = RecoveredRHolder {
            adoption_sequence: 5,
            operation_id: Uuid::new_v4(),
            previous_owner_id: first.current_owner_id,
            current_owner_id: Uuid::new_v4(),
            ..first.clone()
        };
        assert!(!exact_latest_r_holder(
            &[first.clone(), newer.clone()],
            obligation_id,
            &first
        ));
        assert!(exact_latest_r_holder(
            &[first, newer.clone()],
            obligation_id,
            &newer
        ));
    }

    fn owner(owner_id: Uuid, pid: u32, boot: Uuid, namespace: Option<String>) -> OwnerIdentity {
        OwnerIdentity {
            owner_id,
            node_id: "fixture-node".into(),
            cluster_id: "fixture-cluster".into(),
            os_boot: BootIdentity::Known {
                value: boot,
                provider: "linux_kernel_boot_id".into(),
                version: 1,
            },
            daemon_process: ProcessIdentity {
                version: 1,
                pid,
                creation_marker: Some(1),
                domain: "local_process_observation_v1".into(),
            },
            platform_domain: PlatformDomain {
                version: 1,
                platform: "linux".into(),
                host_identity: None,
                namespace: namespace.clone(),
                continuity_provider: namespace.map(|_| PID_NAMESPACE_PROVIDER.into()),
            },
        }
    }

    fn options(owner: OwnerIdentity) -> OpenOptions {
        OpenOptions {
            data_dir: std::path::PathBuf::from("/fixture"),
            excluded_roots: Vec::new(),
            installation: InstallationContext::Existing,
            owner,
            daemon_lock: DaemonLockGuard::from_held_lock(tempfile::tempfile().expect("test lock")),
            queue_capacity: 1,
        }
    }

    fn snapshot(old: OwnerIdentity) -> RecoverySnapshot {
        RecoverySnapshot {
            journal_id: Uuid::new_v4(),
            installation_id: Uuid::new_v4(),
            obligations: Vec::new(),
            families: Vec::new(),
            family_authority: FamilyAuthority::from_validated_rows(
                0,
                family_edges_digest(&[]),
                &[],
            ),
            retained_assets: Vec::new(),
            total: ResourceVector::default(),
            trial_groups: Vec::new(),
            protected_paths: Vec::new(),
            owners: vec![old],
            recovered_s_holders: Vec::new(),
            recovered_r_holders: Vec::new(),
        }
    }

    #[test]
    fn unknown_or_mismatched_pid_namespace_refuses_old_daemon_exit() {
        let token = current_pid_namespace_token().expect("Linux proc namespace");
        let boot = Uuid::new_v4();
        let old_id = Uuid::new_v4();
        let old = owner(old_id, u32::MAX - 1, boot, None);
        let current = owner(
            Uuid::new_v4(),
            std::process::id(),
            boot,
            Some(token.clone()),
        );
        assert!(
            prove_previous_daemon_exit(&options(current.clone()), &snapshot(old), old_id).is_err()
        );
        let old = owner(old_id, u32::MAX - 1, boot, Some("1:1".into()));
        assert!(
            prove_previous_daemon_exit(&options(current.clone()), &snapshot(old), old_id).is_err()
        );
        let current_unknown = owner(current.owner_id, current.daemon_process.pid, boot, None);
        let old = owner(old_id, u32::MAX - 1, boot, Some(token));
        assert!(
            prove_previous_daemon_exit(&options(current_unknown), &snapshot(old), old_id).is_err()
        );
    }

    #[test]
    fn root_namespace_check_refuses_unknown_and_other_pid_namespaces() {
        let token = current_pid_namespace_token().expect("Linux proc namespace");
        let boot = Uuid::new_v4();
        let current = owner(
            Uuid::new_v4(),
            std::process::id(),
            boot,
            Some(token.clone()),
        );
        let old = owner(Uuid::new_v4(), u32::MAX - 1, boot, Some(token.clone()));
        assert!(fixed_root_namespace_matches(&token, &old, &current));
        let old_unknown = owner(old.owner_id, old.daemon_process.pid, boot, None);
        assert!(!fixed_root_namespace_matches(
            &token,
            &old_unknown,
            &current
        ));
        let old_other = owner(
            old.owner_id,
            old.daemon_process.pid,
            boot,
            Some("1:1".into()),
        );
        assert!(!fixed_root_namespace_matches(&token, &old_other, &current));
        assert!(!fixed_root_namespace_matches("1:1", &old, &current));
    }

    #[test]
    fn trusted_root_plan_requires_canonical_namespace_and_cached_executable_digest() {
        let token = current_pid_namespace_token().expect("Linux proc namespace");
        let binding = format!("software-avc-executable-sha256:{}", "a".repeat(64));
        let mut root = PlannedMember {
            member_id: Uuid::new_v4(),
            kind: MemberKind::Root,
            adapter: Adapter::ProcessRoot,
            adapter_version: 1,
            domain: "local_process_observation_v1".into(),
            locator: "/fixture/fixed-avc".into(),
            recovery: RecoveryMethod::TrustedFixedSoftwareSameBootV1 {
                pid_namespace: token,
                executable_binding: binding,
            },
            owned_by: None,
            depends_on: Vec::new(),
            maximum_cardinality: 1,
            protected_paths: Vec::new(),
        };
        let manifest = |planned: PlannedMember| Manifest {
            version: 1,
            graph_id: Uuid::new_v4(),
            owner_id: Uuid::new_v4(),
            attempt_id: None,
            members: vec![planned],
        };
        codec::validate_manifest(&manifest(root.clone())).expect("forward root shape");
        let typed = v2::tests::fixture();
        let v2::MemberPlan::VodOutputTaskV1 { slot, task } = &typed.members[3] else {
            panic!("typed task fixture");
        };
        let mut task = task.clone();
        let mut slot = slot.clone();
        let root_id = v2::Id::new(root.member_id).expect("root ID");
        let scratch_id = v2::Id::new(Uuid::new_v4()).expect("scratch ID");
        task.root_member_id = root_id;
        slot.owned_by = Some(root_id);
        slot.depends_on = vec![root_id, scratch_id];
        assert!(fixed_root_task_relation(
            &root,
            &slot,
            &task,
            typed.graph_id
        ));
        task.root_member_id = v2::Id::new(Uuid::new_v4()).expect("wrong root");
        assert!(!fixed_root_task_relation(
            &root,
            &slot,
            &task,
            typed.graph_id
        ));
        task.root_member_id = root_id;
        slot.owned_by = None;
        assert!(!fixed_root_task_relation(
            &root,
            &slot,
            &task,
            typed.graph_id
        ));
        if let RecoveryMethod::TrustedFixedSoftwareSameBootV1 {
            executable_binding, ..
        } = &mut root.recovery
        {
            *executable_binding = "software-avc-executable-sha256:uncached".into();
        }
        assert!(codec::validate_manifest(&manifest(root.clone())).is_err());
        root.kind = MemberKind::Task;
        assert!(codec::validate_manifest(&manifest(root)).is_err());
    }
}
