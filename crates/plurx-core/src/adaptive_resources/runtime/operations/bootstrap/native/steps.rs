//! Descriptor-relative steps in the SAME registered cell. Never reopen a path,
//! adopt EEXIST, clone a descriptor or repeat native work to retry a COMMIT.
use super::*;
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd};

pub(in crate::adaptive_resources::runtime::operations) fn mount_id(
    file: &File,
) -> v2::NativeValue<u64> {
    #[cfg(target_os = "linux")]
    {
        let mut stat = std::mem::MaybeUninit::<libc::statx>::zeroed();
        // SAFETY: the fd remains owned by the caller, the empty NUL-terminated
        // pathname selects that exact fd, and stat points to writable storage.
        let rc = unsafe {
            libc::statx(
                file.as_raw_fd(),
                c"".as_ptr(),
                libc::AT_EMPTY_PATH,
                libc::STATX_MNT_ID,
                stat.as_mut_ptr(),
            )
        };
        if rc == 0 {
            // SAFETY: a successful statx initialized this struct; check mask.
            let stat = unsafe { stat.assume_init() };
            if stat.stx_mask & libc::STATX_MNT_ID != 0 {
                return v2::NativeValue::Known {
                    value: stat.stx_mnt_id,
                };
            }
        }
    }
    let _ = file;
    v2::NativeValue::Unknown {
        reason: "descriptor mount identity unavailable".into(),
    }
}

fn empty_audit(outcome: v2::scratch::NativeOutcomeV1) -> BootstrapResultV1 {
    BootstrapResultV1 {
        outcome,
        acquired: None,
        closed_copy_ids: vec![],
        child_effect: ChildEffect::None,
        inventory: InventoryCertainty::Exact,
    }
}
fn failed(class: &str) -> v2::scratch::NativeOutcomeV1 {
    v2::scratch::NativeOutcomeV1::Failed {
        error_class: class.into(),
    }
}

impl RootCell {
    /// Call only after this step's actual result and acknowledgement are known.
    /// All fallible validation precedes moving the actual retained descriptors.
    pub(super) fn launch_next(
        &mut self,
        plan: &ScratchPlanV3,
        io: v2::Id,
        operation: &BootstrapIoV1,
    ) -> Result<tokio::sync::oneshot::Sender<()>, OperationError> {
        if self.pending.is_some() || self.failed_join || self.io_id == io {
            return Err(OperationError::State);
        }
        let result = self.result.as_ref().ok_or(OperationError::State)?;
        let parent = match operation {
            BootstrapIoV1::OpenNextComponent { parent, index, .. }
                if usize::from(*index) < plan.bootstrap().components.len() =>
            {
                parent
            }
            BootstrapIoV1::CreateRenditionChild { parent, .. } => parent,
            _ => return Err(OperationError::State),
        };
        // An observed reference alone never selects a descriptor. The actual
        // original result must own exactly its known copy under this same cell.
        let prior = result
            .audit
            .acquired
            .as_ref()
            .ok_or(OperationError::State)?;
        if result.audit.outcome != v2::scratch::NativeOutcomeV1::Completed
            || prior.directory_id != parent.directory_id
            || prior.held_copy_ids.len() != 1
            || result.files.len() != 1
            || !result.files.contains_key(&prior.held_copy_ids[0])
        {
            return Err(OperationError::State);
        }
        if matches!(operation, BootstrapIoV1::CreateRenditionChild { .. })
            && !matches!(prior.native_object.mount_id, v2::NativeValue::Known { .. })
        {
            // No mkdir when the actual parent cannot support descendant mount
            // continuity. A same-device value cannot substitute for a mount ID.
            return Err(OperationError::State);
        }
        let observe_id = v2::Id::new(Uuid::new_v4())?;
        let join_id = v2::Id::new(Uuid::new_v4())?;
        let mut result = self.result.take().ok_or(OperationError::State)?;
        let plan = plan.clone();
        let operation = operation.clone();
        let (enter, registered) = tokio::sync::oneshot::channel();
        let pending = tokio::task::spawn_blocking(move || {
            if registered.blocking_recv().is_err() {
                result.audit = empty_audit(v2::scratch::NativeOutcomeV1::NeverEntered);
                return result;
            }
            let result = run_step(result, &plan, io, observe_id, &operation);
            #[cfg(all(
                test,
                feature = "fixtures",
                target_os = "linux",
                any(target_arch = "x86_64", target_arch = "aarch64")
            ))]
            if result.audit.outcome == v2::scratch::NativeOutcomeV1::Completed {
                super::super::super::native_result_fixture::after_result(
                    match &operation {
                        BootstrapIoV1::OpenNextComponent { .. } => {
                            super::super::super::native_result_fixture::Kind::BootstrapTraversal
                        }
                        BootstrapIoV1::CreateRenditionChild { .. } => {
                            super::super::super::native_result_fixture::Kind::BootstrapChild
                        }
                        _ => unreachable!("validated original step"),
                    },
                    io.uuid(),
                );
            }
            result
        });
        self.io_id = io;
        self.observe_id = observe_id;
        self.join_id = join_id;
        self.pending = Some(pending);
        Ok(enter)
    }
}

