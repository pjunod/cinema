//! Cleanup-only owner for one positively exited fixed software R. The same
//! reopened parent/child descriptors move from import into this role.
use super::super::response_memory::ResponseBudget;
use super::bootstrap::retirement::ProducerFiniteClosure;
use super::*;
use crate::resource_custody::recovery::PositivePreviousRootExit;

/// Nonclone closure of the imported scratch plus the exact old fixed process
/// domain. A DTO or recovered ledger row cannot construct this receipt.
pub(in crate::adaptive_resources) struct RecoveredRClosure {
    binding: ReservationBinding,
    _native: ProducerFiniteClosure,
    _old_domain: PositivePreviousRootExit,
}
impl std::fmt::Debug for RecoveredRClosure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecoveredRClosure")
            .field("binding", &self.binding)
            .finish_non_exhaustive()
    }
}
impl RecoveredRClosure {
    fn from_validated_native(
        native: ProducerFiniteClosure,
        old_domain: PositivePreviousRootExit,
    ) -> Self {
        let binding = native.binding();
        Self {
            binding,
            _native: native,
            _old_domain: old_domain,
        }
    }
    pub(in crate::adaptive_resources) fn binding(&self) -> ReservationBinding {
        self.binding
    }
}

impl NodeResourceRuntime {
    pub(in crate::adaptive_resources::runtime) async fn complete_recovered_r_preparation(
        &self,
        owner: &OwnedPreparation,
        member: Uuid,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let pending = {
            let bundle = owner.state.lock().await;
            let role = bundle.roles.first().ok_or(OperationError::State)?;
            if role.refunded {
                return Ok(());
            }
            role.transition.is_some()
        };
        if !pending {
            self.close_producer_finite_native(owner, 0, member, deadline)
                .await?;
        }
        self.settle_recovered_r_preparation(owner, deadline).await
    }

