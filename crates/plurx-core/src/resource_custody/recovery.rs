//! Narrow, cleanup-only recovery selection. These values are minted from the
//! validated journal under its original installation lock; none is a receipt.
use super::*;
use crate::resource_custody::codec_v2::scratch::bootstrap::BootstrapFold;
use crate::resource_custody::codec_v2::{self as v2, scratch::bootstrap::FrozenScratchV3};
use rusqlite::Connection;
use std::io::Read;
use std::sync::Mutex;

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
        || old.platform_domain.namespace.is_some()
        || current.platform_domain.namespace.is_some()
        || old.daemon_process.version != 1
        || old.daemon_process.domain != "local_process_observation_v1"
        || old.daemon_process.pid == 0
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
    let path = format!("/proc/{}/stat", old.daemon_process.pid);
    let mut stat = String::new();
    match std::fs::File::open(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(unavailable()),
        Ok(file) => {
            file.take(16 * 1024)
                .read_to_string(&mut stat)
                .map_err(|_| unavailable())?;
            let (prefix, rest) = stat.rsplit_once(')').ok_or_else(unavailable)?;
            let (reported_pid, _) = prefix.split_once('(').ok_or_else(unavailable)?;
            let reported_pid = reported_pid
                .trim()
                .parse::<u32>()
                .map_err(|_| unavailable())?;
            let marker = rest
                .split_whitespace()
                .nth(19)
                .ok_or_else(unavailable)?
                .parse::<u64>()
                .map_err(|_| unavailable())?;
            if reported_pid != old.daemon_process.pid
                || Some(marker) == old.daemon_process.creation_marker
            {
                return Err(unavailable());
            }
        }
    }
    Ok(PositivePreviousOwnerExit {
        journal_id: recovered.journal_id,
        installation_id: recovered.installation_id,
        old_owner: old.clone(),
        current_owner: current.clone(),
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
