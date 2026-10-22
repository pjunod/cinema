//! The one fixed software-AVC launch/task composition. Caller data cannot
//! substitute a Command, future, pipe owner, source descriptor or close proof.
use super::*;
use crate::adaptive_resources::physical_graph::finite_vod::GraphFiniteVodTask;
use crate::adaptive_resources::runtime::finite_vod::{pump::SoftwarePump, SoftwareAvcCommand};
use std::process::Stdio;

pub(super) struct FiniteLaunch {
    pub(super) root_member: Uuid,
    task_member: Uuid,
    pub(super) acquisition_id: Uuid,
    source_members: Vec<Uuid>,
    command: SoftwareAvcCommand,
    pump: Option<SoftwarePump>,
    pub(super) entered: bool,
    pub(super) control: Arc<crate::adaptive_resources::runtime::finite_vod::FiniteControl>,
    task_entered: bool,
    task: Option<GraphFiniteVodTask>,
    task_observation: Uuid,
    task_observed_committed: bool,
}

impl NodeResourceRuntime {
    #[allow(clippy::too_many_arguments)]
    pub(in crate::adaptive_resources::runtime) fn queue_finite_vod(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        root_member: Uuid,
        task_member: Uuid,
        acquisition_id: Uuid,
        source_members: Vec<Uuid>,
        command: SoftwareAvcCommand,
        pump: SoftwarePump,
        control: Arc<crate::adaptive_resources::runtime::finite_vod::FiniteControl>,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.try_lock().map_err(|_| OperationError::State)?;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        if role.refunded || role.finite_vod.is_some() || role.graph.is_none()
            || source_members.is_empty() || source_members.len() > 2
            || source_members.len() != command.source_count() || acquisition_id.is_nil()
            || role.typed_members.as_ref().is_none_or(|members| !members.iter().any(|member|
                matches!(member, v2::MemberPlan::VodOutputTaskV1 { slot, task }
                    if slot.member_id.uuid() == task_member && task.root_member_id.uuid() == root_member))) {
            return Err(OperationError::State);
        }
        role.finite_vod = Some(FiniteLaunch {
            root_member,
            task_member,
            acquisition_id,
            source_members,
            command,
            pump: Some(pump),
            entered: false,
            control,
            task_entered: false,
            task: None,
            task_observation: Uuid::new_v4(),
            task_observed_committed: false,
        });
        Ok(())
    }

