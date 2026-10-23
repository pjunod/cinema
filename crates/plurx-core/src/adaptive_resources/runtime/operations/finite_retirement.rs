//! One retained native-output owner across the journal's R→S boundary. The
//! provisional target stays under the full original R charge until known COMMIT.
use super::*;
use crate::adaptive_resources::physical_graph::finite_vod::FiniteVodProcessClosure;
use crate::adaptive_resources::retained::RetainedSwap;
use crate::adaptive_resources::PhysicalRetirementReceipt;
use crate::resource_custody::{CommittedRetirementReceipt, MemberKind, PhysicalAssetRetirement};
use v2::scratch::bootstrap::frozen_snapshot_ref;

/// Constructed only after original producer/source closure and an infallible
/// move of every live bootstrap owner into the retained runtime slot.
pub(in crate::adaptive_resources) struct FiniteRetirementClosure {
    source: ReservationBinding,
    target: ReservationBinding,
    _process: FiniteVodProcessClosure,
}
impl FiniteRetirementClosure {
    pub(in crate::adaptive_resources) fn into_bindings(
        self,
    ) -> (ReservationBinding, ReservationBinding) {
        (self.source, self.target)
    }
}

pub(super) struct FiniteRetirement {
    source: ObligationHandle,
    target: ObligationHandle,
    source_member: Uuid,
    target_member: Uuid,
    manifest: v2::Manifest,
    swap: RetainedSwap,
    native: Option<bootstrap::BootstrapMember>,
    operation: Option<JournalOperation>,
    committed: Option<CommittedRetirementReceipt>,
    installed: bool,
}

impl FiniteRetirement {
    pub(in crate::adaptive_resources::runtime) fn resolve_source_member(
        &self,
        source: Uuid,
    ) -> Option<Uuid> {
        (self.installed && self.source_member == source).then_some(self.target_member)
    }
    pub(super) fn reader_progress(&self, lease: Uuid) -> Option<Arc<tokio::sync::Notify>> {
        self.native
            .as_ref()
            .and_then(|native| bootstrap::readers::reader_progress(native, lease))
    }
}

impl NodeResourceRuntime {
    /// Continue the same retirement, including after a cancelled/unknown wait.
    /// Return a selection only after the journal and ledger agree on S.
    pub(in crate::adaptive_resources::runtime) async fn retire_finite_output(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        source_member: Uuid,
        deadline: Instant,
    ) -> Result<Uuid, OperationError> {
        self.validate_preparation(owner)?;
        let needs_prepare = {
            let bundle = owner.state.lock().await;
            let role = bundle.roles.get(index).ok_or(OperationError::State)?;
            role.retirement.is_none()
        };
        if needs_prepare {
            self.join_finite_producer(owner, index).await?;
            self.freeze_finite_output(owner, index, source_member, deadline)
                .await?;
            let needs_seal = {
                let bundle = owner.state.lock().await;
                bundle.roles.get(index).ok_or(OperationError::State)?.phase != Phase::Sealed
            };
            if needs_seal {
                self.seal_role(owner, index, deadline).await?;
            }
        }
        let mut bundle = owner.state.lock().await;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        if role.retirement.is_none() {
            self.prepare_finite_retirement(role, source_member, deadline)?;
        }
        self.finish_finite_retirement(role, source_member, deadline)
            .await
    }

