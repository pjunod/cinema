//! Final-process parking preserves the original S claim and census owner.
//! Only the permanent independently paid collector record owns exit progress.
use super::super::finite_vod::{
    RetainedExitIdentity, RetainedExitOriginal, RetainedExitReady, RetainedExitSelection,
};
use super::*;
use bootstrap::retained_exit::{retained_head, retained_key, RetainedExitRun};

pub(in crate::adaptive_resources::runtime) enum ExitProgress {
    Untouched,
    Adopting,
    Adopted,
    Closing,
    Unknown,
    Exited,
}

pub(in crate::adaptive_resources::runtime) struct RetainedExitRecord {
    pub selection: Option<RetainedExitSelection>,
    native: Option<RetainedExitRun>,
    progress: ExitProgress,
}
impl RetainedExitRecord {
    pub fn new(selection: RetainedExitSelection) -> Self {
        let progress = match &selection.original {
            RetainedExitOriginal::Book { .. } => ExitProgress::Adopted,
            RetainedExitOriginal::Imported { .. } => ExitProgress::Untouched,
        };
        Self {
            selection: Some(selection),
            native: None,
            progress,
        }
    }
}

impl NodeResourceRuntime {
    /// Synchronous bounded visitor: no directory/ID clone occurs here. A busy
    /// book, adoption, or native owner refuses the entire completion claim.
    pub(in crate::adaptive_resources::runtime) fn visit_retained_shutdown(
        &self,
        exited: impl Fn(RetainedExitIdentity) -> bool,
        mut visit: impl FnMut(RetainedExitSelection) -> Result<(), OperationError>,
    ) -> Result<(), OperationError> {
        let book = self.operations.lock().map_err(|_| OperationError::State)?;
        if !book.draining {
            return Err(OperationError::State);
        }
        for (operation, state) in &book.bundles {
            let bundle = state.try_lock().map_err(|_| OperationError::State)?;
            for (index, role) in bundle.roles.iter().enumerate() {
                for (member, native) in &role.bootstrap {
                    let Some(key) = retained_key(native) else {
                        continue;
                    };
                    if !bundle.installed
                        || role.refunded
                        || role.transition.is_some()
                        || role.pending_finite_close.is_some()
                        || role.finite_vod.is_some()
                        || role.graph.is_some()
                        || !role.sources.is_empty()
                        || !role.roots.is_empty()
                        || !role.drains.is_empty()
                        || role.bootstrap.len() != 1
                        || role.phase != Phase::Sealed
                        || role.inventory.is_none()
                    {
                        return Err(OperationError::State);
                    }
                    let handle = role.handle.as_ref().ok_or(OperationError::State)?;
                    if book
                        .bindings
                        .get(&role.binding)
                        .is_none_or(|saved| saved.obligation_id() != handle.obligation_id())
                    {
                        return Err(OperationError::State);
                    }
                    let head = retained_head(native).ok_or(OperationError::State)?;
                    let identity = RetainedExitIdentity {
                        ledger: self.ledger.id,
                        obligation: handle.obligation_id(),
                        operation: *operation,
                        role_index: index,
                        sequence: role.sequence,
                        native_member: *member,
                        native_head_operation: head.operation_id.uuid(),
                        native_head_revision: head.revision,
                        binding: role.binding,
                    };
                    visit(
                        RetainedExitSelection::new(
                            identity,
                            RetainedExitOriginal::Book {
                                preparation: OwnedPreparation {
                                    ledger: self.ledger.id,
                                    id: *operation,
                                    state: Arc::clone(state),
                                },
                                index,
                                member: *member,
                            },
                            key,
                        )
                        .map_err(|_| OperationError::State)?,
                    )?;
                }
            }
        }
        let imported = self.recovered_s.lock().map_err(|_| OperationError::State)?;
        for (id, owner) in imported.iter() {
            let binding = *self
                .recovered_bindings
                .get(id)
                .ok_or(OperationError::State)?;
            let directories = owner
                .directories
                .lock()
                .map_err(|_| OperationError::State)?;
            let claim = owner.claim();
            let sequence = if let Some(adoption) = self
                .recovered_adoptions
                .lock()
                .map_err(|_| OperationError::State)?
                .get(id)
            {
                adoption
                    .try_lock()
                    .map_err(|_| OperationError::State)?
                    .exit_sequence()
                    .unwrap_or(claim.sequence())
            } else {
                claim.sequence()
            };
            let imported_identity = RetainedExitIdentity {
                ledger: self.ledger.id,
                obligation: *id,
                operation: *id,
                role_index: 0,
                sequence,
                binding,
                native_member: claim.member_id().uuid(),
                native_head_operation: claim.current_head().operation_id.uuid(),
                native_head_revision: claim.current_head().revision,
            };
            if directories.is_none() {
                if exited(imported_identity) {
                    continue;
                }
                // None alone proves nothing. Only the exact installed book
                // adoption can account for a pair transferred out of import.
                let installed = book.bundles.values().any(|state| {
                    state.try_lock().is_ok_and(|bundle| {
                        bundle.installed
                            && bundle.roles.iter().any(|role| {
                                role.binding == binding
                                    && role.durable_tag == *id
                                    && role
                                        .handle
                                        .as_ref()
                                        .and_then(|h| h.recovery_authority())
                                        .is_some_and(|a| {
                                            a.binding() == binding && a.obligation_id() == *id
                                        })
                                    && role
                                        .bootstrap
                                        .get(&owner.claim().member_id().uuid())
                                        .is_some_and(|native| retained_key(native).is_some())
                            })
                    })
                });
                if !installed {
                    return Err(OperationError::State);
                }
                continue;
            }
            // An in-flight original Adopt is retained by its own runtime
            // receiver. The permanent paid service observes it without retry.
            let claim = owner.claim();
            let identity = RetainedExitIdentity {
                ledger: self.ledger.id,
                obligation: *id,
                operation: *id,
                role_index: 0,
                sequence,
                binding,
                native_member: claim.member_id().uuid(),
                native_head_operation: claim.current_head().operation_id.uuid(),
                native_head_revision: claim.current_head().revision,
            };
            visit(
                RetainedExitSelection::new(
                    identity,
                    RetainedExitOriginal::Imported {
                        owner: Arc::clone(owner),
                    },
                    &claim.plan().rendition_id,
                )
                .map_err(|_| OperationError::State)?,
            )?;
        }
        Ok(())
    }

