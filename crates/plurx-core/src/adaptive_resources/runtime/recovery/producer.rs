//! Narrow native import of an exited fixed software producer's existing R.
//! Reopened directories are actual counted owners, never restored from DTOs.
use super::*;
use crate::resource_custody::recovery::{ActiveFiniteRImportClaim, PositivePreviousRootExit};

pub(crate) struct ImportedActiveR {
    claim: ActiveFiniteRImportClaim,
    root_exit: PositivePreviousRootExit,
    directories: Mutex<Option<(File, File)>>,
}

/// Nonserde, move-only proof held by the runtime before it can submit a
/// current-holder adoption. Its Arc retains the exact imported parent/child
/// descriptors across cancellation and an unknown journal result.
#[must_use]
pub(crate) struct NativeActiveRImportProof {
    owner: Arc<ImportedActiveR>,
    current_owner_id: Uuid,
    binding: ReservationBinding,
}

/// Writer capability minted only from a known durable producer adoption.
/// Its binding names the same full restored R entry as the native owner map.
#[derive(Clone, Debug)]
pub(crate) struct RecoveredRAuthority {
    journal_id: Uuid,
    installation_id: Uuid,
    obligation_id: Uuid,
    graph_id: Uuid,
    original_owner_id: Uuid,
    current_owner_id: Uuid,
    binding: ReservationBinding,
    adoption_operation_id: Uuid,
    adoption_sequence: i64,
    state_head: v2::scratch::AssetEventRefV1,
    root_member_id: Uuid,
    root_pid: u32,
    root_creation_marker: u64,
}
impl RecoveredRAuthority {
    pub(crate) fn journal_id(&self) -> Uuid {
        self.journal_id
    }
    pub(crate) fn installation_id(&self) -> Uuid {
        self.installation_id
    }
    pub(crate) fn obligation_id(&self) -> Uuid {
        self.obligation_id
    }
    pub(crate) fn graph_id(&self) -> Uuid {
        self.graph_id
    }
    pub(crate) fn original_owner_id(&self) -> Uuid {
        self.original_owner_id
    }
    pub(crate) fn current_owner_id(&self) -> Uuid {
        self.current_owner_id
    }
    pub(crate) fn binding(&self) -> ReservationBinding {
        self.binding
    }
    pub(crate) fn adoption_operation_id(&self) -> Uuid {
        self.adoption_operation_id
    }
    pub(crate) fn adoption_sequence(&self) -> i64 {
        self.adoption_sequence
    }
    pub(crate) fn state_head(&self) -> &v2::scratch::AssetEventRefV1 {
        &self.state_head
    }
    pub(crate) fn root_member_id(&self) -> Uuid {
        self.root_member_id
    }
    pub(crate) fn root_pid(&self) -> u32 {
        self.root_pid
    }
    pub(crate) fn root_creation_marker(&self) -> u64 {
        self.root_creation_marker
    }
}

impl NativeActiveRImportProof {
    pub(crate) fn claim(&self) -> &ActiveFiniteRImportClaim {
        &self.owner.claim
    }
    pub(crate) fn root_exit(&self) -> &PositivePreviousRootExit {
        &self.owner.root_exit
    }
    pub(crate) fn journal_id(&self) -> Uuid {
        self.owner.claim.journal_id()
    }
    pub(crate) fn installation_id(&self) -> Uuid {
        self.owner.claim.installation_id()
    }
    pub(crate) fn obligation_id(&self) -> Uuid {
        self.owner.claim.obligation_id()
    }
    pub(crate) fn graph_id(&self) -> Uuid {
        self.owner.claim.graph_id()
    }
    pub(crate) fn previous_owner_id(&self) -> Uuid {
        self.owner.claim.old_owner_id()
    }
    pub(crate) fn current_owner_id(&self) -> Uuid {
        self.current_owner_id
    }
    pub(crate) fn binding(&self) -> ReservationBinding {
        self.binding
    }
    pub(crate) fn original_sequence(&self) -> i64 {
        self.owner.claim.sequence()
    }
    pub(crate) fn state_head(&self) -> &v2::scratch::AssetEventRefV1 {
        self.owner.claim.state_head()
    }
    pub(crate) fn adopted_authority(
        &self,
        ack: &crate::resource_custody::RecoveryAdoptionAck,
    ) -> Option<RecoveredRAuthority> {
        if ack.operation_id.is_nil()
            || ack.sequence <= self.original_sequence()
            || ack.obligation_id != self.obligation_id()
            || ack.previous_owner_id != self.previous_owner_id()
            || ack.current_owner_id != self.current_owner_id()
        {
            return None;
        }
        Some(RecoveredRAuthority {
            journal_id: self.journal_id(),
            installation_id: self.installation_id(),
            obligation_id: self.obligation_id(),
            graph_id: self.graph_id(),
            original_owner_id: self.previous_owner_id(),
            current_owner_id: self.current_owner_id(),
            binding: self.binding,
            adoption_operation_id: ack.operation_id,
            adoption_sequence: ack.sequence,
            state_head: self.state_head().clone(),
            root_member_id: self.root_exit().root_member_id(),
            root_pid: self.root_exit().pid(),
            root_creation_marker: self.root_exit().creation_marker(),
        })
    }
}