    fn prepare_finite_retirement(
        &self,
        role: &mut OwnedRole,
        source_member: Uuid,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        if role.refunded
            || role.transition.is_some()
            || role.retirement.is_some()
            || role.phase != Phase::Sealed
            || role.graph.is_some()
            || role.bootstrap.len() != 1
            || !role.scratch.is_empty()
            || !role.drains.is_empty()
            || !role.closed_drains.is_empty()
            || Instant::now() >= deadline
        {
            return Err(OperationError::State);
        }
        let source = role.handle.clone().ok_or(OperationError::State)?;
        let finite = role.finite_vod.as_ref().ok_or(OperationError::State)?;
        let process = finite
            .process_closure
            .as_ref()
            .ok_or(OperationError::State)?;
        if finite.source_drifted
            || !finite.producer_joined
            || !finite.retiring
            || !process.matches(role.sealed.as_ref().ok_or(OperationError::State)?)
            || process.generation() != source.graph_id()
        {
            return Err(OperationError::State);
        }
        let planned = role.typed_members.as_ref().ok_or(OperationError::State)?;
        let (source_slot, plan) = planned
            .iter()
            .find_map(|member| match member {
                v2::MemberPlan::ScratchAssetsV3 { slot, plan }
                    if slot.member_id.uuid() == source_member =>
                {
                    Some((slot, plan))
                }
                _ => None,
            })
            .ok_or(OperationError::State)?;
        let bootstrap = role
            .bootstrap
            .get(&source_member)
            .ok_or(OperationError::State)?;
        // Runtime payload accounting is separate from frozen durable totals.
        // The active unknown producer claim remains held throughout cloning;
        // only a fold fitting S's explicit resident partition can transfer.
        let current_inventory_bytes = bootstrap.resident_inventory_bytes()?;
        if current_inventory_bytes > plan.limits.resident_memory_bytes {
            return Err(OperationError::State);
        }
        let full = bootstrap.frozen_inventory()?;
        let frozen_inventory_bytes = full.resident_inventory_bytes()?;
        if frozen_inventory_bytes > plan.limits.resident_memory_bytes {
            return Err(OperationError::State);
        }
        let reference = frozen_snapshot_ref(plan, &full)?;
        // The first adapter has no response/native reader owner. Never invent
        // a retained reader from a serialized lease identifier.
        if full.obligation_id.uuid() != source.obligation_id()
            || full.member_id != source_slot.member_id
        {
            return Err(OperationError::State);
        }
        let source_resources = {
            let state = self
                .ledger
                .inner
                .lock()
                .map_err(|_| OperationError::State)?;
            let entry = state
                .entries
                .get(&role.binding.reservation)
                .ok_or(OperationError::State)?;
            if entry.binding != role.binding || entry.durable != Some(role.durable_tag) {
                return Err(OperationError::State);
            }
            entry.resources
        };
        let response_memory_bytes = role
            .response_budget
            .as_ref()
            .ok_or(OperationError::State)?
            .limit_bytes()?;
        // Keep every future output/reader subpartition, the configured-root
        // bootstrap allowance, the actual inventory payload, the complete
        // response budget, and the existing persistent metadata allowance.
        // These are known requested capacities, not a whole-RSS estimate.
        const PERSISTENT_METADATA_BYTES: u64 = 64 * 1024;
        let retained_memory_bytes = plan
            .limits
            .resident_memory_bytes
            .checked_add(plan.bootstrap().bounds.resident_memory_bytes)
            .and_then(|bytes| {
                bytes.checked_add(current_inventory_bytes.max(frozen_inventory_bytes))
            })
            .and_then(|bytes| bytes.checked_add(response_memory_bytes))
            .and_then(|bytes| bytes.checked_add(PERSISTENT_METADATA_BYTES))
            .ok_or(OperationError::State)?;
        let retained_handles = u64::from(
            full.state
                .asset
                .totals
                .handles_reserved
                .max(plan.limits.live_handles),
        );
        let retained_resources = ResourceVector {
            cpu_ns_per_second: 0,
            peak_memory_bytes: retained_memory_bytes,
            handles: retained_handles,
            processes: 0,
            hardware_slots: 0,
            tuner_slots: 0,
            scratch_bytes: full.state.asset.totals.charged_storage_bytes,
        };
        // The original R charge must cover every S dimension. An incomplete
        // profile refuses transfer and preserves the original producer claim.
        if source_resources.checked_sub(retained_resources).is_none() {
            return Err(OperationError::State);
        }
        let target_durable = Uuid::new_v4();
        let swap = self.ledger.prepare_retained_swap(
            role.binding,
            role.durable_tag,
            target_durable,
            retained_resources,
        )?;
        let target = self.journal.retirement_target(
            swap.target_durable(),
            Uuid::new_v4(),
            swap.target_binding(),
        )?;
        let target_member = v2::Id::new(Uuid::new_v4())?;
        let operation_id = v2::Id::new(Uuid::new_v4())?;
        let target_manifest = v2::Manifest {
            version: 2,
            owner_id: v2::Id::new(self.owner.owner_id)?,
            graph_id: v2::Id::new(target.graph_id())?,
            attempt_id: None,
            members: vec![v2::MemberPlan::ScratchAssetsV3 {
                slot: v2::MemberSlot {
                    member_id: target_member,
                    owned_by: None,
                    depends_on: vec![],
                    protected_paths: source_slot.protected_paths.clone(),
                },
                plan: plan.clone(),
            }],
        };
        let transfer = v2::MemberTransfer {
            source_member_id: source_slot.member_id,
            target_member_id: target_member,
            identity: v2::TransferIdentity::ScratchAssetsV3 {
                plan: plan.clone(),
                snapshot: reference.clone(),
            },
        };
        let retained = v2::RetainedAssetsV2 {
            version: 2,
            asset_id: plan.asset_id,
            rendition_id: plan.rendition_id.clone(),
            generation_epoch: plan.generation_epoch,
            producer_obligation_id: v2::Id::new(source.obligation_id())?,
            producer_graph_id: v2::Id::new(source.graph_id())?,
            transfer_operation_id: operation_id,
            target_obligation_id: v2::Id::new(target.obligation_id())?,
            target_graph_id: v2::Id::new(target.graph_id())?,
            target_owner_id: v2::Id::new(self.owner.owner_id)?,
            transfer: transfer.clone(),
            reader_ids: full.readers.iter().map(|reader| reader.lease_id).collect(),
            reconciliation: v2::Reconciliation::LiveOwned,
        };
        v2::validate_retained_resources_v2(&target_manifest, &retained, &full, retained_resources)?;
        let members = closed_members(
            role,
            process,
            source_member,
            &transfer,
            &target,
            plan.asset_id,
            operation_id,
        )?;
        let settlement = v2::Settlement {
            version: 2,
            owner_id: v2::Id::new(self.owner.owner_id)?,
            graph_id: v2::Id::new(source.graph_id())?,
            operation_id,
            members,
        };
        let prepared = bootstrap.prepare_retained_transfer(
            &full,
            &reference,
            plan,
            v2::Id::new(target.obligation_id())?,
            target_member,
        )?;
        if prepared.resident_inventory_bytes()?
            > current_inventory_bytes.max(frozen_inventory_bytes)
        {
            // The staged target fold is an actual post-transfer allocation.
            // Refuse before native movement if cloning exceeded S's payload
            // partition; the entire original R charge remains held.
            return Err(OperationError::State);
        }
        let key = TransitionKey {
            handle: source.clone(),
            operation_id: operation_id.uuid(),
            expected_sequence: role.sequence,
        };
        let target_binding = swap.target_binding();
        // All validation and allocations precede taking the same native cells.
        // The provisional S slot is installed before the physical receipt and
        // journal command. No await can strand its owner between those steps.
        role.retirement = Some(FiniteRetirement {
            source,
            target: target.clone(),
            source_member,
            target_member: target_member.uuid(),
            manifest: target_manifest.clone(),
            swap,
            native: None,
            operation: None,
            committed: None,
            installed: false,
        });
        let staged = prepared.move_to_retained(
            role.bootstrap
                .get_mut(&source_member)
                .expect("validated source member"),
        );
        role.retirement
            .as_mut()
            .expect("installed provisional S")
            .native = Some(staged);
        let process = role
            .finite_vod
            .as_mut()
            .expect("validated finite owner")
            .process_closure
            .take()
            .expect("validated original process closure");
        let receipt = PhysicalRetirementReceipt::from_finite_vod(FiniteRetirementClosure {
            source: role.binding,
            target: target_binding,
            _process: process,
        });
        let proof = PhysicalAssetRetirement {
            receipt,
            settlement,
            retained,
            target_manifest,
            target_resources: retained_resources,
            target,
        };
        role.retirement
            .as_mut()
            .expect("installed provisional S")
            .operation = Some(JournalOperation::transition(self.journal.change(
            key,
            Mutation::RetireWithAssets(Box::new(proof)),
            deadline,
        )));
        Ok(())
    }