fn run_step(
    mut result: RootResult,
    plan: &ScratchPlanV3,
    io: v2::Id,
    observed: v2::Id,
    operation: &BootstrapIoV1,
) -> RootResult {
    let old = result
        .audit
        .acquired
        .as_ref()
        .expect("validated actual parent")
        .clone();
    let old_copy = old.held_copy_ids[0];
    result.audit = empty_audit(failed("descriptor-relative bootstrap acquisition failed"));
    let (copy, name, create) = match operation {
        BootstrapIoV1::OpenNextComponent { index, copy_id, .. } => (
            *copy_id,
            &plan.bootstrap().components[usize::from(*index)],
            false,
        ),
        BootstrapIoV1::CreateRenditionChild { copy_id, .. } => {
            (*copy_id, &plan.bootstrap().child_component, true)
        }
        _ => return result,
    };
    let Ok(name) = CString::new(name.as_bytes()) else {
        return result;
    };
    let parent = result
        .files
        .get(&old_copy)
        .expect("validated actual parent descriptor");
    // Recheck the namespace in the actual registered worker, not in its caller.
    if !std::fs::metadata("/proc/self/ns/mnt").is_ok_and(|m|
        matches!(plan.filesystem_domain.mount_namespace, v2::NativeValue::Known { value } if value == m.ino()))
    { return result; }
    let parent_before = parent.metadata();
    if create {
        // EEXIST is never adoption. Once entered, a failed mkdir is an actual
        // failed observation; it is not NeverEntered or evidence of ownership.
        result.audit.child_effect = ChildEffect::Unchanged;
        // SAFETY: parent is retained; name is one validated NUL-terminated component.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            if std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
                result.audit.child_effect = ChildEffect::Unknown;
            }
            return result;
        }
        result.audit.child_effect = ChildEffect::Created;
    }
    // SAFETY: parent is retained throughout the call and name is validated.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return result;
    }
    // SAFETY: successful openat returned a new, otherwise unowned descriptor.
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata();
    let mount = mount_id(&file);
    let known = metadata.as_ref().is_ok_and(|m| m.is_dir());
    let native = v2::NativeObject {
        file_type: v2::FileType::Directory,
        device: metadata.as_ref().map_or_else(
            |_| v2::NativeValue::Unknown {
                reason: "fstat failed".into(),
            },
            |m| v2::NativeValue::Known { value: m.dev() },
        ),
        inode: metadata.as_ref().map_or_else(
            |_| v2::NativeValue::Unknown {
                reason: "fstat failed".into(),
            },
            |m| v2::NativeValue::Known { value: m.ino() },
        ),
        mount_id: mount,
        filesystem_domain: plan.filesystem_domain.clone(),
    };
    // A descendant mount (including a same-device bind mount) is not continuity.
    // Mount crossings are allowed ONLY along configured infrastructure traversal.
    let continuous = !create
        || (native.device == old.native_object.device
            && matches!((&native.mount_id, &old.native_object.mount_id),
            (v2::NativeValue::Known { value: a }, v2::NativeValue::Known { value: b }) if a == b));
    let allocated_within_bound = !create
        || metadata.as_ref().is_ok_and(|m| {
            m.blocks()
                .checked_mul(512)
                .is_some_and(|n| n <= plan.bootstrap().bounds.child_directory_reserve_bytes)
        }) && parent_before
            .as_ref()
            .ok()
            .zip(parent.metadata().ok())
            .is_some_and(|(before, after)| {
                after
                    .blocks()
                    .saturating_sub(before.blocks())
                    .checked_mul(512)
                    .is_some_and(|n| n <= plan.bootstrap().bounds.parent_entry_reserve_bytes)
            });
    result.files.insert(copy, file);
    result.audit.acquired = Some(v2::scratch::DirectoryObservedV1 {
        directory_id: observed,
        acquisition_io_id: io,
        held_copy_ids: vec![copy],
        native_object: native,
    });
    if known && continuous && allocated_within_bound {
        if !create {
            // Close the actual previous copy before recording closed_copy_ids.
            // No duplicate fd exists; peak traversal is exactly old+new.
            drop(result.files.remove(&old_copy));
            result.audit.closed_copy_ids.push(old_copy);
        }
        result.audit.outcome = v2::scratch::NativeOutcomeV1::Completed;
    } else if !allocated_within_bound {
        result.audit.outcome = failed("bootstrap directory allocation exceeded reserved allowance");
    }
    result
}

