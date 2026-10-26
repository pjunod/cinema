//! The one fixed software-AVC launch/task composition. Caller data cannot
//! substitute a Command, future, pipe owner, source descriptor or close proof.
use super::*;
use crate::adaptive_resources::physical_graph::finite_vod::{
    FiniteFixedImage, FiniteVodNeverRootClosure, FiniteVodProcessClosure, FiniteVodRootOnlyClosure,
    GraphFiniteVodTask,
};
use crate::adaptive_resources::runtime::finite_vod::{pump::SoftwarePump, SoftwareAvcCommand};
use sha2::{Digest, Sha256};
use std::process::Stdio;

pub(super) struct FiniteLaunch {
    pub(super) root_member: Uuid,
    pub(super) task_member: Uuid,
    pub(super) acquisition_id: Uuid,
    pub(super) source_members: Vec<Uuid>,
    /// Synchronous original registrations; absence is meaningful only with
    /// the sealed no-root graph and a matching unarmed planned slot.
    pub(super) registered_source_members: std::collections::BTreeSet<Uuid>,
    command: SoftwareAvcCommand,
    pump: Option<SoftwarePump>,
    pub(super) entered: bool,
    pub(super) control: Arc<crate::adaptive_resources::runtime::finite_vod::FiniteControl>,
    task_entered: bool,
    task: FiniteTaskDisposition,
    fixed_image: Option<FiniteFixedImage>,
    pub(super) process_closure: Option<FiniteVodProcessClosure>,
    pub(super) root_only_closure: Option<FiniteVodRootOnlyClosure>,
    pub(super) never_root_closure: Option<FiniteVodNeverRootClosure>,
    completion: Option<crate::adaptive_resources::runtime::finite_vod::FiniteProducerCompletion>,
    task_observation: Uuid,
    task_observed_committed: bool,
    pub(super) retiring: bool,
    pub(super) producer_joined: bool,
    pub(super) source_drifted: bool,
    /// Original registered metadata/digest worker. A cancelled waiter never
    /// detaches it or drops a transient digest File as positive close proof.
    engine_check: Option<tokio::task::JoinHandle<EngineCheckOutcome>>,
    pub(super) engine_check_unknown: bool,
}

enum EngineCheckOutcome {
    NeverEntered,
    Current,
    Changed,
    Unknown,
}

impl FiniteLaunch {
    pub(super) fn prepared_payload_disposable(&self) -> bool {
        !self.entered
            && !self.task_entered
            && self.engine_check.is_none()
            && !self.engine_check_unknown
    }

    async fn join_engine_check(&mut self) -> Result<(), OperationError> {
        join_engine_check_pending(
            &mut self.engine_check,
            &mut self.source_drifted,
            &mut self.engine_check_unknown,
        )
        .await
    }
}

async fn join_engine_check_pending(
    pending: &mut Option<tokio::task::JoinHandle<EngineCheckOutcome>>,
    drifted: &mut bool,
    unknown: &mut bool,
) -> Result<(), OperationError> {
    let Some(original) = pending.as_mut() else {
        return if *unknown {
            Err(OperationError::State)
        } else {
            Ok(())
        };
    };
    let joined = original.await;
    *pending = None;
    match joined {
        Ok(EngineCheckOutcome::Current) => Ok(()),
        Ok(EngineCheckOutcome::NeverEntered | EngineCheckOutcome::Changed) => {
            *drifted = true;
            Ok(())
        }
        Ok(EngineCheckOutcome::Unknown) | Err(_) => {
            *unknown = true;
            Err(OperationError::State)
        }
    }
}

#[derive(Clone, Copy)]
enum FiniteTaskDisposition {
    Pending,
    Registered(GraphFiniteVodTask),
    ProvenNeverAcquired,
}
impl FiniteTaskDisposition {
    fn registered(self) -> Option<GraphFiniteVodTask> {
        match self {
            Self::Registered(task) => Some(task),
            Self::Pending | Self::ProvenNeverAcquired => None,
        }
    }
}

