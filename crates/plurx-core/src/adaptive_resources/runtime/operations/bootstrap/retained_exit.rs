//! Local descriptor closure for final process exit. Assets, durable heads and
//! the original S reservation remain unchanged. This is never asset settlement.
use super::native::cleanup::CleanupCell;
use super::*;
use std::fs::File;
use v2::scratch::{AssetEventRefV1, DirectoryObservedV1, ScratchLifecycleV1};

// This private addition belongs to each independently charged permanent pool
// record, not to a caller's old/imported S claim. Check before any clone.
pub(in crate::adaptive_resources::runtime) const NATIVE_EXIT_STORAGE_BYTES: usize = 16 * 1024;

pub(in crate::adaptive_resources::runtime) struct RetainedExitRun {
    cell: CleanupCell,
    head: Option<AssetEventRefV1>,
    native_operation: Uuid,
    original: Option<(Uuid, Uuid)>,
    closed: bool,
}

pub(in crate::adaptive_resources::runtime::operations) fn retained_key(
    bootstrap: &BootstrapMember,
) -> Option<&str> {
    (bootstrap.fold.as_ref()?.state_ref().0.lifecycle == ScratchLifecycleV1::RetainedReadOnly)
        .then_some(bootstrap.plan.rendition_id.as_str())
}

pub(in crate::adaptive_resources::runtime::operations) fn retained_head(
    bootstrap: &BootstrapMember,
) -> Option<&AssetEventRefV1> {
    bootstrap.fold.as_ref()?.state_ref().0.head.as_ref()
}

fn baseline(fold: &BootstrapFold) -> [Option<&DirectoryObservedV1>; 3] {
    let (asset, bootstrap) = fold.state_ref();
    [
        bootstrap.current.as_ref().map(|d| &d.observed),
        bootstrap.parent.as_ref().map(|d| &d.observed),
        asset.directory.as_ref(),
    ]
}

impl RetainedExitRun {
    /// Conservative requested-capacity peak for all metadata/action/input/
    /// result copies in the existing native close cell and its original job.
    /// Executor backing remains Unknown; this does not mint complete peaks.
    pub(in crate::adaptive_resources::runtime) fn requested_storage(
        fold: &BootstrapFold,
    ) -> Result<usize, OperationError> {
        let observed = baseline(fold);
        let mut count = 0usize;
        let mut bytes = 0usize;
        for directory in observed.iter().flatten() {
            if directory.held_copy_ids.len() != 1 {
                return Err(OperationError::State);
            }
            count += 1;
            bytes = bytes
                .checked_add(
                    native::directory_metadata_bytes(directory).ok_or(OperationError::State)?,
                )
                .ok_or(OperationError::State)?;
        }
        if count == 0 || count > 3 {
            return Err(OperationError::State);
        }
        let directory_vectors = 4usize
            .checked_mul(std::mem::size_of::<DirectoryObservedV1>())
            .and_then(|n| n.checked_add(bytes))
            .and_then(|n| n.checked_mul(5))
            .ok_or(OperationError::State)?;
        // Vec growth for at most three positive closes may reserve four IDs.
        let id_vectors = 4usize
            .checked_mul(std::mem::size_of::<v2::Id>())
            .and_then(|n| n.checked_mul(5))
            .ok_or(OperationError::State)?;
        directory_vectors
            .checked_add(id_vectors)
            .and_then(|n| n.checked_add(super::btree_requested_upper_bound::<v2::Id, File>(count)?))
            // lend_observed_directories retains its exact-copy set while
            // collecting the original Files into the separate lent map.
            .and_then(|n| n.checked_add(super::btree_requested_upper_bound::<v2::Id, ()>(count)?))
            .ok_or(OperationError::State)
    }

    pub(in crate::adaptive_resources::runtime::operations) fn prepare(
        bootstrap: &mut BootstrapMember,
        bundle: Uuid,
        member: Uuid,
    ) -> Result<Self, OperationError> {
        super::readers::clear_terminal_readers(bootstrap)?;
        let fold = bootstrap.fold.as_ref().ok_or(OperationError::State)?;
        if fold.state_ref().0.lifecycle != ScratchLifecycleV1::RetainedReadOnly
            || Self::requested_storage(fold)? > NATIVE_EXIT_STORAGE_BYTES
        {
            return Err(OperationError::State);
        }
        // The independently paid bound above precedes every allocation below.
        let observed: Vec<_> = baseline(fold).into_iter().flatten().cloned().collect();
        let mut ids: Vec<_> = observed.iter().map(|d| d.held_copy_ids[0]).collect();
        ids.sort_unstable();
        let cell = CleanupCell::close_retained(&observed, &ids)?;
        Ok(Self {
            cell,
            head: Some(
                fold.state_ref()
                    .0
                    .head
                    .as_ref()
                    .ok_or(OperationError::State)?
                    .clone(),
            ),
            native_operation: Uuid::new_v4(),
            original: Some((bundle, member)),
            closed: false,
        })
    }

    pub(in crate::adaptive_resources::runtime::operations) async fn finish(
        &mut self,
        bootstrap: &mut BootstrapMember,
        bundle: Uuid,
        member: Uuid,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        if Instant::now() >= deadline {
            return Err(OperationError::Deadline);
        }
        if self
            .original
            .is_some_and(|identity| identity != (bundle, member))
        {
            return Err(OperationError::State);
        }
        self.original = Some((bundle, member));
        let fold = bootstrap.fold.as_ref().ok_or(OperationError::State)?;
        let (asset, _) = fold.state_ref();
        if asset.lifecycle != ScratchLifecycleV1::RetainedReadOnly
            || asset.totals.pending_jobs != 0
            || asset.totals.readers != 0
            || bootstrap.change.is_some()
            || bootstrap.next_arm.is_some()
            || !bootstrap.readers.is_empty()
            || !bootstrap.native_io_ready()
        {
            return Err(OperationError::State);
        }
        let head = asset.head.as_ref().ok_or(OperationError::State)?;
        if self.head.as_ref().is_some_and(|original| original != head) {
            return Err(OperationError::State);
        }
        if self.closed {
            return Ok(());
        }
        self.head = Some(head.clone());
        if !self.cell.launched() {
            let directories = baseline(fold);
            let root = bootstrap
                .native_root
                .as_mut()
                .ok_or(OperationError::State)?;
            let enter = self.cell.launch(
                root,
                &directories,
                #[cfg(all(
                    test,
                    feature = "fixtures",
                    target_os = "linux",
                    any(target_arch = "x86_64", target_arch = "aarch64")
                ))]
                (bundle, self.native_operation),
            )?;
            let _ = enter.send(());
        }
        tokio::time::timeout_at(deadline.into(), self.cell.join_registered())
            .await
            .map_err(|_| OperationError::Deadline)??;
        self.cell.acknowledge_retained_exit(
            bootstrap
                .native_root
                .as_ref()
                .ok_or(OperationError::State)?,
        )?;
        // No durable Observe, unlink, native-asset retirement or ledger refund.
        self.closed = true;
        #[cfg(not(all(
            test,
            feature = "fixtures",
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )))]
        let _ = (bundle, self.native_operation);
        Ok(())
    }

    pub(in crate::adaptive_resources::runtime) fn closed_head(&self) -> Option<&AssetEventRefV1> {
        self.closed.then_some(self.head.as_ref()).flatten()
    }
}