impl NodeResourceRuntime {
    /// Prearmed traversal and exclusive fresh child under the same admitted
    /// member. The cell keeps the actual parent+child after known SinkReady.
    /// No descriptor or serde reference is exported as publication authority.
    pub(crate) async fn open_scratch_bootstrap_sink(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.open_scratch_bootstrap_root(owner, index, member, deadline)
            .await?;
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        self.eligible(&bundle)?;
        let bundle_id = bundle.id;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        loop {
            if role.refunded
                || !matches!(role.phase, Phase::AcquisitionPossible | Phase::Sealed)
                || role
                    .transition
                    .as_ref()
                    .is_some_and(|t| t.effect != Effect::Bootstrap(member))
            {
                return Err(OperationError::State);
            }
            // A cancelled waiter resumes the EXACT original pending command.
            resume_change(&self.journal, role, member, deadline)?;
            if role.transition.is_some() {
                self.finish_transition(bundle_id, role).await?;
            }
            let bootstrap = role
                .bootstrap
                .get_mut(&member)
                .ok_or(OperationError::State)?;
            let state = bootstrap
                .fold
                .as_ref()
                .ok_or(OperationError::State)?
                .state();
            if bootstrap.next_arm.is_none() {
                if state.bootstrap.phase == BootstrapPhase::SinkReady {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(OperationError::Deadline);
                }
                let obligation = v2::Id::new(
                    role.handle
                        .as_ref()
                        .ok_or(OperationError::State)?
                        .obligation_id(),
                )?;
                let reference = |d: &BootstrapDirectoryV1| v2::scratch::OwnedDirectoryRefV1 {
                    owning_obligation: obligation,
                    member_id: v2::Id::new(member).expect("validated member"),
                    directory_id: d.observed.directory_id,
                };
                let copy_id = v2::Id::new(Uuid::new_v4())?;
                let operation = match state.bootstrap.phase {
                    BootstrapPhase::Traversing => BootstrapIoV1::OpenNextComponent {
                        index: state.bootstrap.next_component,
                        parent: reference(
                            state
                                .bootstrap
                                .current
                                .as_ref()
                                .ok_or(OperationError::State)?,
                        ),
                        copy_id,
                    },
                    BootstrapPhase::ParentReady => BootstrapIoV1::CreateRenditionChild {
                        parent: reference(
                            state
                                .bootstrap
                                .parent
                                .as_ref()
                                .ok_or(OperationError::State)?,
                        ),
                        copy_id,
                    },
                    _ => return Err(OperationError::State),
                };
                let io = v2::Id::new(Uuid::new_v4())?;
                let change = AssetChangeV3 {
                    scratch_member_id: v2::Id::new(member)?,
                    asset_id: bootstrap.plan.asset_id,
                    generation_epoch: bootstrap.plan.generation_epoch,
                    previous: state.asset.head,
                    event: AssetEventV3::ArmBootstrapV1 {
                        io_id: io,
                        operation: operation.clone(),
                    },
                };
                bootstrap.next_arm = Some((io, operation));
                bootstrap.change = Some((io, change.clone()));
                start_named_transition(
                    &self.journal,
                    role,
                    io.uuid(),
                    Mutation::AssetChangeV3 { change },
                    Effect::Bootstrap(member),
                    deadline,
                )?;
                self.finish_transition(bundle_id, role).await?;
            }
            let bootstrap = role
                .bootstrap
                .get_mut(&member)
                .ok_or(OperationError::State)?;
            let (io, operation) = bootstrap
                .next_arm
                .as_ref()
                .ok_or(OperationError::State)?
                .clone();
            let state = bootstrap
                .fold
                .as_ref()
                .ok_or(OperationError::State)?
                .state();
            let native = bootstrap
                .native_root
                .as_mut()
                .ok_or(OperationError::State)?;
            if native.io_id != io {
                if state
                    .asset
                    .head
                    .as_ref()
                    .is_none_or(|h| h.operation_id != io)
                    || state.asset.totals.pending_jobs != 1
                    || bootstrap.change.is_some()
                {
                    return Err(OperationError::State);
                }
                let enter = native.launch_next(&bootstrap.plan, io, &operation)?;
                // Original handle is already installed in the SAME owned cell.
                #[cfg(all(
                    test,
                    feature = "fixtures",
                    target_os = "linux",
                    any(target_arch = "x86_64", target_arch = "aarch64")
                ))]
                super::super::super::native_result_fixture::bind_next(
                    match &operation {
                        BootstrapIoV1::OpenNextComponent { .. } => {
                            super::super::super::native_result_fixture::Kind::BootstrapTraversal
                        }
                        BootstrapIoV1::CreateRenditionChild { .. } => {
                            super::super::super::native_result_fixture::Kind::BootstrapChild
                        }
                        _ => unreachable!("validated original step"),
                    },
                    member,
                    io.uuid(),
                    native
                        .pending
                        .as_ref()
                        .expect("same original step job")
                        .id(),
                );
                self.release_native_entry(role.binding, enter)?;
            }
            let audit = native.join().await?.audit.clone();
            let completed = audit.outcome == v2::scratch::NativeOutcomeV1::Completed;
            let observe_id = native.observe_id;
            if state
                .asset
                .head
                .as_ref()
                .is_none_or(|h| h.operation_id != observe_id)
            {
                if bootstrap.change.is_some() {
                    return Err(OperationError::State);
                }
                let change = AssetChangeV3 {
                    scratch_member_id: v2::Id::new(member)?,
                    asset_id: bootstrap.plan.asset_id,
                    generation_epoch: bootstrap.plan.generation_epoch,
                    previous: state.asset.head,
                    event: AssetEventV3::ObserveBootstrapV1 {
                        io_id: io,
                        native_join_operation_id: native.join_id,
                        result: audit,
                    },
                };
                bootstrap.change = Some((observe_id, change.clone()));
                start_named_transition(
                    &self.journal,
                    role,
                    observe_id.uuid(),
                    Mutation::AssetChangeV3 { change },
                    Effect::Bootstrap(member),
                    deadline,
                )?;
                self.finish_transition(bundle_id, role).await?;
            }
            // No await between acknowledgement and clearing the local arm. An
            // uncertain result keeps the original native outcome and both fds.
            role.bootstrap
                .get_mut(&member)
                .ok_or(OperationError::State)?
                .next_arm = None;
            if !completed {
                return Err(OperationError::State);
            }
        }
    }
}