    pub(in crate::adaptive_resources::runtime) async fn start_finite_vod(
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
        self.eligible(&bundle)?;
        let bundle_id = bundle.id;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        let finite = role.finite_vod.as_ref().ok_or(OperationError::State)?;
        let (root_member, task_member, acquisition_id) = (
            finite.root_member,
            finite.task_member,
            finite.acquisition_id,
        );
        let source_members = finite.source_members.clone();
        if role.refunded {
            return Err(OperationError::State);
        }
        // Resume the exact retained journal command, never submit replacement
        // IDs after a cancelled/unknown wait. Native ownership is already held.
        if let Some(transition) = &role.transition {
            if !matches!(transition.effect, Effect::Enrich)
                && !matches!(transition.effect, Effect::FiniteTask(m) if m == task_member)
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
            let finite = role.finite_vod.as_ref().ok_or(OperationError::State)?;
            if finite.entered || finite.control.requested() {
                return Err(OperationError::State);
            }
            let mut descriptors = [(0, 3), (0, 4)];
            for (index, member) in source_members.iter().enumerate() {
                let source = role
                    .sources
                    .get_mut(member)
                    .ok_or(OperationError::State)?
                    .inherited_source(root_member)?;
                if source.acquisition_id.uuid() != acquisition_id || source.acknowledged {
                    return Err(OperationError::State);
                }
                descriptors[index].0 = source.raw_fd;
            }
            let selected = &role
                .finite_vod
                .as_ref()
                .ok_or(OperationError::State)?
                .command;
            let root_plan = role
                .members
                .iter()
                .find(|member| member.member_id == root_member)
                .ok_or(OperationError::State)?;
            if selected.program().to_str() != Some(root_plan.locator.as_str()) {
                return Err(OperationError::State);
            }
            let mut command = tokio::process::Command::new(selected.program());
            command.args(selected.args());
            command
                .env("XDG_CACHE_HOME", selected.runtime_cache())
                .env("CUDA_CACHE_PATH", selected.runtime_cache())
                .env("AV_LOG_FORCE_NOCOLOR", "1");
            for (key, value) in selected.env() {
                command.env(key, value);
            }
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            crate::process::inherited_descriptors::inherit_raw_file_descriptors_with_cwd(
                &mut command,
                &descriptors[..source_members.len()],
                None,
            );
            // The original bundle lock holds the SAME source Files throughout
            // pre_exec and native spawn. No daemon source reopen or parent dup.
            self.launch_root_role(
                bundle_id,
                role,
                root_member,
                &mut command,
                crate::process::ChildWork::realtime("VOD transcode"),
                deadline,
                Some(acquisition_id),
            )
            .await?
        };
        // Actual launch succeeded and its original Child/ChildJob are retained.
        // Only now may the predeclared inherited copies enrich SourceObserved.
        for member in &source_members {
            let source = role
                .sources
                .get_mut(member)
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
                    Effect::SourceRoot(*member),
                    deadline,
                )?;
                self.finish_transition(bundle_id, role).await?;
            }
        }
        self.start_finite_task(bundle_id, role, root, task_member, deadline)
            .await
    }

    async fn start_finite_task(
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
        if !matches!(plan, v2::MemberPlan::VodOutputTaskV1 { .. }) || role.refunded {
            return Err(OperationError::State);
        }
        if role
            .finite_vod
            .as_ref()
            .ok_or(OperationError::State)?
            .task_observed_committed
        {
            return self.release_finite_task(role).await;
        }
        if role
            .finite_vod
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
            let finite = role.finite_vod.as_mut().ok_or(OperationError::State)?;
            let task = graph
                .spawn_durable_finite_vod(root, member, arm, &mut finite.pump)
                .await?;
            finite.task = Some(task);
            role.invoked.insert(member);
        }
        let finite = role.finite_vod.as_ref().ok_or(OperationError::State)?;
        if finite.task.is_none() {
            return Err(OperationError::State);
        }
        // The exact plan describes this fixed actor; the original graph has
        // already retained its real JoinHandle and guarded native pipe records.
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
            finite.task_observation,
            Mutation::RecordTypedAcquired { envelope },
            Effect::FiniteTask(member),
            deadline,
        )?;
        self.finish_transition(bundle_id, role).await?;
        self.release_finite_task(role).await
    }

    async fn release_finite_task(&self, role: &mut OwnedRole) -> Result<(), OperationError> {
        let finite = role.finite_vod.as_mut().ok_or(OperationError::State)?;
        if finite.task_entered {
            return Ok(());
        }
        if !finite.task_observed_committed || finite.control.requested() {
            return Err(OperationError::State);
        }
        let task = finite.task.ok_or(OperationError::State)?;
        let enter = role
            .graph
            .as_ref()
            .ok_or(OperationError::State)?
            .take_finite_vod_start(task)
            .await?;
        self.release_native_entry(role.binding, enter)?;
        finite.task_entered = true;
        Ok(())
    }
}

pub(super) fn acknowledge_task(
    role: &mut OwnedRole,
    member: Uuid,
    operation: Uuid,
) -> Result<(), OperationError> {
    let finite = role.finite_vod.as_mut().ok_or(OperationError::State)?;
    if finite.task_member != member || finite.task_observation != operation || finite.task.is_none()
    {
        return Err(OperationError::State);
    }
    finite.task_observed_committed = true;
    Ok(())
}