impl NodeResourceRuntime {
    /// Call synchronously while the restored R is UnknownHeld and before any
    /// journal adoption is queued. The caller must retain the returned proof
    /// in its runtime owner map before awaiting anything.
    pub(crate) fn import_active_finite_r(
        &self,
        claim: ActiveFiniteRImportClaim,
        root_exit: PositivePreviousRootExit,
    ) -> Result<NativeActiveRImportProof, RuntimeOpenError> {
        let id = claim.obligation_id();
        let mut retained = self.recovered_r.lock().map_err(|_| unavailable())?;
        if retained.contains_key(&id) {
            return Err(unavailable());
        }
        let binding = *self.recovered_bindings.get(&id).ok_or_else(unavailable)?;
        let state = self.ledger.inner.lock().map_err(|_| unavailable())?;
        let entry = state
            .entries
            .get(&binding.reservation)
            .ok_or_else(unavailable)?;
        if entry.binding != binding
            || entry.durable != Some(id)
            || entry.resources != claim.resources()
            || entry.resources.handles < 2
            || entry.state != ReservationState::UnknownHeld
            || claim.journal_id() != self.recovered.journal_id
            || claim.installation_id() != self.recovered.installation_id
            || !root_exit.matches(
                &self.recovered,
                &self.owner,
                id,
                claim.graph_id(),
                claim
                    .manifest()
                    .members
                    .iter()
                    .find_map(|member| match member {
                        v2::MemberPlan::LegacyV1 { planned }
                            if planned.kind == crate::resource_custody::MemberKind::Root =>
                        {
                            Some(planned.member_id)
                        }
                        _ => None,
                    })
                    .ok_or_else(unavailable)?,
            )
        {
            return Err(unavailable());
        }
        drop(state);
        let plan = claim.plan();
        let bootstrap = plan.bootstrap();
        if self.rendition_root.as_deref()
            != Some(Path::new(&bootstrap.authority.resolved_absolute_root))
            || bootstrap.authority.installation_id.uuid() != self.recovered.installation_id
            || plan.filesystem_domain.daemon_owner_id.uuid() != claim.old_owner_id()
            || plan.filesystem_domain.os_boot != self.owner.os_boot
            || !matches!(&plan.filesystem_domain.mount_namespace, v2::NativeValue::Known { value }
                if std::fs::metadata("/proc/self/ns/mnt").is_ok_and(|m| m.ino() == *value))
            || plan.limits.live_handles < 2
            || bootstrap.bounds.peak_directory_handles < 2
        {
            return Err(unavailable());
        }
        let mut parent = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open("/")
            .map_err(|_| unavailable())?;
        for name in &bootstrap.components {
            let next = open_dir(&parent, name)?;
            let fd = parent.into_raw_fd();
            if unsafe { libc::close(fd) } != 0 {
                return Err(unavailable());
            }
            parent = next;
        }
        let (parent_observed, child_observed) = claim.with_fold(|fold| {
            let state = fold.state();
            (
                state.bootstrap.parent.as_ref().map(|p| p.observed.clone()),
                state.asset.directory.clone(),
            )
        })?;
        let parent_observed = parent_observed.ok_or_else(unavailable)?;
        let child_observed = child_observed.ok_or_else(unavailable)?;
        if parent_observed.held_copy_ids.len() != 1
            || child_observed.held_copy_ids.len() != 1
            || parent_observed.held_copy_ids[0] == child_observed.held_copy_ids[0]
            || !native_matches(
                &parent,
                &parent_observed.native_object,
                &plan.filesystem_domain,
            )
        {
            return Err(unavailable());
        }
        let child = open_dir(&parent, &bootstrap.child_component)?;
        if !native_matches(
            &child,
            &child_observed.native_object,
            &plan.filesystem_domain,
        ) || mount_id(&parent) != mount_id(&child)
            || parent.metadata().map_err(|_| unavailable())?.dev()
                != child.metadata().map_err(|_| unavailable())?.dev()
        {
            return Err(unavailable());
        }
        let names = claim
            .with_fold(|fold| {
                let mut names = BTreeSet::new();
                for observed in fold.files() {
                    if observed.temporary || !observed.held_copy_ids.is_empty() {
                        return None;
                    }
                    let output = observed.published?;
                    if !plan.outputs.contains(&output) {
                        return None;
                    }
                    let name = filename(output);
                    if !names.insert(name.clone())
                        || !regular_entry_matches(&child, &name, observed, &plan.filesystem_domain)
                    {
                        return None;
                    }
                }
                Some(names)
            })?
            .ok_or_else(unavailable)?;
        exact_entries(&child, &names)?;
        let imported = Arc::new(ImportedActiveR {
            claim,
            root_exit,
            directories: Mutex::new(Some((parent, child))),
        });
        retained.insert(id, imported.clone());
        Ok(NativeActiveRImportProof {
            owner: imported,
            current_owner_id: self.owner.owner_id,
            binding,
        })
    }
}