    pub(in crate::adaptive_resources::runtime) async fn settle_recovered_r_preparation(
        &self,
        owner: &OwnedPreparation,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        let bundle_id = bundle.id;
        let role = bundle.roles.get_mut(0).ok_or(OperationError::State)?;
        if role.refunded {
            return Ok(());
        }
        if role.transition.is_some() {
            return self.finish_transition(bundle_id, role).await;
        }
        let handle = role.handle.clone().ok_or(OperationError::State)?;
        let authority = handle
            .producer_recovery_authority()
            .ok_or(OperationError::State)?;
        let member = authority.scratch_member_id();
        let scratch = role.bootstrap.get(&member).ok_or(OperationError::State)?;
        let (scratch_plan, scratch_head) = scratch.producer_close_fact(role.binding, member)?;
        let plans = role.typed_members.as_ref().ok_or(OperationError::State)?;
        if !matches!(
            role.phase,
            Phase::UnknownHeld | Phase::Sealed | Phase::AcquisitionPossible
        ) || role.graph.is_some()
            || role.retirement.is_some()
            || role.bootstrap.len() != 1
            || role.binding != authority.binding()
            || role.durable_tag != authority.obligation_id()
            || plans.len() < 4
        {
            return Err(OperationError::State);
        }
        let operation_id = v2::Id::new(Uuid::new_v4())?;
        let close_id = operation_id;
        let mut facts = Vec::with_capacity(plans.len());
        let mut root_count = 0;
        let mut task_count = 0;
        let mut scratch_count = 0;
        for plan in plans {
            let member_id = v2::Id::new(plan.member_id())?;
            let fact = match plan {
                v2::MemberPlan::SourceV1 { source, .. } => v2::ClosedFact::SourceV1 {
                    holder_id: source.holder_id,
                    copies: source
                        .planned_copies
                        .iter()
                        .cloned()
                        .map(|copy| v2::CopyClosed::DomainExtinct {
                            copy,
                            operation_id: close_id,
                            domain: source.filesystem_domain.clone(),
                        })
                        .collect(),
                },
                v2::MemberPlan::LegacyV1 { planned }
                    if planned.kind == crate::resource_custody::MemberKind::Root
                        && planned.member_id == authority.root_member_id() =>
                {
                    root_count += 1;
                    v2::ClosedFact::LegacyV1 {
                        fact: crate::resource_custody::ClosedMemberFact {
                            member_id: planned.member_id,
                            adapter: planned.adapter.clone(),
                            adapter_version: planned.adapter_version,
                            domain: planned.domain.clone(),
                            object_identity: format!("{}:{}", authority.root_pid(), authority.root_creation_marker()),
                            evidence: "same-boot exact fixed software root and previous daemon positively extinct under installation lock".into(),
                        },
                    }
                }
                v2::MemberPlan::VodOutputTaskV1 { task, .. }
                    if task.root_member_id.uuid() == authority.root_member_id() =>
                {
                    task_count += 1;
                    v2::ClosedFact::VodOutputTaskDomainExtinctV1 {
                        task: task.clone(),
                        root_member_id: task.root_member_id,
                        root_pid: authority.root_pid(),
                        root_creation_marker: authority.root_creation_marker(),
                        operation_id: close_id,
                    }
                }
                v2::MemberPlan::ScratchAssetsV3 { slot, plan }
                    if slot.member_id.uuid() == member && plan == &scratch_plan =>
                {
                    scratch_count += 1;
                    v2::ClosedFact::ScratchAssetsV3 {
                        plan: plan.clone(),
                        exact_head: scratch_head.clone(),
                        operation_id: scratch_head.operation_id,
                    }
                }
                _ => return Err(OperationError::State),
            };
            facts.push(v2::MemberDisposition::Closed { member_id, fact });
        }
        if root_count != 1 || task_count != 1 || scratch_count != 1 {
            return Err(OperationError::State);
        }
        let settlement = v2::Settlement {
            version: 2,
            owner_id: v2::Id::new(authority.original_owner_id())?,
            graph_id: v2::Id::new(handle.graph_id())?,
            operation_id,
            members: facts,
        };
        // The original imported manifest/fold can exceed the settled R charge
        // once that charge is refunded. Move only its constructor-born compact
        // old-domain proof into a separate retained owner before native take.
        {
            let mut imports = self.recovered_r.lock().map_err(|_| OperationError::State)?;
            let mut domains = self
                .recovered_r_domains
                .lock()
                .map_err(|_| OperationError::State)?;
            if !domains.contains_key(&role.durable_tag) {
                let imported = imports
                    .remove(&role.durable_tag)
                    .ok_or(OperationError::State)?;
                let imported = match Arc::try_unwrap(imported) {
                    Ok(value) => value,
                    Err(shared) => {
                        imports.insert(role.durable_tag, shared);
                        return Err(OperationError::State);
                    }
                };
                let domain = match imported.into_closed_domain(authority, role.binding, member) {
                    Ok(domain) => domain,
                    Err(imported) => {
                        imports.insert(role.durable_tag, Arc::new(imported));
                        return Err(OperationError::State);
                    }
                };
                domains.insert(role.durable_tag, domain);
            }
        }
        // Validate both capabilities under their sole locks before native
        // take. On a failed take the compact positive domain remains stored.
        let mut domains = self
            .recovered_r_domains
            .lock()
            .map_err(|_| OperationError::State)?;
        let domain = match domains.entry(role.durable_tag) {
            std::collections::btree_map::Entry::Occupied(entry) => entry,
            std::collections::btree_map::Entry::Vacant(_) => return Err(OperationError::State),
        };
        if domain.get().root_member_id() != authority.root_member_id()
            || domain.get().pid() != authority.root_pid()
            || domain.get().creation_marker() != authority.root_creation_marker()
            || role.binding != authority.binding()
            || member != authority.scratch_member_id()
        {
            return Err(OperationError::State);
        }
        let native = role
            .bootstrap
            .get_mut(&member)
            .ok_or(OperationError::State)?
            .take_producer_close(role.binding, member)?;
        let domain = domain.remove();
        drop(domains);
        let closure = RecoveredRClosure::from_validated_native(native, domain);
        let receipt = PhysicalCloseReceipt::from_recovered_r(closure);
        let key = TransitionKey {
            handle,
            operation_id: operation_id.uuid(),
            expected_sequence: role.sequence,
        };
        role.bootstrap.clear();
        role.response_budget = None;
        role.transition = Some(Transition {
            operation: JournalOperation::transition(self.journal.change(
                key,
                Mutation::SettleTyped(Box::new(crate::resource_custody::PhysicalTypedSettlement {
                    receipt,
                    settlement,
                })),
                deadline,
            )),
            effect: Effect::Settle,
        });
        self.finish_transition(bundle_id, role).await
    }

