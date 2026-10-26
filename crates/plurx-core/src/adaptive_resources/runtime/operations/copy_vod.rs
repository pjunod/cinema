//! Original one-input rolling copy root and parser registration. The copied
//! recipe selects an immutable graph; no caller future, raw File, or parser
//! result may authorize source/scratch settlement.
use super::*;
use crate::adaptive_resources::physical_graph::copy_vod::{
    CopyFixedImage, CopyProcessClosure, CopyTaskSummary, CopyWork, GraphCopyTask,
};
use crate::adaptive_resources::physical_graph::finite_vod::FiniteVodNeverRootClosure;
use crate::adaptive_resources::runtime::copy_vod::CopyPipeCommand;
use std::process::Stdio;

pub(super) struct CopyLaunch {
    pub(super) root_member: Uuid,
    pub(super) task_member: Uuid,
    pub(super) acquisition_id: Uuid,
    pub(super) source_members: [Uuid; 2],
    pub(super) command: CopyPipeCommand,
    pub(super) work: Option<CopyWork>,
    pub(super) entered: bool,
    pub(super) task: Option<GraphCopyTask>,
    pub(super) task_observation: Uuid,
    pub(super) task_observed_committed: bool,
    // Successful delivery of the ORIGINAL parser latch, not parser/native
    // entry or cleanup evidence. Used only for same-task release replay.
    start_released: bool,
    pub(super) fixed_image: Option<CopyFixedImage>,
    pub(super) process_closure: Option<CopyProcessClosure>,
    pub(super) never_root_closure: Option<FiniteVodNeverRootClosure>,
    pub(super) terminal: Option<CopyTaskSummary>,
    pub(super) source_drifted: bool,
    pub(super) cleanup_requested: Arc<std::sync::atomic::AtomicBool>,
    pub(super) progress: Arc<tokio::sync::Notify>,
}