    async fn finish_finite_retirement(
        &self,
        role: &mut OwnedRole,
        source_member: Uuid,
        deadline: Instant,
    ) -> Result<Uuid, OperationError> {
        let retirement = role.retirement.as_mut().ok_or(OperationError::State)?;
        if retirement.source_member != source_member {
            return Err(OperationError::State);
        }
        if retirement.installed {
            return Ok(retirement.target_member);
        }
        let operation = retirement.operation.as_mut().ok_or(OperationError::State)?;
        if operation.pending.is_none() && !matches!(operation.result, Some(Ok(_))) {
            operation.retry(&self.journal, deadline)?;
        }
        let OperationResult::RetiredWithAssets { ack, target } = operation.resolve().await? else {
            return Err(OperationError::State);
        };
        if ack.operation_id.uuid() != operation.id
            || ack.sequence <= role.sequence
            || ack.source_obligation_id.uuid() != retirement.source.obligation_id()
            || ack.source_graph_id.uuid() != retirement.source.graph_id()
            || ack.target_obligation_id.uuid() != retirement.target.obligation_id()
            || ack.target_graph_id.uuid() != retirement.target.graph_id()
            || target.obligation_id() != retirement.target.obligation_id()
            || target.graph_id() != retirement.target.graph_id()
            || retirement.native.is_none()
        {
            return Err(OperationError::State);
        }
        if retirement.committed.is_none() {
            retirement.committed = Some(self.journal.take_retirement_receipt(
                operation.id,
                &retirement.source,
                &retirement.target,
            )?);
        }
        // The journal has positively committed R→S and this continuation
        // retains its original operation, target native cell and receipt.
        // Release producer-only heap/native owners while R is STILL charged;
        // a failed ledger swap can retry with the same committed capability.
        role.members.clear();
        role.typed_members = None;
        role.armed.clear();
        role.invoked.clear();
        role.sources.clear();
        role.roots.clear();
        role.sealed = None;
        role.finite_vod = None;
        role.bootstrap.clear();
        // The original post-COMMIT capability is retained before any ledger or
        // operation-book lock. Failure preserves it for this exact continuation.
        let mut book = self.operations.lock().map_err(|_| OperationError::State)?;
        if !book.closing.contains(&role.binding)
            || book
                .bindings
                .get(&role.binding)
                .is_none_or(|h| h.obligation_id() != retirement.source.obligation_id())
            || book
                .bindings
                .contains_key(&retirement.swap.target_binding())
        {
            return Err(OperationError::State);
        }
        let cleanup = self.ledger.commit_retained_swap(
            &mut retirement.swap,
            retirement
                .committed
                .as_ref()
                .expect("retained committed receipt"),
        )?;
        // Single ledger replacement succeeded. Everything below is infallible
        // in-memory rebinding under the original bundle + operation-book locks.
        book.bindings.remove(&role.binding);
        book.closing.remove(&role.binding);
        role.binding = retirement.swap.target_binding();
        role.durable_tag = retirement.swap.target_durable();
        book.bindings
            .insert(role.binding, retirement.target.clone());
        role.bootstrap.clear();
        role.bootstrap.insert(
            retirement.target_member,
            retirement
                .native
                .take()
                .expect("validated retained native member"),
        );
        role.members.clear();
        role.typed_members = Some(std::mem::take(&mut retirement.manifest.members));
        role.armed.clear();
        role.armed.insert(retirement.target_member);
        role.invoked.clear();
        role.invoked.insert(retirement.target_member);
        role.sources.clear();
        role.roots.clear();
        role.sealed = None;
        role.finite_vod = None;
        role._cleanup = cleanup;
        role.handle = Some(retirement.target.clone());
        role.sequence = ack.sequence;
        role.phase = Phase::Sealed;
        retirement.installed = true;
        bootstrap::readers::notify_progress(
            role.bootstrap
                .get(&retirement.target_member)
                .expect("installed retained member"),
        );
        Ok(retirement.target_member)
    }
}