impl NodeResourceRuntime {
    /// Store-side negative equality for the exact prepared finite graph and
    /// its original planned output. These hashes are not native identity,
    /// physical proof, a reservation or publication authority.
    pub(in crate::adaptive_resources::runtime) fn finite_worker_binding_digests(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        output_member: Uuid,
    ) -> Result<([u8; 32], [u8; 32]), OperationError> {
        self.validate_preparation(owner)?;
        let bundle = owner.state.try_lock().map_err(|_| OperationError::State)?;
        if !bundle.installed || !bundle.prepare.acknowledged || bundle.id != owner.id {
            return Err(OperationError::State);
        }
        let role = bundle.roles.get(index).ok_or(OperationError::State)?;
        let handle = role.handle.as_ref().ok_or(OperationError::State)?;
        let finite = role.finite_vod.as_ref().ok_or(OperationError::State)?;
        let members = role.typed_members.as_ref().ok_or(OperationError::State)?;
        let mut output_asset = None;
        for member in members {
            if let v2::MemberPlan::ScratchAssetsV3 { slot, plan } = member {
                if slot.member_id.uuid() == output_member
                    && output_asset.replace(plan.asset_id.uuid()).is_some()
                {
                    return Err(OperationError::State);
                }
            }
        }
        let output_asset = output_asset.ok_or(OperationError::State)?;
        let mut prepared = Sha256::new();
        prepared.update(b"plurx-finite-prepared-handle-v1");
        for id in [
            self.recovered.journal_id,
            self.recovered.installation_id,
            self.owner.owner_id,
            handle.obligation_id(),
            handle.graph_id(),
            owner.id,
            finite.root_member,
            finite.task_member,
            finite.acquisition_id,
        ] {
            prepared.update(id.as_bytes());
        }
        let mut output = Sha256::new();
        output.update(b"plurx-finite-planned-output-v1");
        for id in [
            self.recovered.journal_id,
            self.recovered.installation_id,
            self.owner.owner_id,
            handle.obligation_id(),
            handle.graph_id(),
            output_member,
            output_asset,
        ] {
            output.update(id.as_bytes());
        }
        Ok((prepared.finalize().into(), output.finalize().into()))
    }

    /// Recheck the captured dependency and reporter versions off the async
    /// worker. This is a recipe-drift fence; it cannot prove native custody.
    async fn validate_finite_engine_snapshot(
        &self,
        owner: &OwnedPreparation,
        index: usize,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        if !bundle.installed {
            return Err(OperationError::State);
        }
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        let finite = role.finite_vod.as_mut().ok_or(OperationError::State)?;
        finite.join_engine_check().await?;
        if role.refunded || finite.source_drifted || finite.engine_check_unknown {
            return Err(OperationError::State);
        }
        let snapshot = finite.command.engine_snapshot();
        let runtime = tokio::runtime::Handle::try_current().map_err(std::io::Error::other)?;
        let (enter, registered) = tokio::sync::oneshot::channel();
        finite.engine_check = Some(runtime.spawn_blocking(move || {
            if registered.blocking_recv().is_err() {
                return EngineCheckOutcome::NeverEntered;
            }
            match snapshot.checked_current() {
                Some(true) => EngineCheckOutcome::Current,
                Some(false) => EngineCheckOutcome::Changed,
                None => EngineCheckOutcome::Unknown,
            }
        }));
        // Registration is now stored in the original role. No await or
        // cancellation point separates that store from native entry release.
        let _ = enter.send(());
        finite.join_engine_check().await?;
        if finite.source_drifted {
            return Err(OperationError::State);
        }
        Ok(())
    }

    /// Join only the original registered check; normal producer join and all
    /// cancellation paths call this before native settlement or refund.
    pub(in crate::adaptive_resources::runtime) async fn reconcile_finite_engine_check(
        &self,
        owner: &OwnedPreparation,
        index: usize,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        role.finite_vod
            .as_mut()
            .ok_or(OperationError::State)?
            .join_engine_check()
            .await
    }