impl NodeResourceRuntime {
    /// Reconcile only an already-armed traversal/create operation. Cancellation
    /// never advances to another component or enters a child creation that had
    /// not already entered. An unlaunched registered arm records NeverEntered.
    pub(in crate::adaptive_resources::runtime::operations) async fn observe_finite_bootstrap_arm(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        let bundle_id = bundle.id;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        if role.refunded
            || role
                .transition
                .as_ref()
                .is_some_and(|transition| transition.effect != Effect::Bootstrap(member))
        {
            return Err(OperationError::State);
        }
        resume_change(&self.journal, role, member, deadline)?;
        if role.transition.is_some() {
            self.finish_transition(bundle_id, role).await?;
        }
        let bootstrap = role
            .bootstrap
            .get_mut(&member)
            .ok_or(OperationError::State)?;
        let Some((io, operation)) = bootstrap.next_arm.clone() else {
            return Ok(());
        };
        let state = bootstrap
            .fold
            .as_ref()
            .ok_or(OperationError::State)?
            .state();
        let native = bootstrap
            .native_root
            .as_mut()
            .ok_or(OperationError::State)?;
        if state
            .asset
            .head
            .as_ref()
            .is_some_and(|head| head.operation_id == native.observe_id)
            && bootstrap.change.is_none()
        {
            bootstrap.next_arm = None;
            return Ok(());
        }
        if state
            .asset
            .head
            .as_ref()
            .is_none_or(|head| head.operation_id != io)
            || state.asset.totals.pending_jobs != 1
            || bootstrap.change.is_some()
        {
            return Err(OperationError::State);
        }
        if native.io_id != io {
            // The original arm committed but native entry was never released.
            // Retain the worker in this cell and drop its latch: it reports
            // NeverEntered, with no openat/mkdirat on cancellation.
            let enter = native.launch_next(&bootstrap.plan, io, &operation)?;
            drop(enter);
        }
        let audit = native.join().await?.audit.clone();
        let observe_id = native.observe_id;
        let change = AssetChangeV3 {
            scratch_member_id: v2::Id::new(member)?,
            asset_id: bootstrap.plan.asset_id,
            generation_epoch: bootstrap.plan.generation_epoch,
            previous: state.asset.head,
            event: AssetEventV3::ObserveBootstrapV1 {
                io_id: io,
                native_join_operation_id: native.join_id,
                result: audit,
            },
        };
        bootstrap.change = Some((observe_id, change.clone()));
        start_named_transition(
            &self.journal,
            role,
            observe_id.uuid(),
            Mutation::AssetChangeV3 { change },
            Effect::Bootstrap(member),
            deadline,
        )?;
        self.finish_transition(bundle_id, role).await?;
        role.bootstrap
            .get_mut(&member)
            .ok_or(OperationError::State)?
            .next_arm = None;
        Ok(())
    }
}
