//! Live rolling-prefix removal through the original held child directory.
//! A failed or unknown native unlink remains in this same charged member.
use super::*;
use native::cleanup::{CleanupCell, CleanupFact};
use v2::scratch::{AssetEventV1, AssetIoV1, AssetObservationV1, NativeOutcomeV1, OutputKey};

#[cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "Live copy prefix removal is currently entered only on Linux; retain the complete original removal owner and portable close/join path on other platforms."
    )
)]
pub(super) struct LiveRemoval {
    index: u32,
    io: v2::Id,
    observe: v2::Id,
    join: v2::Id,
    file: v2::Id,
    native: v2::NativeObject,
    cell: CleanupCell,
    armed: bool,
    launched: bool,
    joined: bool,
}

impl LiveRemoval {
    pub(super) fn launched(&self) -> bool {
        self.launched
    }
    pub(super) async fn join_registered(&mut self) -> Result<(), OperationError> {
        self.cell.join_registered().await
    }
}

impl NodeResourceRuntime {
    /// Remove one expired published segment. The caller's expiry decision is
    /// not native proof: the original reader ledger, exact file identity,
    /// durable Arm, registered unlink job and Observe all remain authoritative.
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Live copy prefix removal is currently entered only on Linux; retain the complete original removal owner and portable close/join path on other platforms."
        )
    )]
    pub(in crate::adaptive_resources::runtime) async fn remove_copy_segment(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        segment: u32,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        self.eligible(&bundle)?;
        let bundle_id = bundle.id;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        if role.refunded || !matches!(role.phase, Phase::AcquisitionPossible | Phase::Sealed) {
            return Err(OperationError::State);
        }
        let bootstrap = role
            .bootstrap
            .get_mut(&member)
            .ok_or(OperationError::State)?;
        if bootstrap.prefix_removal.is_none() {
            if role.transition.is_some()
                || bootstrap.change.is_some()
                || bootstrap.retirement.is_some()
                || bootstrap.next_arm.is_some()
                || !bootstrap.native_io_ready()
            {
                return Err(OperationError::State);
            }
            let fold = bootstrap.fold.as_ref().ok_or(OperationError::State)?;
            let state = fold.state();
            if state.asset.lifecycle != v2::scratch::ScratchLifecycleV1::Open
                || state.asset.totals.pending_jobs != 0
            {
                return Err(OperationError::State);
            }
            let Some(file) = fold
                .files()
                .iter()
                .find(|file| file.published == Some(OutputKey::Segment { index: segment }))
            else {
                // A known previous removal may be retried by the maintenance
                // caller. Absence is only an idempotent result, not proof of
                // the original operation's physical close.
                return Ok(());
            };
            if fold.has_reader_for_file(file.file_id) || !file.held_copy_ids.is_empty() {
                return Err(OperationError::State);
            }
            let child = state
                .asset
                .directory
                .as_ref()
                .ok_or(OperationError::State)?;
            bootstrap.prefix_removal = Some(LiveRemoval {
                index: segment,
                io: v2::Id::new(Uuid::new_v4())?,
                observe: v2::Id::new(Uuid::new_v4())?,
                join: v2::Id::new(Uuid::new_v4())?,
                file: file.file_id,
                native: file.native_object.clone(),
                cell: CleanupCell::remove_owned_file(file, child)?,
                armed: false,
                launched: false,
                joined: false,
            });
        }
        if bootstrap
            .prefix_removal
            .as_ref()
            .is_none_or(|removal| removal.index != segment)
        {
            return Err(OperationError::State);
        }
        for _ in 0..5 {
            let (io, observe, join, file, native, armed, launched, joined) = {
                let removal = role
                    .bootstrap
                    .get(&member)
                    .and_then(|b| b.prefix_removal.as_ref())
                    .ok_or(OperationError::State)?;
                (
                    removal.io,
                    removal.observe,
                    removal.join,
                    removal.file,
                    removal.native.clone(),
                    removal.armed,
                    removal.launched,
                    removal.joined,
                )
            };
            if !armed {
                let head = role
                    .bootstrap
                    .get(&member)
                    .and_then(|b| b.fold.as_ref())
                    .ok_or(OperationError::State)?
                    .state()
                    .asset
                    .head;
                if head.is_none_or(|head| head.operation_id != io) {
                    if role.transition.is_none()
                        && role
                            .bootstrap
                            .get(&member)
                            .is_some_and(|b| b.change.is_none())
                    {
                        native::publication::queue_output_change(
                            &self.journal,
                            role,
                            member,
                            io.uuid(),
                            AssetEventV1::ArmIo {
                                io_id: io,
                                operation: AssetIoV1::RemoveOwned {
                                    file_id: file,
                                    expected_native_object: Some(native),
                                },
                            },
                            deadline,
                        )?;
                    }
                    super::resume_change(&self.journal, role, member, deadline)?;
                    self.finish_transition(bundle_id, role).await?;
                }
                role.bootstrap
                    .get_mut(&member)
                    .and_then(|b| b.prefix_removal.as_mut())
                    .ok_or(OperationError::State)?
                    .armed = true;
                continue;
            }
            if !launched {
                let bootstrap = role
                    .bootstrap
                    .get_mut(&member)
                    .ok_or(OperationError::State)?;
                let state = bootstrap
                    .fold
                    .as_ref()
                    .ok_or(OperationError::State)?
                    .state();
                let baseline = [
                    state
                        .bootstrap
                        .current
                        .as_ref()
                        .map(|directory| &directory.observed),
                    state
                        .bootstrap
                        .parent
                        .as_ref()
                        .map(|directory| &directory.observed),
                    state.asset.directory.as_ref(),
                ];
                let removal = bootstrap
                    .prefix_removal
                    .as_mut()
                    .ok_or(OperationError::State)?;
                let entry = removal.cell.launch(
                    bootstrap
                        .native_root
                        .as_mut()
                        .ok_or(OperationError::State)?,
                    &baseline,
                )?;
                removal.launched = true;
                let _ = entry.send(());
                continue;
            }
            if !joined {
                let removal = role
                    .bootstrap
                    .get_mut(&member)
                    .and_then(|b| b.prefix_removal.as_mut())
                    .ok_or(OperationError::State)?;
                removal.cell.join_registered().await?;
                removal.joined = true;
                continue;
            }
            let fact = role
                .bootstrap
                .get(&member)
                .and_then(|b| b.prefix_removal.as_ref())
                .and_then(|r| r.cell.fact())
                .ok_or(OperationError::State)?;
            let (outcome, observation) = match fact {
                CleanupFact::Asset {
                    outcome,
                    observation,
                } => (outcome, observation),
                _ => return Err(OperationError::State),
            };
            let head = role
                .bootstrap
                .get(&member)
                .and_then(|b| b.fold.as_ref())
                .ok_or(OperationError::State)?
                .state()
                .asset
                .head;
            if head.is_none_or(|head| head.operation_id != observe) {
                if role.transition.is_none()
                    && role
                        .bootstrap
                        .get(&member)
                        .is_some_and(|b| b.change.is_none())
                {
                    native::publication::queue_output_change(
                        &self.journal,
                        role,
                        member,
                        observe.uuid(),
                        AssetEventV1::ObserveIo {
                            io_id: io,
                            native_join_operation_id: join,
                            outcome: outcome.clone(),
                            observation: observation.clone(),
                        },
                        deadline,
                    )?;
                }
                super::resume_change(&self.journal, role, member, deadline)?;
                self.finish_transition(bundle_id, role).await?;
            }
            let bootstrap = role
                .bootstrap
                .get_mut(&member)
                .ok_or(OperationError::State)?;
            let removal = bootstrap
                .prefix_removal
                .as_mut()
                .ok_or(OperationError::State)?;
            removal.cell.acknowledge_after_observe(
                bootstrap
                    .native_root
                    .as_mut()
                    .ok_or(OperationError::State)?,
            )?;
            if outcome == NativeOutcomeV1::Completed
                && observation == (AssetObservationV1::Removed { file_id: file })
            {
                bootstrap.prefix_removal = None;
                return Ok(());
            }
            if outcome == NativeOutcomeV1::NeverEntered
                && observation == AssetObservationV1::NoAcquisition
            {
                bootstrap.prefix_removal = None;
            }
            // Any entered ambiguous unlink keeps its exact original cell,
            // operation and full R charge. No second deletion is armed.
            return Err(OperationError::State);
        }
        Err(OperationError::State)
    }
}