    pub(in crate::adaptive_resources::runtime) fn install_recovered_r_preparation(
        &self,
        handle: ObligationHandle,
        sequence: i64,
    ) -> Result<OwnedPreparation, OperationError> {
        let authority = handle
            .producer_recovery_authority()
            .ok_or(OperationError::State)?;
        let binding = authority.binding();
        let imported = self
            .recovered_r
            .lock()
            .map_err(|_| OperationError::State)?
            .get(&handle.obligation_id())
            .cloned()
            .ok_or(OperationError::State)?;
        let claim = imported.claim();
        let manifest = claim.manifest().clone();
        let member = claim.scratch_member_id().uuid();
        let id = handle.obligation_id();
        if authority.current_owner_id() != self.owner.owner_id
            || claim.sequence() != sequence
            || claim.obligation_id() != id
            || claim.graph_id() != handle.graph_id()
            || self.recovered_bindings.get(&id) != Some(&binding)
        {
            return Err(OperationError::State);
        }
        let resources = {
            let state = self
                .ledger
                .inner
                .lock()
                .map_err(|_| OperationError::State)?;
            let entry = state
                .entries
                .get(&binding.reservation)
                .ok_or(OperationError::State)?;
            if entry.binding != binding
                || entry.durable != Some(id)
                || entry.resources != claim.resources()
                || !matches!(
                    entry.state,
                    ReservationState::UnknownHeld | ReservationState::Reaping
                )
            {
                return Err(OperationError::State);
            }
            entry.resources
        };
        // This cleanup-only budget has no live response slots. Its ceiling is
        // the original full R memory charge and grants no serving capability.
        let budget = ResponseBudget::new(
            usize::try_from(resources.peak_memory_bytes).map_err(|_| OperationError::State)?,
        )?;
        let role = OwnedRole {
            binding,
            durable_tag: id,
            members: Vec::new(),
            typed_members: Some(manifest.members),
            scratch: BTreeMap::new(),
            bootstrap: BTreeMap::new(),
            sources: BTreeMap::new(),
            finite_vod: None,
            retirement: None,
            response_budget: Some(budget),
            armed: BTreeSet::from([member]),
            invoked: BTreeSet::from([member]),
            roots: BTreeMap::new(),
            drains: BTreeMap::new(),
            closed_drains: BTreeMap::new(),
            graph: None,
            sealed: None,
            _cleanup: CleanupAuthority {
                ledger: self.ledger.clone(),
                binding,
            },
            handle: Some(handle.clone()),
            sequence,
            phase: Phase::UnknownHeld,
            transition: None,
            receipt: None,
            refunded: false,
        };
        let bundle = Bundle {
            id,
            family_ids: Vec::new(),
            retained: BTreeSet::new(),
            prepare: JournalOperation {
                id: authority.adoption_operation_id(),
                pending: None,
                result: None,
                rejected: None,
                acknowledged: true,
            },
            installed: false,
            roles: vec![role],
        };
        let state = {
            let mut book = self.operations.lock().map_err(|_| OperationError::State)?;
            if let Some(existing) = book.bundles.get(&id) {
                let bound = book.bindings.get(&binding).ok_or(OperationError::State)?;
                if bound.obligation_id() != id
                    || bound.graph_id() != handle.graph_id()
                    || bound.producer_recovery_authority().is_none_or(|saved| {
                        saved.binding() != binding
                            || saved.adoption_operation_id() != authority.adoption_operation_id()
                    })
                {
                    return Err(OperationError::State);
                }
                existing.clone()
            } else {
                if book.draining || book.bindings.contains_key(&binding) {
                    return Err(OperationError::State);
                }
                let state = Arc::new(AsyncMutex::new(bundle));
                book.bindings.insert(binding, handle.clone());
                book.bundles.insert(id, state.clone());
                state
            }
        };
        let owner = OwnedPreparation {
            ledger: self.ledger.id,
            id,
            state: state.clone(),
        };
        let mut bundle = state.try_lock().map_err(|_| OperationError::State)?;
        if bundle.id != id || bundle.roles.len() != 1 {
            return Err(OperationError::State);
        }
        let installed = bundle.installed;
        let role = bundle.roles.get_mut(0).ok_or(OperationError::State)?;
        if role.binding != binding
            || role.durable_tag != id
            || role.handle.as_ref().is_none_or(|held| {
                held.producer_recovery_authority().is_none_or(|saved| {
                    saved.binding() != binding
                        || saved.adoption_operation_id() != authority.adoption_operation_id()
                })
            })
            || role.sequence != sequence
            || role.refunded
            || role.transition.is_some()
        {
            return Err(OperationError::State);
        }
        if !installed {
            if !role.bootstrap.is_empty() {
                return Err(OperationError::State);
            }
            // Role and cleanup authority exist before consuming the sole
            // native File pair. A validation failure leaves them imported.
            let parts = authority
                .take_native(&imported)
                .map_err(|_| OperationError::State)?;
            role.bootstrap.insert(
                member,
                bootstrap::BootstrapMember::from_recovered_producer(parts),
            );
        } else if role.bootstrap.len() != 1 || !role.bootstrap.contains_key(&member) {
            return Err(OperationError::State);
        }
        bundle.installed = true;
        bundle.roles[0]
            ._cleanup
            .mark_reaping()
            .map_err(|_| OperationError::State)?;
        Ok(owner)
    }
}