    #[allow(clippy::too_many_arguments)]
    /// Only the original preparation initializer may attach this payload.
    /// Its unpublished state cannot contend or have acquired native members.
    pub(in crate::adaptive_resources::runtime) fn initialize_finite_vod(
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
        app_state: Arc<crate::adaptive_resources::runtime::finite_lifetime::FiniteAppState>,
        response_budget: Arc<crate::adaptive_resources::runtime::response_memory::ResponseBudget>,
    ) {
        let mut bundle = owner
            .state
            .try_lock()
            .expect("unpublished finite preparation");
        let role = bundle.roles.get_mut(index).expect("original finite role");
        assert!(!(role.refunded || role.finite_vod.is_some() || role.graph.is_none()
            || source_members.len() < 2 || source_members.len() > 3
            || source_members.len() != command.source_count() + 1 || acquisition_id.is_nil()
            || source_members.iter().collect::<std::collections::BTreeSet<_>>().len()
                != source_members.len()
            || role.typed_members.as_ref().is_none_or(|members| !members.iter().any(|member|
                matches!(member, v2::MemberPlan::VodOutputTaskV1 { slot, task }
                    if slot.member_id.uuid() == task_member && task.root_member_id.uuid() == root_member)))),
            "fixed finite initializer must match its original manifest");
        role.response_budget = Some(response_budget);
        role.finite_vod = Some(FiniteLaunch {
            root_member,
            task_member,
            acquisition_id,
            source_members,
            registered_source_members: std::collections::BTreeSet::new(),
            command,
            pump: Some(pump),
            entered: false,
            control,
            task_entered: false,
            task: FiniteTaskDisposition::Pending,
            fixed_image: None,
            process_closure: None,
            root_only_closure: None,
            never_root_closure: None,
            completion: None,
            task_observation: Uuid::new_v4(),
            task_observed_committed: false,
            retiring: false,
            producer_joined: false,
            source_drifted: false,
            engine_check: None,
            engine_check_unknown: false,
        });
        role.finite_app = Some(app_state);
    }

    /// Revalidate each media source through its original retained File. Any
    /// failed native identity check latches the producer against later freeze
    /// or R-to-S transfer, including when the waiting caller is cancelled.
    pub(in crate::adaptive_resources::runtime) async fn validate_finite_sources(
        &self,
        owner: &OwnedPreparation,
        index: usize,
    ) -> Result<(), OperationError> {
        self.validate_finite_engine_snapshot(owner, index).await?;
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        if !bundle.installed {
            return Err(OperationError::State);
        }
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        let result = {
            let finite = role.finite_vod.as_ref().ok_or(OperationError::State)?;
            if role.refunded
                || finite.source_drifted
                || !finite.entered
                || !finite.task_entered
                || finite.producer_joined
                || finite.source_members.len() != finite.command.source_count() + 1
            {
                return Err(OperationError::State);
            }
            let mut result = Ok(());
            for member in finite
                .source_members
                .iter()
                .take(finite.command.source_count())
            {
                if let Err(error) = role
                    .sources
                    .get(member)
                    .ok_or(OperationError::State)
                    .and_then(source_files::SourceCell::unchanged_for_publication)
                {
                    result = Err(error);
                    break;
                }
            }
            result
        };
        if result.is_err() {
            role.finite_vod
                .as_mut()
                .ok_or(OperationError::State)?
                .source_drifted = true;
        }
        result
    }