fn closed_members(
    role: &OwnedRole,
    process: &FiniteVodProcessClosure,
    scratch: Uuid,
    transfer: &v2::MemberTransfer,
    target: &ObligationHandle,
    asset: v2::Id,
    operation: v2::Id,
) -> Result<Vec<v2::MemberDisposition>, OperationError> {
    let planned = role.typed_members.as_ref().ok_or(OperationError::State)?;
    if planned.len() != role.sources.len() + 3 || role.roots.len() != 1 {
        return Err(OperationError::State);
    }
    let mut result = Vec::with_capacity(planned.len());
    let close_id = v2::Id::new(process.close_operation_id())?;
    for member in planned {
        let member_id = v2::Id::new(member.member_id())?;
        let fact = match member {
            v2::MemberPlan::ScratchAssetsV3 { .. } if member.member_id() == scratch => {
                result.push(v2::MemberDisposition::Transferred {
                    transfer: transfer.clone(),
                    target_obligation_id: v2::Id::new(target.obligation_id())?,
                    target_graph_id: v2::Id::new(target.graph_id())?,
                    asset_id: asset,
                    operation_id: operation,
                });
                continue;
            }
            v2::MemberPlan::SourceV1 { .. } => role
                .sources
                .get(&member.member_id())
                .ok_or(OperationError::State)?
                .finite_closed_fact(process)?,
            v2::MemberPlan::LegacyV1 { planned }
                if planned.member_id == process.root_member()
                    && planned.kind == MemberKind::Root
                    && planned.adapter == Adapter::ProcessRoot =>
            {
                let Metric::Known(identity) = process.root().process else {
                    return Err(OperationError::State);
                };
                v2::ClosedFact::LegacyV1 { fact: crate::resource_custody::ClosedMemberFact {
                    member_id: planned.member_id, adapter: planned.adapter.clone(), adapter_version: planned.adapter_version,
                    domain: planned.domain.clone(), object_identity: format!("{}:{}", identity.pid, identity.start_marker),
                    evidence: "original trusted fixed software AVC root waited; writer and pipes joined; original Child/ChildJob disposed".into(),
                }}
            }
            v2::MemberPlan::VodOutputTaskV1 { task, .. }
                if task.root_member_id.uuid() == process.root_member()
                    && task.graph_id.uuid() == process.generation()
                    && task.inventory.pending_io.is_empty()
                    && task.inventory.buffers.len() == 1
                    && task.inventory.readers.len() == 2
                    && task.inventory.buffers[0].handles == 0
                    && task
                        .inventory
                        .readers
                        .iter()
                        .all(|r| r.handles == 1 && r.memory_bytes == 0)
                    && task
                        .inventory
                        .readers
                        .iter()
                        .any(|r| r.owner_id == task.stdout_owner_id)
                    && task
                        .inventory
                        .readers
                        .iter()
                        .any(|r| r.owner_id == task.diagnostic_owner_id) =>
            {
                let owners = task
                    .inventory
                    .buffers
                    .iter()
                    .chain(&task.inventory.readers)
                    .map(|owner| v2::OwnerClosed {
                        owner_id: owner.owner_id,
                        operation_id: close_id,
                    })
                    .collect();
                v2::ClosedFact::VodOutputTaskV1 {
                    task: task.clone(),
                    join_operation_id: close_id,
                    owners,
                }
            }
            _ => return Err(OperationError::State),
        };
        result.push(v2::MemberDisposition::Closed { member_id, fact });
    }
    Ok(result)
}
