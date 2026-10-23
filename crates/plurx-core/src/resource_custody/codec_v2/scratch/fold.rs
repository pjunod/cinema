//! Deterministic audit fold. Input is a chain of complete journal result events,
//! not caller-supplied totals. A fold never constructs a native receipt.
use super::*;
mod readers;
#[cfg(test)]
mod retained_reader_tests;

pub(crate) struct ScratchFold {
    pub(crate) state: ScratchStateV2,
    pub(crate) files: Vec<FileObservedV1>,
    readers: Vec<ReaderLeaseV1>,
    pending: Option<(Id, AssetIoV1)>,
}
impl ScratchFold {
    pub(super) fn has_pending(&self) -> bool {
        self.pending.is_some()
    }
    pub(super) fn readers(&self) -> &[ReaderLeaseV1] {
        &self.readers
    }
    pub(super) fn from_frozen_inventory(
        state: ScratchStateV2,
        files: Vec<FileObservedV1>,
        readers: Vec<ReaderLeaseV1>,
    ) -> Self {
        Self {
            state,
            files,
            readers,
            pending: None,
        }
    }
    pub(crate) fn empty(plan: &ScratchPlanV2) -> Result<Self, CustodyError> {
        validate_plan(plan)?;
        Ok(Self::empty_output(plan.asset_id))
    }
    pub(super) fn empty_output(asset_id: Id) -> Self {
        Self {
            state: ScratchStateV2 {
                version: 2,
                asset_id,
                directory: None,
                head: None,
                revision: 0,
                lifecycle: ScratchLifecycleV1::Open,
                totals: AssetTotalsV1::default(),
                directory_synced: false,
            },
            files: Vec::new(),
            readers: Vec::new(),
            pending: None,
        }
    }
    pub(crate) fn apply(
        mut self,
        plan: &ScratchPlanV2,
        operation_id: Id,
        change: &AssetChangeV2,
    ) -> Result<Self, CustodyError> {
        if change.asset_id != plan.asset_id
            || change.generation_epoch != plan.generation_epoch
            || change.previous != self.state.head
            || self
                .state
                .head
                .as_ref()
                .is_some_and(|h| h.operation_id == operation_id)
        {
            return Err(codec::invalid());
        }
        // Consume the transient fold on error rather than cloning the entire
        // movie inventory per event. A refused fold cannot be reused as state.
        self.apply_event(plan.output_view(), operation_id, &change.event)?;
        self.state.revision = self
            .state
            .revision
            .checked_add(1)
            .ok_or_else(codec::invalid)?;
        self.state.head = Some(AssetEventRefV1 {
            operation_id,
            revision: self.state.revision,
        });
        self.recount(plan.output_view())?;
        validate_projection(plan, &self.state)?;
        Ok(self)
    }
    pub(super) fn apply_event(
        &mut self,
        plan: OutputPlanView<'_>,
        operation_id: Id,
        event: &AssetEventV1,
    ) -> Result<(), CustodyError> {
        match event {
            AssetEventV1::ArmIo { io_id, operation } => {
                let read_only = matches!(
                    operation,
                    AssetIoV1::ReadRange { .. } | AssetIoV1::CloseReader { .. }
                ) || (self.state.lifecycle == ScratchLifecycleV1::RetainedReadOnly
                    && matches!(operation, AssetIoV1::OpenReader { .. }));
                let retirement_io = self.state.lifecycle == ScratchLifecycleV1::Retiring
                    && matches!(
                        operation,
                        AssetIoV1::RemoveOwned { .. } | AssetIoV1::SyncFiles { .. }
                    );
                if (self.state.lifecycle != ScratchLifecycleV1::Open
                    && !read_only
                    && !retirement_io)
                    || self.pending.is_some()
                    || *io_id != operation_id
                {
                    return Err(codec::invalid());
                }
                match operation {
                    AssetIoV1::OpenReader { .. }
                    | AssetIoV1::ReadRange { .. }
                    | AssetIoV1::CloseReader { .. } => {
                        self.arm_reader(plan, *io_id, operation)?;
                    }
                    AssetIoV1::CreateDirectory { copy_id } => {
                        if plan.directory_mode.is_none()
                            || self.state.directory.is_some()
                            || self.copy_exists(*copy_id)
                        {
                            return Err(codec::invalid());
                        }
                    }
                    AssetIoV1::PublishNew {
                        slot_id,
                        output,
                        logical_end,
                        payload_capacity,
                        copy_id,
                    }
                    | AssetIoV1::PublishMetadata {
                        slot_id,
                        output,
                        logical_end,
                        payload_capacity,
                        copy_id,
                    } => {
                        let metadata = matches!(
                            output,
                            OutputKey::IdentityMetadata | OutputKey::EncodedProcessMetadata
                        );
                        if metadata != matches!(operation, AssetIoV1::PublishMetadata { .. })
                            || self.state.directory.is_none()
                            || slot_id.0 != *io_id
                            || !plan.outputs.contains(output)
                            || *logical_end == 0
                            || *logical_end > plan.limits.one_io_payload_bytes
                            || *logical_end > plan.limits.one_file_logical_bytes
                            || *payload_capacity < *logical_end
                            || self.copy_exists(*copy_id)
                            || self.files.iter().any(|f| {
                                f.slot_id == *slot_id || f.published.as_ref() == Some(output)
                            })
                        {
                            return Err(codec::invalid());
                        }
                    }
                    AssetIoV1::RemoveOwned {
                        file_id,
                        expected_native_object,
                    } => {
                        // First path removes only objects with no exported copies.
                        // Reader/copy support must not silently reuse this rule.
                        let f = self.file(*file_id)?;
                        if !f.held_copy_ids.is_empty()
                            || expected_native_object
                                .as_ref()
                                .is_some_and(|expected| expected != &f.native_object)
                            || (self.state.lifecycle == ScratchLifecycleV1::Retiring
                                && expected_native_object.as_ref() != Some(&f.native_object))
                        {
                            return Err(codec::invalid());
                        }
                    }
                    AssetIoV1::SyncFiles { file_ids } => {
                        let retirement_file_set_invalid = self.state.lifecycle
                            == ScratchLifecycleV1::Retiring
                            && if self.files.is_empty() {
                                !file_ids.is_empty()
                            } else {
                                file_ids.is_empty()
                                    || file_ids.len() != self.files.len()
                                    || self.files.iter().any(|f| !file_ids.contains(&f.file_id))
                            };
                        if self.state.directory.is_none()
                            || file_ids.len() > 16
                            || file_ids.iter().collect::<BTreeSet<_>>().len() != file_ids.len()
                            || retirement_file_set_invalid
                        {
                            return Err(codec::invalid());
                        }
                        for id in file_ids {
                            self.file(*id)?;
                        }
                    }
                }
                self.pending = Some((*io_id, operation.clone()));
            }
            AssetEventV1::ObserveIo {
                io_id,
                native_join_operation_id: _,
                outcome,
                observation,
            } => {
                let (armed_id, operation) = self.pending.clone().ok_or_else(codec::invalid)?;
                if *io_id != armed_id {
                    return Err(codec::invalid());
                }
                if let NativeOutcomeV1::Failed { error_class } = outcome {
                    codec::text(error_class)?;
                }
                match (&operation, observation) {
                    (_, AssetObservationV1::NoAcquisition) => {
                        if !matches!(outcome, NativeOutcomeV1::NeverEntered) {
                            return Err(codec::invalid());
                        }
                    }
                    (
                        AssetIoV1::CreateDirectory { copy_id },
                        AssetObservationV1::Directory { observed },
                    ) => {
                        let dir = observed;
                        if matches!(outcome, NativeOutcomeV1::NeverEntered)
                            || dir.acquisition_io_id != *io_id
                            || dir.held_copy_ids != [*copy_id]
                        {
                            return Err(codec::invalid());
                        }
                        // New identities are minted only after acquisition, using
                        // this globally unique observation record's UUID. Borrow
                        // needs the exact preexisting custodian, not a new identity.
                        match plan.directory_mode {
                            Some(DirectoryModeV1::CreateExclusive)
                                if dir.directory_id == operation_id => {}
                            Some(DirectoryModeV1::BorrowExisting { asset_owner_ref })
                                if dir.directory_id == asset_owner_ref.directory_id => {}
                            _ => return Err(codec::invalid()),
                        }
                        self.state.directory = Some(dir.clone());
                    }
                    (
                        AssetIoV1::PublishNew {
                            slot_id,
                            output,
                            logical_end,
                            payload_capacity: _,
                            copy_id,
                        }
                        | AssetIoV1::PublishMetadata {
                            slot_id,
                            output,
                            logical_end,
                            payload_capacity: _,
                            copy_id,
                        },
                        AssetObservationV1::File {
                            observed: f,
                            content_binding,
                        },
                    ) => {
                        if matches!(outcome, NativeOutcomeV1::NeverEntered)
                            || f.file_id != operation_id
                            || f.slot_id != *slot_id
                            || f.creation_io_id != *io_id
                            || f.native_object.file_type != FileType::Regular
                            || &f.native_object.filesystem_domain != plan.filesystem_domain
                            || f.logical_len > *logical_end
                            || f.held_copy_ids.len() > 1
                            || f.held_copy_ids.iter().any(|c| *c != *copy_id)
                            || f.charged_bytes < plan.limits.storage_for(*logical_end)?
                            || self
                                .files
                                .iter()
                                .any(|old| old.file_id == f.file_id || old.slot_id == f.slot_id)
                        {
                            return Err(codec::invalid());
                        }
                        f.native_object.validate()?;
                        f.reported_allocated_bytes.validate()?;
                        if let NativeValue::Known { value } = f.reported_allocated_bytes {
                            if f.charged_bytes < value.max(f.logical_len) {
                                return Err(codec::invalid());
                            }
                        }
                        match outcome {
                            NativeOutcomeV1::Completed => {
                                if f.temporary
                                    || f.published.as_ref() != Some(output)
                                    || f.logical_len != *logical_end
                                    || content_binding.is_none()
                                {
                                    return Err(codec::invalid());
                                }
                                codec::long_text(
                                    content_binding.as_ref().ok_or_else(codec::invalid)?,
                                )?;
                            }
                            NativeOutcomeV1::Failed { .. } => {
                                // An error after rename may have a published alias;
                                // it is still owned, but never a manifest claim.
                                if (!f.temporary && f.published.is_none())
                                    || f.published.as_ref().is_some_and(|key| key != output)
                                    || content_binding.is_some()
                                {
                                    return Err(codec::invalid());
                                }
                            }
                            NativeOutcomeV1::NeverEntered => return Err(codec::invalid()),
                        }
                        self.files.push(f.clone());
                        self.state.directory_synced = false;
                    }
                    (
                        AssetIoV1::RemoveOwned { file_id, .. },
                        AssetObservationV1::Removed { file_id: removed },
                    ) => {
                        if file_id != removed || !matches!(outcome, NativeOutcomeV1::Completed) {
                            return Err(codec::invalid());
                        }
                        let index = self
                            .files
                            .iter()
                            .position(|f| f.file_id == *file_id)
                            .ok_or_else(codec::invalid)?;
                        self.files.remove(index);
                        self.state.directory_synced = false;
                    }
                    (
                        AssetIoV1::RemoveOwned { file_id, .. },
                        AssetObservationV1::RemovalUnresolved {
                            file_id: unresolved,
                        },
                    ) => {
                        if file_id != unresolved
                            || self.state.lifecycle != ScratchLifecycleV1::Retiring
                            || !matches!(outcome, NativeOutcomeV1::Failed { .. })
                        {
                            return Err(codec::invalid());
                        }
                        self.file(*file_id)?;
                        // Keep the file and its charge. No retry is admitted
                        // after a post-entry removal with unresolved effect.
                        self.state.lifecycle = ScratchLifecycleV1::RetirementUncertain;
                        self.state.directory_synced = false;
                    }
                    (AssetIoV1::SyncFiles { .. }, AssetObservationV1::Synced) => {
                        if !matches!(
                            outcome,
                            NativeOutcomeV1::Completed | NativeOutcomeV1::Failed { .. }
                        ) {
                            return Err(codec::invalid());
                        }
                        if self.state.lifecycle == ScratchLifecycleV1::Retiring {
                            self.state.directory_synced = *outcome == NativeOutcomeV1::Completed;
                        }
                    }
                    (
                        AssetIoV1::OpenReader { .. }
                        | AssetIoV1::ReadRange { .. }
                        | AssetIoV1::CloseReader { .. },
                        _,
                    ) => {
                        self.observe_reader(&operation, observation, outcome)?;
                    }
                    _ => return Err(codec::invalid()),
                }
                self.pending = None;
            }
            AssetEventV1::Fence { terminal_epoch } => {
                if *terminal_epoch != plan.generation_epoch
                    || self.state.lifecycle != ScratchLifecycleV1::Open
                {
                    return Err(codec::invalid());
                }
                self.state.lifecycle = ScratchLifecycleV1::Freezing;
            }
            AssetEventV1::Freeze {
                exact_head_before_freeze,
            } => {
                if self.state.head.as_ref() != Some(exact_head_before_freeze)
                    || self.state.lifecycle != ScratchLifecycleV1::Freezing
                    || self.pending.is_some()
                    || self.state.directory.is_none()
                    || self.files.iter().any(|f| {
                        !self.only_reader_copies(f) || f.temporary || f.published.is_none()
                    })
                {
                    return Err(codec::invalid());
                }
                self.state.lifecycle = ScratchLifecycleV1::Frozen;
            }
            AssetEventV1::BeginRetirement {
                exact_head_before_retirement,
            } => {
                if self.state.head.as_ref() != Some(exact_head_before_retirement)
                    || self.state.lifecycle != ScratchLifecycleV1::RetainedReadOnly
                    || self.pending.is_some()
                    || !self.readers.is_empty()
                    || self.state.totals.pending_jobs != 0
                {
                    return Err(codec::invalid());
                }
                self.state.lifecycle = ScratchLifecycleV1::Retiring;
                self.state.directory_synced = false;
            }
        }
        Ok(())
    }
    fn file(&self, id: Id) -> Result<&FileObservedV1, CustodyError> {
        self.files
            .iter()
            .find(|f| f.file_id == id)
            .ok_or_else(codec::invalid)
    }
    fn copy_exists(&self, id: Id) -> bool {
        self.state
            .directory
            .as_ref()
            .is_some_and(|d| d.held_copy_ids.contains(&id))
            || self.files.iter().any(|f| f.held_copy_ids.contains(&id))
    }
    pub(super) fn recount(&mut self, plan: OutputPlanView<'_>) -> Result<(), CustodyError> {
        let mut totals = AssetTotalsV1::default();
        if let Some(dir) = &self.state.directory {
            totals.charged_storage_bytes = plan.limits.directory_charge();
            totals.handles_reserved =
                u32::try_from(dir.held_copy_ids.len()).map_err(|_| codec::invalid())?;
        }
        for f in &self.files {
            totals.resident_file_objects = totals
                .resident_file_objects
                .checked_add(1)
                .ok_or_else(codec::invalid)?;
            totals.logical_bytes = totals
                .logical_bytes
                .checked_add(f.logical_len)
                .ok_or_else(codec::invalid)?;
            totals.charged_storage_bytes = totals
                .charged_storage_bytes
                .checked_add(f.charged_bytes)
                .ok_or_else(codec::invalid)?;
            totals.handles_reserved = totals
                .handles_reserved
                .checked_add(u32::try_from(f.held_copy_ids.len()).map_err(|_| codec::invalid())?)
                .ok_or_else(codec::invalid)?;
        }
        self.count_readers(&mut totals)?;
        if let Some((_, pending)) = &self.pending {
            totals.pending_jobs = 1;
            match pending {
                AssetIoV1::CreateDirectory { .. } => {
                    totals.charged_storage_bytes = plan.limits.directory_charge();
                    totals.handles_reserved = totals
                        .handles_reserved
                        .checked_add(1)
                        .ok_or_else(codec::invalid)?;
                }
                AssetIoV1::PublishNew {
                    logical_end,
                    payload_capacity,
                    ..
                }
                | AssetIoV1::PublishMetadata {
                    logical_end,
                    payload_capacity,
                    ..
                } => {
                    totals.resident_file_objects = totals
                        .resident_file_objects
                        .checked_add(1)
                        .ok_or_else(codec::invalid)?;
                    totals.logical_bytes = totals
                        .logical_bytes
                        .checked_add(*logical_end)
                        .ok_or_else(codec::invalid)?;
                    totals.charged_storage_bytes = totals
                        .charged_storage_bytes
                        .checked_add(plan.limits.storage_for(*logical_end)?)
                        .ok_or_else(codec::invalid)?;
                    totals.memory_reserved_bytes = totals
                        .memory_reserved_bytes
                        .checked_add(*payload_capacity)
                        .ok_or_else(codec::invalid)?;
                    totals.handles_reserved = totals
                        .handles_reserved
                        .checked_add(1)
                        .ok_or_else(codec::invalid)?;
                }
                AssetIoV1::RemoveOwned { .. } => {}
                AssetIoV1::OpenReader { lease } => {
                    totals.handles_reserved = totals
                        .handles_reserved
                        .checked_add(1)
                        .ok_or_else(codec::invalid)?;
                    totals.memory_reserved_bytes = totals
                        .memory_reserved_bytes
                        .checked_add(lease.memory_cap)
                        .ok_or_else(codec::invalid)?;
                    totals.readers = totals.readers.checked_add(1).ok_or_else(codec::invalid)?;
                }
                AssetIoV1::ReadRange { .. } | AssetIoV1::CloseReader { .. } => {}
                AssetIoV1::SyncFiles { file_ids } => {
                    // One opened file at a time plus directory sync descriptor.
                    totals.handles_reserved = totals
                        .handles_reserved
                        .checked_add(u32::from(!file_ids.is_empty()) + 1)
                        .ok_or_else(codec::invalid)?;
                }
            }
        }
        self.state.totals = totals;
        Ok(())
    }
}
