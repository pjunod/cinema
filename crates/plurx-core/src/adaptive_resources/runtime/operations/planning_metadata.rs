//! Final M release uses the original alias fence and original journal command.
//! P must have positively settled first; no native object is allowed in M.
use super::*;
use crate::resource_custody::PhysicalTypedSettlement;

impl NodeResourceRuntime {
    pub(in crate::adaptive_resources::runtime) fn register_planning_metadata(
        &self,
        owner: &OwnedPreparation,
        application_bytes: usize,
        book_structural_bytes: u64,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.try_lock().map_err(|_| OperationError::State)?;
        if bundle.roles.len() != 2 {
            return Err(OperationError::State);
        }
        let role = &mut bundle.roles[1];
        let [v2::MemberPlan::FixedProbeMetadataV1 { metadata, .. }] =
            role.typed_members.as_deref().ok_or(OperationError::State)?
        else {
            return Err(OperationError::State);
        };
        if role.planning_metadata.is_some()
            || role.finite_app.is_some()
            || u64::try_from(application_bytes).map_or(true, |bytes| {
                bytes
                    .checked_add(book_structural_bytes)
                    .is_none_or(|total| total >= metadata.retained_memory_bytes)
            })
        {
            return Err(OperationError::State);
        }
        let state = super::super::planning_probe::PlanningMetadataRole::new(
            metadata.clone(),
            role.binding,
            application_bytes,
            book_structural_bytes,
        );
        role.finite_app = Some(Arc::clone(&state.release.application));
        role.planning_metadata = Some(state);
        Ok(())
    }

    /// Logical attempt cleanup may retain its exact Close DTO/controller under
    /// the live application partition. Only native P and report aliases must
    /// have gone here; the M accounting stays held through final destruction.
    pub(in crate::adaptive_resources::runtime) async fn seal_planning_payload(
        &self,
        owner: &OwnedPreparation,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        if !bundle.installed || bundle.roles.len() != 2 || !bundle.roles[0].refunded {
            return Err(OperationError::State);
        }
        let metadata = bundle.roles[1]
            .planning_metadata
            .as_mut()
            .ok_or(OperationError::State)?;
        if !metadata.seal_after_last_payload() {
            return Err(OperationError::State);
        }
        Ok(())
    }

    pub(in crate::adaptive_resources::runtime) async fn queue_planning_metadata_close(
        &self,
        owner: &OwnedPreparation,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let needs_seal = {
            let mut bundle = owner.state.lock().await;
            if !bundle.installed || bundle.roles.len() != 2 || !bundle.roles[0].refunded {
                return Err(OperationError::State);
            }
            let role = &mut bundle.roles[1];
            if role.pending_finite_close.is_some()
                || role
                    .transition
                    .as_ref()
                    .is_some_and(|transition| transition.effect == Effect::Settle)
            {
                return Ok(());
            }
            let metadata = role
                .planning_metadata
                .as_mut()
                .ok_or(OperationError::State)?;
            if !metadata.seal_after_last_payload() || !metadata.release.application.released() {
                return Err(OperationError::State);
            }
            !matches!(role.phase, Phase::Sealed | Phase::UnknownHeld)
        };
        if needs_seal {
            self.seal_role(owner, 1, deadline).await?;
        }
        let mut bundle = owner.state.lock().await;
        let role = &mut bundle.roles[1];
        if role.pending_finite_close.is_some() {
            return Ok(());
        }
        if role.transition.is_some() || role.refunded {
            return Err(OperationError::State);
        }
        let metadata = role
            .planning_metadata
            .as_ref()
            .ok_or(OperationError::State)?;
        let [v2::MemberPlan::FixedProbeMetadataV1 {
            metadata: original, ..
        }] = role.typed_members.as_deref().ok_or(OperationError::State)?
        else {
            return Err(OperationError::State);
        };
        if original != &metadata.plan
            || !metadata.release.application.released()
            || !role.sources.is_empty()
            || !role.roots.is_empty()
            || !role.drains.is_empty()
            || !role.bootstrap.is_empty()
            || role.finite_vod.is_some()
            || role.copy_vod.is_some()
        {
            return Err(OperationError::State);
        }
        let handle = role.handle.clone().ok_or(OperationError::State)?;
        let graph = role
            .sealed
            .as_ref()
            .ok_or(OperationError::State)?
            .snapshot()
            .await;
        if graph.identity.generation != handle.graph_id()
            || !graph.sealed
            || graph.acquired
            || graph.roots != 0
            || graph.writers != 0
            || graph.source_files != 0
        {
            return Err(OperationError::State);
        }
        let operation_id = Uuid::new_v4();
        let settlement = v2::Settlement {
            version: 2,
            owner_id: v2::Id::new(self.owner.owner_id)?,
            graph_id: v2::Id::new(handle.graph_id())?,
            operation_id: v2::Id::new(operation_id)?,
            members: vec![v2::MemberDisposition::Closed {
                member_id: metadata.plan.member_id,
                fact: v2::ClosedFact::FixedProbeMetadataV1 {
                    metadata: metadata.plan.clone(),
                    // Selected with the original M plan, before any report
                    // or coordinator allocation entered its charged lifetime.
                    release_operation_id: metadata.plan.metadata_id,
                },
            }],
        };
        let key = TransitionKey {
            handle,
            operation_id,
            expected_sequence: role.sequence,
        };
        let closure = metadata.release.take_close()?;
        // No await or fallible work after the sole one-shot capability. The
        // exact rejected/unknown command stays here; finalization destroys
        // this whole bundle before the one matching M ledger release.
        role.pending_finite_close = Some(PendingFiniteClose {
            key,
            mutation: Mutation::SettleTyped(Box::new(PhysicalTypedSettlement {
                receipt: PhysicalCloseReceipt::from_planning_metadata(closure),
                settlement,
            })),
            effect: Effect::Settle,
        });
        Ok(())
    }
}