    pub(in crate::adaptive_resources::runtime) async fn start_finite_vod(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_finite_engine_snapshot(owner, index).await?;
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
        if role.refunded || finite.retiring || finite.source_drifted {
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
            let binary_digest = finite.command.executable().sha256();
            let binary_version = finite.command.executable().source_identity().render_s1();
            let binary_binding = source_files::executable_recipe_binding(&binary_digest);
            let binary_path = finite
                .command
                .program()
                .to_str()
                .ok_or(OperationError::State)?
                .to_owned();
            let mut descriptors = [(0, 3), (0, 4), (0, 9)];
            let mut verified_elf = false;
            for (index, member) in source_members.iter().enumerate() {
                let source = role
                    .sources
                    .get_mut(member)
                    .ok_or(OperationError::State)?
                    .inherited_source(root_member)?;
                if source.acquisition_id.uuid() != acquisition_id || source.acknowledged {
                    return Err(OperationError::State);
                }
                if index + 1 == source_members.len() {
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
                } else if source.verified_sha256.is_some() {
                    return Err(OperationError::State);
                }
                if index + 1 == source_members.len() {
                    verified_elf = source.verified_elf;
                }
                descriptors[index] = (
                    source.raw_fd,
                    if index + 1 == source_members.len() {
                        9
                    } else {
                        3 + index as i32
                    },
                );
            }
            let finite = role.finite_vod.as_mut().ok_or(OperationError::State)?;
            if finite.fixed_image.is_none() {
                finite.fixed_image = Some(FiniteFixedImage::from_verified_source(
                    root_member,
                    task_member,
                    acquisition_id,
                    finite.command.executable().source_identity(),
                    binary_digest,
                    verified_elf,
                )?);
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
            // SourceCell has verified the selected digest on the SAME held
            // executable. pre_exec installs its planned child copy as fd 9;
            // pathname replacement cannot select a different launch object.
            let mut command = tokio::process::Command::new("/proc/self/fd/9");
            command.env_clear();
            command.args(selected.args());
            command
                .env("XDG_CACHE_HOME", selected.runtime_cache())
                .env("CUDA_CACHE_PATH", selected.runtime_cache())
                .env("AV_LOG_FORCE_NOCOLOR", "1")
                .env("TMPDIR", selected.runtime_cache())
                .env("HOME", selected.runtime_cache())
                .env("LANG", "C")
                .env("LC_ALL", "C");
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
        role.finite_vod
            .as_mut()
            .ok_or(OperationError::State)?
            .entered = true;
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
                #[cfg(all(test, target_os = "linux", feature = "fixtures"))]
                crate::adaptive_resources::runtime::finite_vod::cleanup_tests::before_source_root_ack(*member).await;
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
            .registered()
            .is_none()
        {
            if !matches!(
                role.finite_vod.as_ref().ok_or(OperationError::State)?.task,
                FiniteTaskDisposition::Pending
            ) {
                return Err(OperationError::State);
            }
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
            let (task, abort) = graph
                .spawn_durable_finite_vod(
                    root,
                    finite.root_member,
                    member,
                    finite.acquisition_id,
                    arm,
                    &mut finite.pump,
                    &mut finite.fixed_image,
                )
                .await?;
            finite.task = FiniteTaskDisposition::Registered(task);
            role.invoked.insert(member);
            finite.control.install_task(abort)?;
        }
        let finite = role.finite_vod.as_ref().ok_or(OperationError::State)?;
        if finite.task.registered().is_none() {
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
        let task = finite.task.registered().ok_or(OperationError::State)?;
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
    if finite.task_member != member
        || finite.task_observation != operation
        || finite.task.registered().is_none()
    {
        return Err(OperationError::State);
    }
    finite.task_observed_committed = true;
    Ok(())
}

impl NodeResourceRuntime {
    /// Complete only the captured acquisition command before cancellation
    /// destroys any of its original source/task acknowledgment inputs. This
    /// never enters a root, releases a start latch, or replaces an operation.
    pub(in crate::adaptive_resources::runtime) async fn reconcile_finite_acquisition_for_cleanup(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        let bundle_id = bundle.id;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        let finite = role.finite_vod.as_ref().ok_or(OperationError::State)?;
        let Some(transition) = role.transition.as_mut() else {
            return Ok(());
        };
        let captured_acquisition = match transition.effect {
            Effect::Enrich => true,
            Effect::SourceRoot(member) => finite.source_members.contains(&member),
            Effect::FiniteTask(member) => member == finite.task_member,
            Effect::Arm(member) => member == finite.root_member || member == finite.task_member,
            // Bootstrap and pre-root source-prefix commands retain their own
            // cleanup reconciler; this helper cannot change their evidence.
            _ => false,
        };
        if !captured_acquisition {
            return Ok(());
        }
        if transition.operation.pending.is_none()
            && !matches!(transition.operation.result, Some(Ok(_)))
        {
            transition.operation.retry(&self.journal, deadline)?;
        }
        self.finish_transition(bundle_id, role).await
    }

    /// Join the original fixed producer and retain its process-only closure.
    /// Source and output disposition, journal settlement and refund are separate.
    pub(in crate::adaptive_resources::runtime) async fn join_finite_producer(
        &self,
        owner: &OwnedPreparation,
        index: usize,
    ) -> Result<
        crate::adaptive_resources::runtime::finite_vod::FiniteProducerCompletion,
        OperationError,
    > {
        self.validate_preparation(owner)?;
        let (graph, task, root, control, root_only_closed) = {
            let mut bundle = owner.state.lock().await;
            if !bundle.installed {
                return Err(OperationError::State);
            }
            let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
            let finite = role.finite_vod.as_mut().ok_or(OperationError::State)?;
            if let Some(completion) = finite.completion {
                return Ok(completion);
            }
            let root_only_closed = finite.root_only_closure.is_some();
            if matches!(finite.task, FiniteTaskDisposition::ProvenNeverAcquired) != root_only_closed
            {
                return Err(OperationError::State);
            }
            if role.refunded
                || (!root_only_closed && !finite.control.requested() && !finite.task_entered)
            {
                return Err(OperationError::State);
            }
            // Seal only graph entry here. The admitted writer still needs its
            // original runtime entry to finish queued publications and fsyncs.
            finite.retiring = true;
            let task = finite.task.registered();
            let root = if root_only_closed {
                None
            } else {
                role.roots.get(&finite.root_member).copied()
            };
            let control = Arc::clone(&finite.control);
            if let Some(graph) = role.graph.take() {
                role.sealed = Some(graph.seal());
            }
            let graph = role
                .sealed
                .as_ref()
                .ok_or(OperationError::State)?
                .select_same();
            (graph, task, root, control, root_only_closed)
        };
        // No bundle lock is held while joining. The SAME task/result remain in
        // graph custody if this waiter is cancelled before or after the join.
        let writer = if let Some(task) = task {
            let joined = graph.join_finite_vod(task, control.requested()).await?;
            if joined.selection() != task {
                return Err(OperationError::State);
            }
            let summary = joined.summary()?;
            control.release_joined_task()?;
            Some(summary)
        } else {
            None
        };
        let root_status = if let Some(root) = root {
            Some(graph.wait_root(root).await?)
        } else {
            None
        };
        // The writer has stopped issuing checks. A cancelled publication may
        // still have left its original native check registered in FiniteLaunch.
        self.reconcile_finite_engine_check(owner, index).await?;
        let mut bundle = owner.state.lock().await;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        role.finite_vod
            .as_mut()
            .ok_or(OperationError::State)?
            .producer_joined = task.is_some() && root.is_some();
        // Once the writer has joined, fence later native entry through the SAME
        // ledger. Existing native workers are joined without new Arm/Observe.
        self.close_retained_entry(role.binding)?;
        if task.is_none() && !root_only_closed {
            let finite = role.finite_vod.as_mut().ok_or(OperationError::State)?;
            let root = root.ok_or(OperationError::State)?;
            // Close the waited original root before source reconciliation:
            // an uncommitted source Observe may prevent parent close, but
            // cannot justify keeping this already-waited process and pipes.
            let closure = graph
                .try_finite_vod_root_only_closure(
                    root,
                    finite.root_member,
                    finite.task_member,
                    finite.acquisition_id,
                    &mut finite.fixed_image,
                )
                .await?;
            let exact = closure.matches(&graph)
                && closure.root() == root
                && closure.root_member() == finite.root_member
                && closure.task_member() == finite.task_member
                && closure.acquisition_id() == finite.acquisition_id
                && !closure.close_operation_id().is_nil()
                && closure.executable_identity() == finite.command.executable().source_identity()
                && closure.executable_sha256() == finite.command.executable().sha256();
            finite.root_only_closure = Some(closure);
            finite.task = FiniteTaskDisposition::ProvenNeverAcquired;
            if !exact {
                return Err(OperationError::State);
            }
        }
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
        // Only positively observed source parents can close here. Partial or
        // ambiguous acquisition stays held for exact journal reconciliation.
        for source in role.sources.values_mut() {
            if let Err(error) = source.close_parent().await {
                first_error.get_or_insert(error);
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        if task.is_none() {
            let closure = role
                .finite_vod
                .as_ref()
                .and_then(|finite| finite.root_only_closure.as_ref())
                .ok_or(OperationError::State)?;
            for source in role.sources.values() {
                // The original source copy and root inheritance are checked
                // against the separate no-writer native closure. These data
                // facts alone cannot authorize whole-role settlement.
                let _ = source.finite_root_only_closed_fact(closure)?;
            }
        }
        let finite = role.finite_vod.as_mut().ok_or(OperationError::State)?;
        let Some(task) = task else {
            return Err(OperationError::Physical(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "fixed AVC root closed without task acquisition; typed settlement unavailable",
            )));
        };
        if finite.process_closure.is_none() {
            // No await or fallible operation separates the original one-shot
            // witness from installation into this retained runtime owner.
            finite.process_closure = Some(graph.try_finite_vod_process_closure(task).await?);
        }
        let closure = finite
            .process_closure
            .as_ref()
            .ok_or(OperationError::State)?;
        if !closure.matches(&graph)
            || closure.task() != task
            || closure.root_member() != finite.root_member
            || closure.acquisition_id() != finite.acquisition_id
            || closure.executable_identity() != finite.command.executable().source_identity()
            || closure.executable_sha256() != finite.command.executable().sha256()
            || Some(closure.writer()) != writer
        {
            return Err(OperationError::State);
        }
        let completion = crate::adaptive_resources::runtime::finite_vod::FiniteProducerCompletion {
            writer,
            root_status,
            parent_sources_closed: role
                .sources
                .values()
                .filter(|source| source.parent_closed())
                .count(),
        };
        finite.completion = Some(completion);
        Ok(completion)
    }
}

#[cfg(test)]
mod engine_check_tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_engine_check_waiter_keeps_original_registered_job() {
        let (entered, entered_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        let mut pending = Some(tokio::task::spawn_blocking(move || {
            let _ = entered.send(());
            let _ = release_rx.blocking_recv();
            EngineCheckOutcome::Current
        }));
        entered_rx.await.expect("registered worker entered");
        let mut drifted = false;
        let mut unknown = false;
        tokio::select! {
            result = join_engine_check_pending(&mut pending, &mut drifted, &mut unknown) => {
                panic!("blocked original job completed early: {result:?}");
            }
            _ = tokio::task::yield_now() => {}
        }
        assert!(
            pending.is_some(),
            "cancelled waiter retains original JoinHandle"
        );
        release.send(()).expect("release original job");
        join_engine_check_pending(&mut pending, &mut drifted, &mut unknown)
            .await
            .expect("original job positively joined");
        assert!(pending.is_none());
        assert!(!drifted && !unknown);
    }

    #[tokio::test]
    async fn engine_check_unknown_and_panic_remain_sticky_after_join() {
        for panic_job in [false, true] {
            let mut pending = Some(tokio::task::spawn_blocking(move || {
                if panic_job {
                    panic!("native check lost its close result");
                }
                EngineCheckOutcome::Unknown
            }));
            let mut drifted = false;
            let mut unknown = false;
            assert!(
                join_engine_check_pending(&mut pending, &mut drifted, &mut unknown)
                    .await
                    .is_err()
            );
            assert!(pending.is_none());
            assert!(unknown);
            assert!(!drifted);
            assert!(
                join_engine_check_pending(&mut pending, &mut drifted, &mut unknown)
                    .await
                    .is_err(),
                "a later settlement cannot erase uncertainty"
            );
        }
    }
}