    pub(in crate::adaptive_resources::runtime) async fn finish_retained_shutdown(
        &self,
        record: &mut RetainedExitRecord,
        deadline: Instant,
    ) -> Result<RetainedExitReady, OperationError> {
        let selection = record.selection.as_ref().ok_or(OperationError::State)?;
        if selection.identity.ledger != self.ledger.id
            || matches!(
                record.progress,
                ExitProgress::Unknown | ExitProgress::Exited
            )
        {
            return Err(OperationError::State);
        }
        let mut identity = selection.identity;
        {
            let state = self
                .ledger
                .inner
                .lock()
                .map_err(|_| OperationError::State)?;
            let entry = state
                .entries
                .get(&identity.binding.reservation)
                .ok_or(OperationError::State)?;
            if entry.binding != identity.binding || entry.durable != Some(identity.obligation) {
                return Err(OperationError::State);
            }
        }
        match &selection.original {
            RetainedExitOriginal::Book {
                preparation,
                index,
                member,
            } => {
                self.validate_preparation(preparation)?;
                let app = self.finite_app_state(preparation, *index)?;
                if !app.seal() {
                    return Err(OperationError::State);
                }
                self.close_dropped_finite_readers(preparation, *index, deadline)
                    .await?;
                if !app.wait_released(deadline).await {
                    return Err(OperationError::Deadline);
                }
                self.wait_finite_allocations_release(preparation, *index, deadline)
                    .await?;
                let mut bundle = preparation.state.lock().await;
                if !bundle.installed
                    || preparation.id != identity.operation
                    || *index != identity.role_index
                {
                    return Err(OperationError::State);
                }
                let role = bundle.roles.get_mut(*index).ok_or(OperationError::State)?;
                if role.binding != identity.binding
                    || role.sequence < identity.sequence
                    || role.durable_tag != identity.obligation
                    || role.refunded
                    || role.transition.is_some()
                    || role.pending_finite_close.is_some()
                    || role.finite_vod.is_some()
                    || role.graph.is_some()
                {
                    return Err(OperationError::State);
                }
                let sequence = role.sequence;
                let native = role
                    .bootstrap
                    .get_mut(member)
                    .ok_or(OperationError::State)?;
                // Original reader close/observe can advance the S head while
                // draining. Bind the final native run only after all aliases
                // and their original native operations have actually drained.
                let head = retained_head(native).ok_or(OperationError::State)?;
                identity.sequence = sequence;
                identity.native_head_operation = head.operation_id.uuid();
                identity.native_head_revision = head.revision;
                if record.native.is_none() {
                    record.native = Some(RetainedExitRun::prepare(
                        native,
                        identity.operation,
                        *member,
                    )?);
                }
                record.progress = ExitProgress::Closing;
                record
                    .native
                    .as_mut()
                    .ok_or(OperationError::State)?
                    .finish(native, identity.operation, *member, deadline)
                    .await?;
                if record
                    .native
                    .as_ref()
                    .and_then(RetainedExitRun::closed_head)
                    .is_none_or(|head| {
                        head.operation_id.uuid() != identity.native_head_operation
                            || head.revision != identity.native_head_revision
                    })
                {
                    return Err(OperationError::State);
                }
            }
            RetainedExitOriginal::Imported { owner } => {
                use std::os::fd::IntoRawFd;
                let adoption = self
                    .recovered_adoptions
                    .lock()
                    .map_err(|_| OperationError::State)?
                    .get(&identity.obligation)
                    .cloned();
                if let Some(adoption) = adoption {
                    record.progress = ExitProgress::Adopting;
                    let mut adoption = tokio::time::timeout_at(deadline.into(), adoption.lock())
                        .await
                        .map_err(|_| OperationError::Deadline)?;
                    let sequence = match tokio::time::timeout_at(
                        deadline.into(),
                        adoption.resolve_for_exit(self, owner),
                    )
                    .await
                    {
                        Err(_) => return Err(OperationError::Deadline),
                        Ok(Err(_)) => {
                            record.progress = ExitProgress::Unknown;
                            return Err(OperationError::State);
                        }
                        Ok(Ok(sequence)) => sequence,
                    };
                    identity.sequence = sequence;
                    record.progress = ExitProgress::Adopted;
                }
                let book = self.operations.lock().map_err(|_| OperationError::State)?;
                if !book.draining
                    || owner.claim().sequence() > identity.sequence
                    || owner.claim().obligation_id() != identity.obligation
                    || owner.claim().current_head().operation_id.uuid()
                        != identity.native_head_operation
                    || owner.claim().current_head().revision != identity.native_head_revision
                    || self.recovered_bindings.get(&identity.obligation) != Some(&identity.binding)
                {
                    return Err(OperationError::State);
                }
                let mut files = owner
                    .directories
                    .lock()
                    .map_err(|_| OperationError::State)?;
                let pair = files.take().ok_or(OperationError::State)?;
                // Retain uncertainty permanently if either original close is
                // negative. No repeated numeric close, reopen, or S mutation.
                record.progress = ExitProgress::Unknown;
                let first = unsafe { libc::close(pair.0.into_raw_fd()) };
                let second = unsafe { libc::close(pair.1.into_raw_fd()) };
                if first != 0 || second != 0 {
                    return Err(OperationError::State);
                }
            }
        }
        record.progress = ExitProgress::Exited;
        record
            .selection
            .as_mut()
            .ok_or(OperationError::State)?
            .identity = identity;
        Ok(RetainedExitReady { identity })
    }
}
