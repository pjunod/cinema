//! One checked bootstrap/output lineage; data-only, never native proof.
use super::*;
#[cfg(test)]
mod tests;

pub(crate) struct BootstrapFold {
    output: ScratchFold,
    bootstrap: BootstrapStateV1,
    obligation: Id,
    member: Id,
    pending: Option<(Id, BootstrapIoV1)>,
    possible_copy: Option<Id>,
    uncertain_copies: BTreeSet<Id>,
}
impl BootstrapFold {
    /// Requested heap capacity of the retained fold's inventory payload.
    /// This is separate from durable `AssetTotalsV1` audit state.
    pub(crate) fn resident_inventory_bytes(&self) -> Result<u64, CustodyError> {
        if self.pending.is_some()
            || self.output.has_pending()
            || self.bootstrap.possible_child.is_some()
            || self.possible_copy.is_some()
            || !self.uncertain_copies.is_empty()
        {
            return Err(codec::invalid());
        }
        let output_bytes = self.output.resident_inventory_bytes()?;
        let bootstrap_bytes = self.bootstrap.resident_heap_bytes()?;
        let output_payload_bytes = output_bytes
            .checked_sub(std::mem::size_of::<ScratchFold>() as u64)
            .ok_or_else(codec::invalid)?;
        u64::try_from(std::mem::size_of::<Self>())
            .ok()
            .and_then(|own| own.checked_add(output_payload_bytes))
            .and_then(|n| n.checked_add(bootstrap_bytes))
            .ok_or_else(codec::invalid)
    }
    pub(crate) fn empty(
        plan: &ScratchPlanV3,
        obligation: Id,
        member: Id,
    ) -> Result<Self, CustodyError> {
        validate_plan(plan)?;
        Ok(Self {
            output: ScratchFold::empty_output(plan.asset_id),
            bootstrap: BootstrapStateV1 {
                phase: BootstrapPhase::Unstarted,
                next_component: 0,
                current: None,
                parent: None,
                possible_child: None,
                retained_parent_synced: false,
            },
            obligation,
            member,
            pending: None,
            possible_copy: None,
            uncertain_copies: BTreeSet::new(),
        })
    }
    pub(crate) fn obligation_id(&self) -> Id {
        self.obligation
    }
    pub(crate) fn member_id(&self) -> Id {
        self.member
    }
    pub(crate) fn state(&self) -> ScratchStateV3 {
        ScratchStateV3 {
            version: 3,
            asset: self.output.state.clone(),
            bootstrap: self.bootstrap.clone(),
        }
    }
    pub(crate) fn files(&self) -> &[FileObservedV1] {
        &self.output.files
    }
    pub(crate) fn has_reader_for_file(&self, file_id: Id) -> bool {
        self.output.has_reader_for_file(file_id)
    }
    pub(crate) fn frozen_snapshot(
        &self,
        plan: &ScratchPlanV3,
    ) -> Result<FrozenScratchV3, CustodyError> {
        if self.output.state.lifecycle != ScratchLifecycleV1::Frozen
            || self.pending.is_some()
            || self.output.has_pending()
            || self.possible_copy.is_some()
            || self.bootstrap.possible_child.is_some()
            || !self.uncertain_copies.is_empty()
            || !matches!(
                self.bootstrap.phase,
                BootstrapPhase::SinkReady | BootstrapPhase::Closed
            )
        {
            return Err(codec::invalid());
        }
        let snapshot = FrozenScratchV3 {
            version: 1,
            obligation_id: self.obligation,
            member_id: self.member,
            state: self.state(),
            files: self.output.files.clone(),
            readers: self.output.readers().to_vec(),
        };
        Self::validate_frozen_snapshot(plan, &snapshot)?;
        Ok(snapshot)
    }
    /// Data-only rebinding after the journal writer has proved the exact R→S
    /// transfer. The original native copy IDs and event head remain unchanged.
    pub(crate) fn adopt_frozen(
        &self,
        plan: &ScratchPlanV3,
        reference: &FrozenScratchRefV1,
        target_obligation: Id,
        target_member: Id,
    ) -> Result<Self, CustodyError> {
        if target_obligation == self.obligation || target_member == self.member {
            return Err(codec::invalid());
        }
        let full = self.frozen_snapshot(plan)?;
        validate_frozen_snapshot_ref(plan, reference, &full)?;
        let mut adopted = full.state.asset;
        adopted.lifecycle = ScratchLifecycleV1::RetainedReadOnly;
        Ok(Self {
            output: ScratchFold::from_frozen_inventory(adopted, full.files, full.readers),
            bootstrap: full.state.bootstrap,
            obligation: target_obligation,
            member: target_member,
            pending: None,
            possible_copy: None,
            uncertain_copies: BTreeSet::new(),
        })
    }
    pub(super) fn validate_frozen_snapshot(
        plan: &ScratchPlanV3,
        snapshot: &FrozenScratchV3,
    ) -> Result<(), CustodyError> {
        validate_plan(plan)?;
        if snapshot.version != 1
            || snapshot.state.version != 3
            || snapshot.state.asset.asset_id != plan.asset_id
            || snapshot.state.asset.lifecycle != ScratchLifecycleV1::Frozen
            || snapshot.state.bootstrap.possible_child.is_some()
            || snapshot.state.bootstrap.current.is_some()
            || usize::from(snapshot.state.bootstrap.next_component)
                != plan.bootstrap().components.len()
            || snapshot.state.asset.directory.is_none()
            || snapshot.state.asset.totals.pending_jobs != 0
            || !matches!(
                snapshot.state.bootstrap.phase,
                BootstrapPhase::SinkReady | BootstrapPhase::Closed
            )
        {
            return Err(codec::invalid());
        }
        match snapshot.state.bootstrap.phase {
            BootstrapPhase::SinkReady => {
                let parent = snapshot
                    .state
                    .bootstrap
                    .parent
                    .as_ref()
                    .ok_or_else(codec::invalid)?;
                if parent.kind != BootstrapDirectoryKind::ConfiguredParentV1
                    || parent.observed.held_copy_ids.is_empty()
                {
                    return Err(codec::invalid());
                }
            }
            BootstrapPhase::Closed if snapshot.state.bootstrap.parent.is_none() => {}
            _ => return Err(codec::invalid()),
        }
        let original_state = snapshot.state.clone();
        let mut fold = Self {
            output: ScratchFold::from_frozen_inventory(
                original_state.asset.clone(),
                snapshot.files.clone(),
                snapshot.readers.clone(),
            ),
            bootstrap: original_state.bootstrap.clone(),
            obligation: snapshot.obligation_id,
            member: snapshot.member_id,
            pending: None,
            possible_copy: None,
            uncertain_copies: BTreeSet::new(),
        };
        fold.recount(plan)?;
        if fold.state() != original_state {
            return Err(codec::invalid());
        }
        validate_projection(plan, &original_state)?;
        let mut file_ids = BTreeSet::new();
        let mut slots = BTreeSet::new();
        let mut outputs = BTreeSet::new();
        let mut copies = BTreeSet::new();
        for file in &snapshot.files {
            if !file_ids.insert(file.file_id)
                || !slots.insert(file.slot_id)
                || file.published.is_none()
                || file.temporary
                || file.logical_len == 0
                || file.charged_bytes == 0
                || file
                    .published
                    .is_some_and(|key| !plan.outputs.contains(&key))
                || !outputs.insert(file.published)
                || file.native_object.file_type != FileType::Regular
                || file.native_object.filesystem_domain != plan.filesystem_domain
                || file.held_copy_ids.iter().any(|id| !copies.insert(*id))
            {
                return Err(codec::invalid());
            }
            file.native_object.validate()?;
            codec::encode(file)?;
        }
        let mut leases = BTreeSet::new();
        let mut reader_copies = BTreeSet::new();
        for reader in &snapshot.readers {
            let file = snapshot
                .files
                .iter()
                .find(|f| f.file_id == reader.file_id)
                .ok_or_else(codec::invalid)?;
            if !leases.insert(reader.lease_id)
                || !reader_copies.insert(reader.copy_id)
                || reader.asset_id != plan.asset_id
                || reader.memory_cap == 0
                || reader.read_range.start > reader.cursor
                || reader.cursor > reader.read_range.end_exclusive
                || reader.read_range.end_exclusive > file.logical_len
                || !file.held_copy_ids.contains(&reader.copy_id)
            {
                return Err(codec::invalid());
            }
            codec::encode(reader)?;
        }
        if copies != reader_copies {
            return Err(codec::invalid());
        }
        Ok(())
    }
    fn matches_ref(
        &self,
        reference: &OwnedDirectoryRefV1,
        directory: &BootstrapDirectoryV1,
    ) -> bool {
        reference.owning_obligation == self.obligation
            && reference.member_id == self.member
            && reference.directory_id == directory.observed.directory_id
    }
    pub(crate) fn apply(
        mut self,
        plan: &ScratchPlanV3,
        operation_id: Id,
        change: &AssetChangeV3,
    ) -> Result<Self, CustodyError> {
        if change.scratch_member_id != self.member
            || change.asset_id != plan.asset_id
            || change.generation_epoch != plan.generation_epoch
            || change.previous != self.output.state.head
            || self
                .output
                .state
                .head
                .as_ref()
                .is_some_and(|h| h.operation_id == operation_id)
        {
            return Err(codec::invalid());
        }
        match &change.event {
            AssetEventV3::ArmBootstrapV1 { io_id, operation } => {
                if *io_id != operation_id
                    || self.pending.is_some()
                    || self.output.has_pending()
                    || (self.output.state.lifecycle != ScratchLifecycleV1::Open
                        && !matches!(
                            operation,
                            BootstrapIoV1::CloseBootstrapHandles { .. }
                                | BootstrapIoV1::ResolvePossibleChild { .. }
                                | BootstrapIoV1::RemoveRenditionChild { .. }
                                | BootstrapIoV1::SyncRetainedParent { .. }
                                | BootstrapIoV1::CloseRetainedHandles { .. }
                        ))
                {
                    return Err(codec::invalid());
                }
                self.arm(plan, *io_id, operation)?;
                self.pending = Some((*io_id, operation.clone()));
            }
            AssetEventV3::ObserveBootstrapV1 {
                io_id,
                native_join_operation_id: _,
                result,
            } => {
                let (armed, operation) = self.pending.clone().ok_or_else(codec::invalid)?;
                if armed != *io_id || *io_id == operation_id {
                    return Err(codec::invalid());
                }
                self.observe(plan, operation_id, *io_id, &operation, result)?;
                self.pending = None;
            }
            AssetEventV3::OutputV2 { event } => {
                // Fence records all already-possible native effects, even while
                // bootstrap is unfinished. No fresh output in that state.
                let retained_child_ready = self.bootstrap.phase == BootstrapPhase::Closed
                    && self.output.state.lifecycle == ScratchLifecycleV1::RetainedReadOnly;
                let retirement_ready = self.bootstrap.phase == BootstrapPhase::SinkReady
                    && self.output.state.lifecycle == ScratchLifecycleV1::Retiring;
                let empty_cancel = matches!(event, AssetEventV1::BeginProducerCancellation { .. })
                    && self.bootstrap.phase == BootstrapPhase::Unstarted
                    && self.output.state.lifecycle == ScratchLifecycleV1::Open
                    && self.output.state.head.is_some()
                    && self.output.state.totals.pending_jobs == 0
                    && self.bootstrap.current.is_none()
                    && self.bootstrap.parent.is_none()
                    && self.bootstrap.possible_child.is_none()
                    && self.possible_copy.is_none()
                    && self.uncertain_copies.is_empty()
                    && self.output.state.directory.is_none()
                    && self.output.files.is_empty()
                    && self.output.readers().is_empty();
                let partial_cancel =
                    matches!(event, AssetEventV1::BeginProducerCancellation { .. })
                        && matches!(
                            self.bootstrap.phase,
                            BootstrapPhase::Traversing | BootstrapPhase::ParentReady
                        )
                        && self.output.state.lifecycle == ScratchLifecycleV1::Open
                        && self.bootstrap.possible_child.is_none()
                        && self.possible_copy.is_none()
                        && self.uncertain_copies.is_empty()
                        && self.output.state.directory.is_none();
                if !matches!(event, AssetEventV1::Fence { .. })
                    && ((!retained_child_ready
                        && !retirement_ready
                        && !partial_cancel
                        && !empty_cancel
                        && self.bootstrap.phase != BootstrapPhase::SinkReady)
                        || self.pending.is_some())
                {
                    return Err(codec::invalid());
                }
                if matches!(event, AssetEventV1::Freeze { .. })
                    && self.bootstrap.possible_child.is_some()
                {
                    return Err(codec::invalid());
                }
                self.output
                    .apply_event(plan.output_view(), operation_id, event)?;
                if empty_cancel {
                    // The full lineage has already observed the original
                    // root-open arm as NeverEntered. No child or File exists
                    // to close, so cancellation itself is the terminal audit.
                    self.output.state.lifecycle = ScratchLifecycleV1::RetiredClosed;
                    self.bootstrap.phase = BootstrapPhase::Closed;
                }
            }
        }
        self.output.state.revision = self
            .output
            .state
            .revision
            .checked_add(1)
            .ok_or_else(codec::invalid)?;
        self.output.state.head = Some(AssetEventRefV1 {
            operation_id,
            revision: self.output.state.revision,
        });
        self.recount(plan)?;
        validate_output_projection(
            plan.output_view(),
            &self.output.state,
            partial_producer_cancellation_projection(plan, &self.state()),
        )?;
        if self.output.state.lifecycle == ScratchLifecycleV1::RetiredClosed
            && (self.bootstrap.phase != BootstrapPhase::Closed
                || self.bootstrap.current.is_some()
                || self.bootstrap.parent.is_some()
                || self.bootstrap.possible_child.is_some()
                || self.retained_copies_nonempty())
        {
            return Err(codec::invalid());
        }
        codec::encode(&self.state())?;
        Ok(self)
    }
    fn arm(
        &mut self,
        plan: &ScratchPlanV3,
        io_id: Id,
        operation: &BootstrapIoV1,
    ) -> Result<(), CustodyError> {
        let state = &self.bootstrap;
        let copying = match operation {
            BootstrapIoV1::OpenNamespaceRoot { copy_id } => {
                if state.phase != BootstrapPhase::Unstarted
                    || state.current.is_some()
                    || state.parent.is_some()
                {
                    return Err(codec::invalid());
                }
                Some(*copy_id)
            }
            BootstrapIoV1::OpenNextComponent {
                index,
                parent,
                copy_id,
            } => {
                if state.phase != BootstrapPhase::Traversing
                    || *index != state.next_component
                    || usize::from(*index) >= plan.bootstrap().components.len()
                    || !state
                        .current
                        .as_ref()
                        .is_some_and(|d| self.matches_ref(parent, d))
                {
                    return Err(codec::invalid());
                }
                Some(*copy_id)
            }
            BootstrapIoV1::CreateRenditionChild { parent, copy_id } => {
                if state.phase != BootstrapPhase::ParentReady
                    || self.output.state.directory.is_some()
                    || !state
                        .parent
                        .as_ref()
                        .is_some_and(|d| self.matches_ref(parent, d))
                {
                    return Err(codec::invalid());
                }
                Some(*copy_id)
            }
            BootstrapIoV1::CloseBootstrapHandles { copy_ids } => {
                let held = self.held_copies();
                let partial_cancel = self.output.state.lifecycle == ScratchLifecycleV1::Retiring
                    && matches!(
                        state.phase,
                        BootstrapPhase::Traversing | BootstrapPhase::ParentReady
                    )
                    && state.possible_child.is_none()
                    && self.possible_copy.is_none()
                    && self.uncertain_copies.is_empty()
                    && self.output.state.directory.is_none()
                    && self.output.files.is_empty()
                    && self.output.readers().is_empty()
                    && !self.output.has_pending()
                    && !held.is_empty()
                    && copy_ids.iter().copied().collect::<BTreeSet<_>>() == held
                    && copy_ids.len() == held.len();
                if (self.output.state.lifecycle == ScratchLifecycleV1::Retiring && !partial_cancel)
                    || (!partial_cancel
                        && copy_ids.is_empty()
                        && (state.phase != BootstrapPhase::UnknownHeld
                            || state.possible_child.is_some()))
                    || copy_ids.len() > 2
                    || copy_ids.iter().collect::<BTreeSet<_>>().len() != copy_ids.len()
                    || copy_ids.iter().any(|id| !held.contains(id))
                {
                    return Err(codec::invalid());
                }
                None
            }
            BootstrapIoV1::RemoveRenditionChild {
                parent,
                child_directory_id,
                child_component,
            } => {
                let directory = self
                    .output
                    .state
                    .directory
                    .as_ref()
                    .ok_or_else(codec::invalid)?;
                if self.bootstrap.phase != BootstrapPhase::SinkReady
                    || self.output.state.lifecycle != ScratchLifecycleV1::Retiring
                    || !self.output.files.is_empty()
                    || !self.output.readers().is_empty()
                    || self.output.has_pending()
                    || !self.output.state.directory_synced
                    || directory.directory_id != *child_directory_id
                    || child_component != &plan.bootstrap().child_component
                    || !self
                        .bootstrap
                        .parent
                        .as_ref()
                        .is_some_and(|d| self.matches_ref(parent, d))
                {
                    return Err(codec::invalid());
                }
                None
            }
            BootstrapIoV1::CloseRetainedHandles { copy_ids } => {
                let mut exact = self.held_copies();
                let directory = self
                    .output
                    .state
                    .directory
                    .as_ref()
                    .ok_or_else(codec::invalid)?;
                if self.bootstrap.phase != BootstrapPhase::RetainedChildRemoved
                    || self.output.state.lifecycle != ScratchLifecycleV1::Retiring
                    || !self.output.files.is_empty()
                    || !self.output.readers().is_empty()
                    || self.output.has_pending()
                    || !self.bootstrap.retained_parent_synced
                {
                    return Err(codec::invalid());
                }
                exact.extend(directory.held_copy_ids.iter().copied());
                let exact: Vec<_> = exact.into_iter().collect();
                if exact.is_empty()
                    || exact.len() > plan.limits.live_handles as usize
                    || *copy_ids != exact
                {
                    return Err(codec::invalid());
                }
                None
            }
            BootstrapIoV1::SyncRetainedParent { parent } => {
                if self.bootstrap.phase != BootstrapPhase::RetainedChildRemoved
                    || self.output.state.lifecycle != ScratchLifecycleV1::Retiring
                    || !self.output.files.is_empty()
                    || !self.output.readers().is_empty()
                    || self.output.has_pending()
                    || self.bootstrap.retained_parent_synced
                    || !self
                        .bootstrap
                        .parent
                        .as_ref()
                        .is_some_and(|d| self.matches_ref(parent, d))
                {
                    return Err(codec::invalid());
                }
                None
            }
            BootstrapIoV1::ResolvePossibleChild {
                create_io_id,
                parent,
            } => {
                if self.output.state.directory.is_some()
                    || self
                        .possible_copy
                        .is_some_and(|copy| self.uncertain_copies.contains(&copy))
                    || !state
                        .possible_child
                        .as_ref()
                        .is_some_and(|p| p.create_io_id == *create_io_id && p.parent == *parent)
                    || !state
                        .parent
                        .as_ref()
                        .is_some_and(|d| self.matches_ref(parent, d))
                {
                    return Err(codec::invalid());
                }
                None
            }
        };
        if copying.is_some_and(|copy| {
            self.held_copies().contains(&copy)
                || self.possible_copy == Some(copy)
                || self
                    .output
                    .state
                    .directory
                    .as_ref()
                    .is_some_and(|d| d.held_copy_ids.contains(&copy))
        }) {
            return Err(codec::invalid());
        }
        if let BootstrapIoV1::CreateRenditionChild { parent, copy_id } = operation {
            self.possible_copy = Some(*copy_id);
            self.bootstrap.possible_child = Some(PossibleChildV1 {
                create_io_id: io_id,
                parent: parent.clone(),
                child_component: plan.bootstrap().child_component.clone(),
                namespace_effect: ChildEffect::Unknown,
            });
            self.bootstrap.phase = BootstrapPhase::ChildPossible;
        }
        if matches!(
            operation,
            BootstrapIoV1::CloseBootstrapHandles { .. }
                | BootstrapIoV1::CloseRetainedHandles { .. }
        ) {
            self.bootstrap.phase = BootstrapPhase::Closing;
        }
        if matches!(operation, BootstrapIoV1::RemoveRenditionChild { .. }) {
            // arm() already required the completed child-directory sync.
            // Consume its quiescent marker before this destructive job becomes
            // pending; the original durable sync remains in the lineage.
            self.output.state.directory_synced = false;
            self.bootstrap.phase = BootstrapPhase::RemovingRetainedChild;
        }
        Ok(())
    }
    fn held_copies(&self) -> BTreeSet<Id> {
        self.bootstrap
            .current
            .iter()
            .chain(self.bootstrap.parent.iter())
            .flat_map(|d| d.observed.held_copy_ids.iter().copied())
            .chain(self.uncertain_copies.iter().copied())
            .collect()
    }
    fn retained_copies_nonempty(&self) -> bool {
        !self.held_copies().is_empty()
            || self
                .output
                .state
                .directory
                .as_ref()
                .is_some_and(|directory| !directory.held_copy_ids.is_empty())
    }
    fn observe(
        &mut self,
        plan: &ScratchPlanV3,
        operation_id: Id,
        io_id: Id,
        operation: &BootstrapIoV1,
        result: &BootstrapResultV1,
    ) -> Result<(), CustodyError> {
        if let NativeOutcomeV1::Failed { error_class } = &result.outcome {
            codec::text(error_class)?;
        }
        let completed = result.outcome == NativeOutcomeV1::Completed;
        let mut all_held_copies = self.held_copies();
        if let Some(directory) = &self.output.state.directory {
            all_held_copies.extend(directory.held_copy_ids.iter().copied());
        }
        let close_limit = if matches!(operation, BootstrapIoV1::CloseRetainedHandles { .. }) {
            usize::try_from(plan.limits.live_handles).unwrap_or(usize::MAX)
        } else {
            2
        };
        if result.closed_copy_ids.len() > close_limit
            || result.closed_copy_ids.iter().collect::<BTreeSet<_>>().len()
                != result.closed_copy_ids.len()
            || result
                .closed_copy_ids
                .iter()
                .any(|id| !all_held_copies.contains(id))
        {
            return Err(codec::invalid());
        }
        if result.outcome == NativeOutcomeV1::NeverEntered {
            if result.acquired.is_some()
                || !result.closed_copy_ids.is_empty()
                || result.child_effect != ChildEffect::None
                || result.inventory != InventoryCertainty::Exact
            {
                return Err(codec::invalid());
            }
            if matches!(operation, BootstrapIoV1::CreateRenditionChild { .. }) {
                self.bootstrap.possible_child = None;
                self.possible_copy = None;
                self.bootstrap.phase = BootstrapPhase::ParentReady;
            }
            return Ok(());
        }
        let new_copy = match operation {
            BootstrapIoV1::OpenNamespaceRoot { copy_id }
            | BootstrapIoV1::OpenNextComponent { copy_id, .. }
            | BootstrapIoV1::CreateRenditionChild { copy_id, .. } => Some(*copy_id),
            BootstrapIoV1::ResolvePossibleChild { .. } => self.possible_copy,
            BootstrapIoV1::CloseBootstrapHandles { .. }
            | BootstrapIoV1::RemoveRenditionChild { .. }
            | BootstrapIoV1::SyncRetainedParent { .. }
            | BootstrapIoV1::CloseRetainedHandles { .. } => None,
        };
        if let Some(dir) = &result.acquired {
            if dir.directory_id != operation_id
                || dir.acquisition_io_id != io_id
                || new_copy.is_none_or(|id| dir.held_copy_ids != [id])
                || dir.native_object.file_type != FileType::Directory
                || dir.native_object.filesystem_domain != plan.filesystem_domain
            {
                return Err(codec::invalid());
            }
            dir.native_object.validate()?;
            if completed
                && (!matches!(dir.native_object.device, NativeValue::Known { .. })
                    || !matches!(dir.native_object.inode, NativeValue::Known { .. }))
            {
                return Err(codec::invalid());
            }
        }
        let expected_closed: Vec<_> = match operation {
            BootstrapIoV1::OpenNextComponent { .. } => self
                .bootstrap
                .current
                .as_ref()
                .ok_or_else(codec::invalid)?
                .observed
                .held_copy_ids
                .clone(),
            BootstrapIoV1::CloseBootstrapHandles { copy_ids }
            | BootstrapIoV1::CloseRetainedHandles { copy_ids } => copy_ids.clone(),
            BootstrapIoV1::RemoveRenditionChild { .. } => vec![],
            BootstrapIoV1::SyncRetainedParent { .. } => vec![],
            _ => vec![],
        };
        if result
            .closed_copy_ids
            .iter()
            .any(|id| !expected_closed.contains(id))
            || (completed
                && (result.closed_copy_ids != expected_closed
                    || result.inventory != InventoryCertainty::Exact))
        {
            return Err(codec::invalid());
        }
        // Unknown post-entry inventory retains the exact armed copy slot even
        // without a native identity. It cannot become subbudget credit.
        if result.inventory == InventoryCertainty::Unknown && result.acquired.is_none() {
            if let Some(copy) = new_copy {
                self.uncertain_copies.insert(copy);
            }
        }
        self.uncertain_copies
            .retain(|copy| !result.closed_copy_ids.contains(copy));
        for directory in [&mut self.bootstrap.current, &mut self.bootstrap.parent] {
            if let Some(d) = directory {
                d.observed
                    .held_copy_ids
                    .retain(|id| !result.closed_copy_ids.contains(id));
                if d.observed.held_copy_ids.is_empty() {
                    *directory = None;
                }
            }
        }
        match operation {
            BootstrapIoV1::OpenNamespaceRoot { .. } | BootstrapIoV1::OpenNextComponent { .. } => {
                if result.child_effect != ChildEffect::None
                    || (completed && result.acquired.is_none())
                {
                    return Err(codec::invalid());
                }
                if let Some(dir) = &result.acquired {
                    let index = match operation {
                        BootstrapIoV1::OpenNextComponent { index, .. } => Some(*index),
                        _ => None,
                    };
                    let next = index.map_or(0, |i| i + 1);
                    let final_parent =
                        completed && usize::from(next) == plan.bootstrap().components.len();
                    let observed = BootstrapDirectoryV1 {
                        observed: dir.clone(),
                        component_index: index,
                        kind: if final_parent {
                            BootstrapDirectoryKind::ConfiguredParentV1
                        } else if index.is_none() {
                            BootstrapDirectoryKind::NamespaceAnchorV1
                        } else {
                            BootstrapDirectoryKind::InfrastructureTraversalV1
                        },
                    };
                    if final_parent {
                        self.bootstrap.parent = Some(observed);
                    } else if self.bootstrap.current.is_none() {
                        self.bootstrap.current = Some(observed);
                    } else if self.bootstrap.parent.is_none() {
                        self.bootstrap.parent = Some(observed);
                    } else {
                        return Err(codec::invalid());
                    }
                    if completed {
                        self.bootstrap.next_component = next;
                        self.bootstrap.phase = if final_parent {
                            BootstrapPhase::ParentReady
                        } else {
                            BootstrapPhase::Traversing
                        };
                    }
                }
            }
            BootstrapIoV1::CreateRenditionChild { .. }
            | BootstrapIoV1::ResolvePossibleChild { .. } => {
                if !matches!(
                    result.child_effect,
                    ChildEffect::Created | ChildEffect::Unknown | ChildEffect::Unchanged
                ) || (result.acquired.is_some() && result.child_effect != ChildEffect::Created)
                    || (completed && result.child_effect == ChildEffect::Unknown)
                {
                    return Err(codec::invalid());
                }
                if let Some(possible) = &mut self.bootstrap.possible_child {
                    possible.namespace_effect = result.child_effect.clone();
                }
                if let Some(dir) = &result.acquired {
                    // A same-device bind mount is not same-mount proof.
                    let parent = &self
                        .bootstrap
                        .parent
                        .as_ref()
                        .ok_or_else(codec::invalid)?
                        .observed
                        .native_object;
                    if completed
                        && (!matches!((&parent.mount_id, &dir.native_object.mount_id),
                        (NativeValue::Known { value: a }, NativeValue::Known { value: b }) if a == b)
                            || parent.device != dir.native_object.device)
                    {
                        return Err(codec::invalid());
                    }
                    self.output.state.directory = Some(dir.clone());
                    if completed {
                        self.bootstrap.possible_child = None;
                        self.possible_copy = None;
                        self.bootstrap.phase = BootstrapPhase::SinkReady;
                    }
                } else if completed {
                    if !matches!(operation, BootstrapIoV1::ResolvePossibleChild { .. })
                        || result.child_effect != ChildEffect::Unchanged
                    {
                        return Err(codec::invalid());
                    }
                    self.bootstrap.possible_child = None;
                    self.possible_copy = None;
                    self.bootstrap.phase = BootstrapPhase::ParentReady;
                }
            }
            BootstrapIoV1::CloseBootstrapHandles { .. } => {
                if result.acquired.is_some() || result.child_effect != ChildEffect::None {
                    return Err(codec::invalid());
                }
                if completed
                    && self.held_copies().is_empty()
                    && self.bootstrap.possible_child.is_none()
                {
                    self.bootstrap.phase = BootstrapPhase::Closed;
                    if self.output.state.lifecycle == ScratchLifecycleV1::Retiring {
                        if self.output.state.directory.is_some()
                            || !self.output.files.is_empty()
                            || !self.output.readers().is_empty()
                            || self.output.has_pending()
                        {
                            return Err(codec::invalid());
                        }
                        self.output.state.lifecycle = ScratchLifecycleV1::RetiredClosed;
                    }
                }
            }
            BootstrapIoV1::RemoveRenditionChild { .. } => {
                if result.acquired.is_some()
                    || !result.closed_copy_ids.is_empty()
                    || (completed && result.child_effect != ChildEffect::Removed)
                    || (!completed
                        && !matches!(
                            result.child_effect,
                            ChildEffect::Unknown | ChildEffect::Unchanged
                        ))
                {
                    return Err(codec::invalid());
                }
                if completed {
                    self.bootstrap.retained_parent_synced = false;
                    self.bootstrap.phase = BootstrapPhase::RetainedChildRemoved;
                }
            }
            BootstrapIoV1::SyncRetainedParent { .. } => {
                if result.acquired.is_some()
                    || !result.closed_copy_ids.is_empty()
                    || result.child_effect != ChildEffect::None
                {
                    return Err(codec::invalid());
                }
                if completed {
                    self.bootstrap.retained_parent_synced = true;
                }
            }
            BootstrapIoV1::CloseRetainedHandles { .. } => {
                if result.acquired.is_some() || result.child_effect != ChildEffect::None {
                    return Err(codec::invalid());
                }
                if completed {
                    if self.output.state.directory.as_ref().is_none_or(|d| {
                        d.held_copy_ids
                            .iter()
                            .any(|id| !result.closed_copy_ids.contains(id))
                    }) {
                        return Err(codec::invalid());
                    }
                    self.output.state.directory = None;
                    self.output.state.lifecycle = ScratchLifecycleV1::RetiredClosed;
                    self.output.state.directory_synced = false;
                    self.bootstrap.retained_parent_synced = false;
                    self.bootstrap.phase = BootstrapPhase::Closed;
                } else if !result.closed_copy_ids.is_empty()
                    || result.inventory != InventoryCertainty::Unknown
                {
                    // Do not claim a partial close inventory without a distinct
                    // descriptor-by-descriptor result model.
                    return Err(codec::invalid());
                }
            }
        }
        if !completed || result.inventory != InventoryCertainty::Exact {
            self.bootstrap.phase = BootstrapPhase::UnknownHeld;
        }
        Ok(())
    }
    fn recount(&mut self, plan: &ScratchPlanV3) -> Result<(), CustodyError> {
        self.output.recount(plan.output_view())?;
        let held = u32::try_from(self.held_copies().len()).map_err(|_| codec::invalid())?;
        let b = &plan.bootstrap().bounds;
        let t = &mut self.output.state.totals;
        t.handles_reserved = t
            .handles_reserved
            .checked_add(held)
            .ok_or_else(codec::invalid)?;
        if let Some((_, pending)) = &self.pending {
            if t.pending_jobs != 0 {
                return Err(codec::invalid());
            }
            t.pending_jobs = 1;
            if matches!(
                pending,
                BootstrapIoV1::OpenNamespaceRoot { .. }
                    | BootstrapIoV1::OpenNextComponent { .. }
                    | BootstrapIoV1::CreateRenditionChild { .. }
                    | BootstrapIoV1::ResolvePossibleChild { .. }
            ) {
                t.handles_reserved = t
                    .handles_reserved
                    .checked_add(1)
                    .ok_or_else(codec::invalid)?;
            }
        }
        if t.handles_reserved > 0
            || self.pending.is_some()
            || self.bootstrap.possible_child.is_some()
        {
            t.memory_reserved_bytes = t
                .memory_reserved_bytes
                .checked_add(b.resident_memory_bytes)
                .ok_or_else(codec::invalid)?;
        }
        if self.output.state.directory.is_some() || self.bootstrap.possible_child.is_some() {
            // Common output fold already charges the child directory, if known.
            let child = if self.output.state.directory.is_some() {
                b.child_directory_reserve_bytes
                    .checked_sub(plan.limits.directory_charge())
                    .ok_or_else(codec::invalid)?
            } else {
                b.child_directory_reserve_bytes
            };
            t.charged_storage_bytes = t
                .charged_storage_bytes
                .checked_add(child)
                .and_then(|n| n.checked_add(b.parent_entry_reserve_bytes))
                .ok_or_else(codec::invalid)?;
        }
        Ok(())
    }
}