impl NodeResourceRuntime {
    pub(in crate::adaptive_resources::runtime) async fn copy_produced_segment_lengths(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
    ) -> Result<Vec<(u32, u64)>, OperationError> {
        self.validate_preparation(owner)?;
        let bundle = owner.state.lock().await;
        let role = bundle.roles.get(index).ok_or(OperationError::State)?;
        if role.refunded || role.copy_vod.is_none() {
            return Err(OperationError::State);
        }
        let planned_max = role
            .typed_members
            .as_ref()
            .ok_or(OperationError::State)?
            .iter()
            .find_map(|planned| match planned {
                v2::MemberPlan::ScratchAssetsV3 { slot, plan }
                    if slot.member_id.uuid() == member =>
                {
                    Some(plan.outputs.segment_end_exclusive)
                }
                _ => None,
            })
            .ok_or(OperationError::State)?;
        if planned_max == 0 || planned_max > 65_536 {
            return Err(OperationError::State);
        }
        let fold = role
            .bootstrap
            .get(&member)
            .and_then(|bootstrap| bootstrap.fold())
            .ok_or(OperationError::State)?;
        let mut lengths = Vec::new();
        let max = usize::try_from(planned_max).map_err(|_| OperationError::State)?;
        lengths
            .try_reserve_exact(fold.files().len().min(max))
            .map_err(|_| OperationError::State)?;
        if lengths.capacity() > max {
            return Err(OperationError::State);
        }
        for file in fold.files() {
            if file.temporary {
                continue;
            }
            if let Some(v2::scratch::OutputKey::Segment { index }) = file.published.as_ref() {
                if u64::from(*index) >= planned_max || file.logical_len == 0 {
                    return Err(OperationError::State);
                }
                lengths.push((*index, file.logical_len));
            }
        }
        lengths.sort_unstable_by_key(|(index, _)| *index);
        if lengths.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(OperationError::State);
        }
        Ok(lengths)
    }

    pub(in crate::adaptive_resources::runtime) async fn wait_copy_parser_result(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        deadline: Instant,
    ) -> Result<CopyTaskSummary, OperationError> {
        self.validate_preparation(owner)?;
        let waiter = {
            let bundle = owner.state.lock().await;
            let role = bundle.roles.get(index).ok_or(OperationError::State)?;
            if role.refunded
                || !role
                    .copy_vod
                    .as_ref()
                    .is_some_and(|copy| copy.task_observed_committed)
            {
                return Err(OperationError::State);
            }
            let task = role
                .copy_vod
                .as_ref()
                .and_then(|copy| copy.task)
                .ok_or(OperationError::State)?;
            role.graph
                .as_ref()
                .ok_or(OperationError::State)?
                .copy_result_waiter(task)
                .await?
        };
        waiter.wait(deadline).await.map_err(Into::into)
    }

    pub(in crate::adaptive_resources::runtime) async fn validate_copy_sources(
        &self,
        owner: &OwnedPreparation,
        index: usize,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        let copy = role.copy_vod.as_mut().ok_or(OperationError::State)?;
        if role.refunded || copy.source_drifted || !copy.entered || copy.task.is_none() {
            return Err(OperationError::State);
        }
        for member in copy.source_members {
            let result = role
                .sources
                .get(&member)
                .ok_or(OperationError::State)?
                .unchanged_for_publication();
            if let Err(error) = result {
                copy.source_drifted = true;
                return Err(error);
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::adaptive_resources::runtime) fn queue_copy_vod(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        root_member: Uuid,
        task_member: Uuid,
        acquisition_id: Uuid,
        source_members: [Uuid; 2],
        command: CopyPipeCommand,
        work: Option<CopyWork>,
        app_state: Arc<crate::adaptive_resources::runtime::finite_lifetime::FiniteAppState>,
        cleanup_requested: Arc<std::sync::atomic::AtomicBool>,
        progress: Arc<tokio::sync::Notify>,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.try_lock().map_err(|_| OperationError::State)?;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        if role.refunded
            || role.copy_vod.is_some()
            || role.finite_vod.is_some()
            || role.graph.is_none()
            || acquisition_id.is_nil()
            || source_members[0].is_nil()
            || source_members[1].is_nil()
            || source_members[0] == source_members[1]
            || role.typed_members.as_ref().is_none_or(|members| {
                !members.iter().any(|member| {
                    matches!(member, v2::MemberPlan::VodOutputTaskV1 { slot, task }
                    if slot.member_id.uuid() == task_member
                        && task.root_member_id.uuid() == root_member
                        && task.actor == v2::VodActor::RollingCopyV1)
                })
            })
        {
            return Err(OperationError::State);
        }
        role.copy_vod = Some(CopyLaunch {
            root_member,
            task_member,
            acquisition_id,
            source_members,
            command,
            work,
            entered: false,
            task: None,
            task_observation: Uuid::new_v4(),
            task_observed_committed: false,
            start_released: false,
            fixed_image: None,
            process_closure: None,
            never_root_closure: None,
            terminal: None,
            source_drifted: false,
            cleanup_requested,
            progress,
        });
        role.finite_app = Some(app_state);
        Ok(())
    }

    /// Read-only candidate check. The durable queue repeats its checks under
    /// this same bundle lock; this predicate never grants native entry.
    pub(in crate::adaptive_resources::runtime) async fn copy_segment_slot_ready(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        bytes: u64,
    ) -> Result<bool, OperationError> {
        self.validate_preparation(owner)?;
        let bundle = owner.state.lock().await;
        let role = bundle.roles.get(index).ok_or(OperationError::State)?;
        if role.refunded
            || role.copy_vod.is_none()
            || role.pending_finite_close.is_some()
            || !matches!(role.phase, Phase::AcquisitionPossible | Phase::Sealed)
        {
            return Err(OperationError::State);
        }
        if role.transition.is_some() {
            return Ok(false);
        }
        role.bootstrap
            .get(&member)
            .ok_or(OperationError::State)?
            .rolling_segment_slot_ready(bytes)
    }

    pub(in crate::adaptive_resources::runtime) fn mark_copy_admission_failed(
        &self,
        owner: &OwnedPreparation,
        index: usize,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.try_lock().map_err(|_| OperationError::State)?;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        if role.refunded || role.copy_vod.is_none() {
            return Err(OperationError::State);
        }
        role.copy_admission_failed = true;
        Ok(())
    }

    pub(in crate::adaptive_resources::runtime) fn install_copy_work(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        work: CopyWork,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.try_lock().map_err(|_| OperationError::State)?;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        let copy = role.copy_vod.as_mut().ok_or(OperationError::State)?;
        if role.refunded || copy.entered || copy.work.is_some() || copy.never_root_closure.is_some()
        {
            return Err(OperationError::State);
        }
        copy.work = Some(work);
        Ok(())
    }

    /// Acquire BOTH planned SourceV1 members on their original registered
    /// cells while the graph remains dormant. Unknown native commits retain
    /// the same role and original operation for later reconciliation.
    pub(in crate::adaptive_resources::runtime) async fn prepare_copy_inputs(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let source_members = {
            let bundle = owner.state.lock().await;
            bundle
                .roles
                .get(index)
                .and_then(|role| role.copy_vod.as_ref())
                .map(|copy| copy.source_members)
                .ok_or(OperationError::State)?
        };
        for member in source_members {
            self.acquire_prepared_source(owner, index, member, deadline)
                .await?;
        }
        Ok(())
    }

    /// Start only after durable source observations. This is the sole path
    /// that moves original inherited descriptors into the selected child.
    pub(in crate::adaptive_resources::runtime) async fn start_copy_vod(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        if !bundle.installed {
            return Err(OperationError::State);
        }
        let bundle_id = bundle.id;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        let copy = role.copy_vod.as_ref().ok_or(OperationError::State)?;
        if role.refunded || copy.source_drifted || Instant::now() >= deadline {
            return Err(OperationError::State);
        }
        let root_member = copy.root_member;
        let task_member = copy.task_member;
        let acquisition_id = copy.acquisition_id;
        let source_members = copy.source_members;
        if let Some(transition) = &role.transition {
            if !matches!(transition.effect, Effect::Enrich)
                && !matches!(transition.effect, Effect::CopyTask(m) if m == task_member)
                && !matches!(transition.effect, Effect::Arm(m) if m == root_member || m == task_member)
                && !matches!(transition.effect, Effect::SourceRoot(m) if source_members.contains(&m))
            {
                return Err(OperationError::State);
            }
            self.finish_transition(bundle_id, role).await?;
        }
        let root = if let Some(root) = role.roots.get(&root_member) {
            *root
        } else {
            let copy = role.copy_vod.as_ref().ok_or(OperationError::State)?;
            if copy.entered {
                return Err(OperationError::State);
            }
            let executable = copy.command.executable();
            let binary_digest = executable.sha256();
            let binary_version = executable.source_identity().render_s1();
            let binary_binding = source_files::executable_recipe_binding(&binary_digest);
            let binary_path = executable
                .path()
                .to_str()
                .ok_or(OperationError::State)?
                .to_owned();
            let mut descriptors = [(0, 3), (0, 9)];
            let mut verified_elf = false;
            for (offset, member) in source_members.iter().enumerate() {
                let source = role
                    .sources
                    .get_mut(member)
                    .ok_or(OperationError::State)?
                    .inherited_source(root_member)?;
                if source.acquisition_id.uuid() != acquisition_id || source.acknowledged {
                    return Err(OperationError::State);
                }
                if offset == 1 {
                    if !source.verified_elf
                        || source.verified_sha256 != Some(binary_digest)
                        || !matches!(&source.envelope.planned,
                            v2::MemberPlan::SourceV1 { slot, source }
                                if slot.protected_paths == [binary_path.clone()]
                                    && source.source_association.object_version == binary_version
                                    && source.source_association.recipe_source_binding == binary_binding)
                    {
                        return Err(OperationError::State);
                    }
                    verified_elf = true;
                } else if source.verified_sha256.is_some() {
                    return Err(OperationError::State);
                }
                descriptors[offset] = (source.raw_fd, if offset == 1 { 9 } else { 3 });
            }
            let copy = role.copy_vod.as_mut().ok_or(OperationError::State)?;
            if copy.fixed_image.is_none() {
                copy.fixed_image = Some(CopyFixedImage::from_verified_source(
                    root_member,
                    task_member,
                    acquisition_id,
                    copy.command.executable().source_identity(),
                    binary_digest,
                    verified_elf,
                )?);
            }
            let root_plan = role
                .members
                .iter()
                .find(|member| member.member_id == root_member)
                .ok_or(OperationError::State)?;
            if copy.command.program().to_str() != Some(root_plan.locator.as_str()) {
                return Err(OperationError::State);
            }
            let mut command = tokio::process::Command::new("/proc/self/fd/9");
            command.env_clear();
            command.args(copy.command.args());
            command
                .env("XDG_CACHE_HOME", copy.command.runtime_cache())
                .env("TMPDIR", copy.command.runtime_cache())
                .env("HOME", copy.command.runtime_cache())
                .env("LANG", "C")
                .env("LC_ALL", "C")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            crate::process::inherited_descriptors::inherit_raw_file_descriptors_with_cwd(
                &mut command,
                &descriptors,
                None,
            );
            self.launch_root_role(
                bundle_id,
                role,
                root_member,
                &mut command,
                crate::process::ChildWork::realtime("rolling copy"),
                deadline,
                Some(acquisition_id),
            )
            .await?
        };
        role.copy_vod.as_mut().ok_or(OperationError::State)?.entered = true;
        for member in source_members {
            let source = role
                .sources
                .get_mut(&member)
                .ok_or(OperationError::State)?
                .inherited_source(root_member)?;
            if source.acquisition_id.uuid() != acquisition_id {
                return Err(OperationError::State);
            }
            if !source.acknowledged {
                start_named_transition(
                    &self.journal,
                    role,
                    source.observation_id,
                    Mutation::RecordTypedAcquired {
                        envelope: source.envelope,
                    },
                    Effect::SourceRoot(member),
                    deadline,
                )?;
                self.finish_transition(bundle_id, role).await?;
            }
        }
        self.start_copy_task(bundle_id, role, root, task_member, deadline)
            .await
    }

    async fn start_copy_task(
        &self,
        bundle_id: Uuid,
        role: &mut OwnedRole,
        root: GraphRoot,
        member: Uuid,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        let plan = role
            .typed_members
            .as_ref()
            .and_then(|members| members.iter().find(|plan| plan.member_id() == member))
            .cloned()
            .ok_or(OperationError::State)?;
        if !matches!(plan, v2::MemberPlan::VodOutputTaskV1 { ref task, .. }
            if task.actor == v2::VodActor::RollingCopyV1)
            || role.refunded
        {
            return Err(OperationError::State);
        }
        if role
            .copy_vod
            .as_ref()
            .ok_or(OperationError::State)?
            .task_observed_committed
        {
            return self.release_copy_task(role).await;
        }
        if role
            .copy_vod
            .as_ref()
            .ok_or(OperationError::State)?
            .task
            .is_none()
        {
            if role.invoked.contains(&member) || role.graph.is_none() {
                return Err(OperationError::State);
            }
            if !role.armed.contains(&member) {
                start_transition(
                    &self.journal,
                    role,
                    Mutation::Arm {
                        members: vec![member],
                    },
                    Effect::Arm(member),
                    deadline,
                )?;
                self.finish_transition(bundle_id, role).await?;
            }
            if Instant::now() >= deadline {
                return Err(OperationError::Deadline);
            }
            let graph = role.graph.as_mut().ok_or(OperationError::State)?;
            let arm = DurableArm {
                binding: role.binding,
                generation: graph.identity().generation,
            };
            let copy = role.copy_vod.as_mut().ok_or(OperationError::State)?;
            let task = graph
                .spawn_durable_copy_vod(
                    root,
                    copy.root_member,
                    member,
                    copy.acquisition_id,
                    arm,
                    &mut copy.work,
                    &mut copy.fixed_image,
                )
                .await?;
            copy.task = Some(task);
            role.invoked.insert(member);
        }
        let copy = role.copy_vod.as_ref().ok_or(OperationError::State)?;
        let v2::MemberPlan::VodOutputTaskV1 { task, .. } = &plan else {
            return Err(OperationError::State);
        };
        let envelope = v2::Envelope {
            version: 2,
            planned: plan.clone(),
            observed: Some(v2::Observed::VodOutputTaskV1 { task: task.clone() }),
        };
        start_named_transition(
            &self.journal,
            role,
            copy.task_observation,
            Mutation::RecordTypedAcquired { envelope },
            Effect::CopyTask(member),
            deadline,
        )?;
        self.finish_transition(bundle_id, role).await?;
        role.copy_vod
            .as_mut()
            .ok_or(OperationError::State)?
            .task_observed_committed = true;
        self.release_copy_task(role).await
    }

    async fn release_copy_task(&self, role: &mut OwnedRole) -> Result<(), OperationError> {
        let copy = role.copy_vod.as_mut().ok_or(OperationError::State)?;
        if !copy.task_observed_committed {
            return Err(OperationError::State);
        }
        let task = copy.task.ok_or(OperationError::State)?;
        if copy.start_released {
            return Ok(());
        }
        let start = role
            .graph
            .as_ref()
            .ok_or(OperationError::State)?
            .take_copy_start(task)
            .await?;
        self.release_native_entry(role.binding, start)?;
        // No suspension separates successful send from its delivery record.
        // A refused send never grants replay success or a fresh sender.
        copy.start_released = true;
        Ok(())
    }

    /// Join the original registered parser, root and source parents. A
    /// cancelled waiter leaves each original handle in the runtime book; no
    /// parser outcome or root status by itself authorizes output settlement.
    pub(in crate::adaptive_resources::runtime) async fn join_copy_producer(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        abort: bool,
    ) -> Result<CopyTaskSummary, OperationError> {
        self.validate_preparation(owner)?;
        let (graph, task, root) = {
            let mut bundle = owner.state.lock().await;
            if !bundle.installed {
                return Err(OperationError::State);
            }
            let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
            let copy = role.copy_vod.as_ref().ok_or(OperationError::State)?;
            if role.refunded || !copy.entered || !copy.task_observed_committed {
                return Err(OperationError::State);
            }
            if let Some(terminal) = copy.terminal {
                return Ok(terminal);
            }
            let task = copy.task.ok_or(OperationError::State)?;
            let root = role
                .roots
                .get(&copy.root_member)
                .copied()
                .ok_or(OperationError::State)?;
            if let Some(graph) = role.graph.take() {
                role.sealed = Some(graph.seal());
            }
            let graph = role
                .sealed
                .as_ref()
                .ok_or(OperationError::State)?
                .select_same();
            (graph, task, root)
        };
        if abort {
            graph.start_kill_root(root).await?;
        }
        let parser_terminal = graph.join_copy_vod(task, false).await?;
        let root_status = graph.wait_root(root).await?;
        let terminal = CopyTaskSummary {
            completed: parser_terminal.completed && root_status.success(),
            segments: parser_terminal.segments,
        };
        let mut bundle = owner.state.lock().await;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        self.close_retained_entry(role.binding)?;
        let mut first_error = None;
        for bootstrap in role.bootstrap.values_mut() {
            if let Err(error) = bootstrap.join_registered_native().await {
                first_error.get_or_insert(error);
            }
        }
        for source in role.sources.values_mut() {
            if let Err(error) = source.join_registered().await {
                first_error.get_or_insert(error);
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        for source in role.sources.values_mut() {
            if let Err(error) = source.close_parent().await {
                first_error.get_or_insert(error);
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        let copy = role.copy_vod.as_mut().ok_or(OperationError::State)?;
        if copy.process_closure.is_none() {
            copy.process_closure = Some(graph.try_copy_process_closure(task).await?);
        }
        let closure = copy.process_closure.as_ref().ok_or(OperationError::State)?;
        if !closure.matches(&graph)
            || closure.task() != task
            || closure.root() != root
            || closure.root_member() != copy.root_member
            || closure.acquisition_id() != copy.acquisition_id
            || closure.executable_identity() != copy.command.executable().source_identity()
            || closure.executable_sha256() != copy.command.executable().sha256()
            || closure.summary().completed != parser_terminal.completed
            || closure.summary().segments != parser_terminal.segments
        {
            return Err(OperationError::State);
        }
        for source in role.sources.values() {
            let _ = source.copy_closed_fact(closure)?;
        }
        copy.terminal = Some(terminal);
        Ok(terminal)
    }
}

pub(super) fn acknowledge_task(
    role: &mut OwnedRole,
    member: Uuid,
    operation: Uuid,
) -> Result<(), OperationError> {
    let copy = role.copy_vod.as_mut().ok_or(OperationError::State)?;
    if copy.task_member != member || copy.task_observation != operation || copy.task.is_none() {
        return Err(OperationError::State);
    }
    copy.task_observed_committed = true;
    Ok(())
}

/// Test-only diagnostics from the original registered copy. These selectors
/// cannot construct a native owner, start sender, or closure receipt.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(in crate::adaptive_resources::runtime) struct CopyLaunchFixtureIdentity {
    root_member: Uuid,
    task_member: Uuid,
    acquisition_id: Uuid,
    root: GraphRoot,
    task: GraphCopyTask,
    task_observation: Uuid,
    source_observations: [Uuid; 2],
}

#[cfg(test)]
impl NodeResourceRuntime {
    pub(in crate::adaptive_resources::runtime) async fn fixture_copy_launch_identity(
        &self,
        owner: &OwnedPreparation,
    ) -> Result<CopyLaunchFixtureIdentity, OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        let role = bundle.roles.get_mut(0).ok_or(OperationError::State)?;
        let copy = role.copy_vod.as_ref().ok_or(OperationError::State)?;
        if role.finite_vod.is_some()
            || role.refunded
            || role.transition.is_some()
            || !copy.entered
            || !copy.task_observed_committed
            || !copy.start_released
            || role.roots.len() != 1
            || role.sources.len() != 2
            || !role.armed.contains(&copy.root_member)
            || !role.armed.contains(&copy.task_member)
            || !role.invoked.contains(&copy.root_member)
            || !role.invoked.contains(&copy.task_member)
        {
            return Err(OperationError::State);
        }
        let mut identity = CopyLaunchFixtureIdentity {
            root_member: copy.root_member,
            task_member: copy.task_member,
            acquisition_id: copy.acquisition_id,
            root: *role
                .roots
                .get(&copy.root_member)
                .ok_or(OperationError::State)?,
            task: copy.task.ok_or(OperationError::State)?,
            task_observation: copy.task_observation,
            source_observations: [Uuid::nil(); 2],
        };
        if identity.root.registered_control().is_none() {
            return Err(OperationError::State);
        }
        let sources = copy.source_members;
        for (index, member) in sources.into_iter().enumerate() {
            let inherited = role
                .sources
                .get_mut(&member)
                .ok_or(OperationError::State)?
                .inherited_source(identity.root_member)?;
            if !inherited.acknowledged || inherited.acquisition_id.uuid() != identity.acquisition_id
            {
                return Err(OperationError::State);
            }
            identity.source_observations[index] = inherited.observation_id;
        }
        Ok(identity)
    }

    /// Verify the existing copy-specific receipt after actual joins. Never
    /// manufacture a software fixed-image proof or a replacement native owner.
    pub(in crate::adaptive_resources::runtime) async fn fixture_copy_joined_identity(
        &self,
        owner: &OwnedPreparation,
        expected: &CopyLaunchFixtureIdentity,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let bundle = owner.state.lock().await;
        let role = bundle.roles.first().ok_or(OperationError::State)?;
        let copy = role.copy_vod.as_ref().ok_or(OperationError::State)?;
        let graph = role.sealed.as_ref().ok_or(OperationError::State)?;
        let closure = copy.process_closure.as_ref().ok_or(OperationError::State)?;
        if role.finite_vod.is_some()
            || copy.terminal.is_none()
            || !closure.matches(graph)
            || closure.root_member() != expected.root_member
            || closure.root() != expected.root
            || closure.task() != expected.task
            || closure.acquisition_id() != expected.acquisition_id
            || role.sources.len() != 2
        {
            return Err(OperationError::State);
        }
        for source in role.sources.values() {
            if !source.parent_closed() {
                return Err(OperationError::State);
            }
            let _ = source.copy_closed_fact(closure)?;
        }
        let native = graph.snapshot().await;
        if native.roots != 0
            || native.writers != 1
            || native.joined_writers != 1
            || native.abandoned_writers != 0
        {
            return Err(OperationError::State);
        }
        Ok(())
    }
}
