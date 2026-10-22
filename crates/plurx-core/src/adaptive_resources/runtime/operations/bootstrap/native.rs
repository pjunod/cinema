//! Actual original blocking root-open job; no fd/path authority from a DTO.
//! Component traversal/child creation must extend THIS retained cell, not reopen
//! the configured pathname or duplicate a handle outside the admitted slots.
use super::*;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use tokio::task::JoinHandle;
mod steps;
#[cfg(test)]
mod tests;

pub(super) struct RootCell {
    io_id: v2::Id,
    observe_id: v2::Id,
    join_id: v2::Id,
    pending: Option<JoinHandle<RootResult>>,
    result: Option<RootResult>,
    failed_join: bool,
}
struct RootResult {
    // Even a partial fstat failure keeps the ACTUAL descriptor, not only JSON.
    files: BTreeMap<v2::Id, File>,
    audit: BootstrapResultV1,
}
impl RootCell {
    fn launch(
        domain: v2::FilesystemDomain,
        io: v2::Id,
        copy: v2::Id,
    ) -> Result<(Self, tokio::sync::oneshot::Sender<()>), OperationError> {
        let observe_id = v2::Id::new(Uuid::new_v4())?;
        let join_id = v2::Id::new(Uuid::new_v4())?;
        let (enter, registered) = tokio::sync::oneshot::channel();
        let pending = tokio::task::spawn_blocking(move || {
            if registered.blocking_recv().is_err() {
                return RootResult {
                    files: BTreeMap::new(),
                    audit: BootstrapResultV1 {
                        outcome: v2::scratch::NativeOutcomeV1::NeverEntered,
                        acquired: None,
                        closed_copy_ids: vec![],
                        child_effect: ChildEffect::None,
                        inventory: InventoryCertainty::Exact,
                    },
                };
            }
            open_root(domain, io, copy, observe_id)
        });
        let cell = Self {
            io_id: io,
            observe_id,
            join_id,
            pending: Some(pending),
            result: None,
            failed_join: false,
        };
        // The caller must register this original handle in OwnedRole before it
        // releases `enter`. If registration fails, no native call is entered.
        Ok((cell, enter))
    }
    async fn join(&mut self) -> Result<&RootResult, OperationError> {
        if self.failed_join {
            return Err(OperationError::State);
        }
        if let Some(pending) = self.pending.as_mut() {
            // Await the original handle IN PLACE. Dropping a waiter cannot move,
            // abort, replace or discard the actual job/result/descriptor.
            let joined = pending.await;
            self.pending = None;
            match joined {
                Ok(result) => self.result = Some(result),
                Err(_) => {
                    self.failed_join = true;
                    return Err(OperationError::State);
                }
            }
        }
        self.result.as_ref().ok_or(OperationError::State)
    }
}
fn open_root(
    domain: v2::FilesystemDomain,
    io: v2::Id,
    copy: v2::Id,
    observed: v2::Id,
) -> RootResult {
    let failure = |file: Option<File>| RootResult {
        files: file.into_iter().map(|file| (copy, file)).collect(),
        audit: BootstrapResultV1 {
            outcome: v2::scratch::NativeOutcomeV1::Failed {
                error_class: "namespace root native acquisition unavailable".into(),
            },
            acquired: None,
            closed_copy_ids: vec![],
            child_effect: ChildEffect::None,
            inventory: InventoryCertainty::Exact,
        },
    };
    // The Linux namespace must be actually observed in THIS original job. A path,
    // daemon UUID or an Unknown domain cannot assert mount-namespace continuity.
    let namespace = match std::fs::metadata("/proc/self/ns/mnt") {
        Ok(m) => m.ino(),
        Err(_) => return failure(None),
    };
    if !matches!(domain.mount_namespace, v2::NativeValue::Known { value } if value == namespace) {
        return failure(None);
    }
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open("/")
    {
        Ok(file) => file,
        Err(_) => return failure(None),
    };
    let metadata = file.metadata();
    let (native, outcome) = match metadata {
        Ok(m) if m.is_dir() => (
            v2::NativeObject {
                file_type: v2::FileType::Directory,
                device: v2::NativeValue::Known { value: m.dev() },
                inode: v2::NativeValue::Known { value: m.ino() },
                mount_id: steps::mount_id(&file),
                filesystem_domain: domain,
            },
            v2::scratch::NativeOutcomeV1::Completed,
        ),
        _ => (
            v2::NativeObject {
                file_type: v2::FileType::Directory,
                device: v2::NativeValue::Unknown {
                    reason: "retained root fstat unavailable".into(),
                },
                inode: v2::NativeValue::Unknown {
                    reason: "retained root fstat unavailable".into(),
                },
                mount_id: v2::NativeValue::Unknown {
                    reason: "retained root fstat unavailable".into(),
                },
                filesystem_domain: domain,
            },
            v2::scratch::NativeOutcomeV1::Failed {
                error_class: "root identity observation unavailable".into(),
            },
        ),
    };
    RootResult {
        files: BTreeMap::from([(copy, file)]),
        audit: BootstrapResultV1 {
            outcome,
            acquired: Some(v2::scratch::DirectoryObservedV1 {
                directory_id: observed,
                acquisition_io_id: io,
                held_copy_ids: vec![copy],
                native_object: native,
            }),
            closed_copy_ids: vec![],
            child_effect: ChildEffect::None,
            inventory: InventoryCertainty::Exact,
        },
    }
}

impl NodeResourceRuntime {
    /// Only the initial namespace-root step, not ParentReady/SinkReady. Normal
    /// output must wait for the charged component/child steps and known outcomes.
    pub(crate) async fn open_scratch_bootstrap_root(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        if !cfg!(target_os = "linux") {
            return Err(OperationError::State);
        }
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        if !bundle.installed {
            return Err(OperationError::State);
        }
        self.eligible(&bundle)?;
        let bundle_id = bundle.id;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        if role.refunded
            || !matches!(role.phase, Phase::AcquisitionPossible | Phase::Sealed)
            || role
                .transition
                .as_ref()
                .is_some_and(|t| t.effect != Effect::Bootstrap(member))
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
        if bootstrap.root_ready {
            return Ok(());
        }
        let (io, copy) = bootstrap.root_arm.ok_or(OperationError::State)?;
        let state = bootstrap
            .fold
            .as_ref()
            .ok_or(OperationError::State)?
            .state();
        if bootstrap.plan.bootstrap().authority.installation_id.uuid()
            != self.recovered.installation_id
            || self.rendition_root.as_deref()
                != Some(Path::new(
                    &bootstrap.plan.bootstrap().authority.resolved_absolute_root,
                ))
            || bootstrap.plan.filesystem_domain.os_boot != self.owner.os_boot
            || !matches!(self.owner.os_boot, BootIdentity::Known { .. })
        {
            return Err(OperationError::State);
        }
        if bootstrap.native_root.is_none() {
            if Instant::now() >= deadline {
                return Err(OperationError::Deadline);
            }
            if bootstrap.change.is_some()
                || role.transition.is_some()
                || state
                    .asset
                    .head
                    .as_ref()
                    .is_none_or(|h| h.operation_id != io)
                || state.asset.totals.pending_jobs != 1
                || !role.armed.contains(&member)
            {
                return Err(OperationError::State);
            }
            let (cell, enter) =
                RootCell::launch(bootstrap.plan.filesystem_domain.clone(), io, copy)?;
            bootstrap.native_root = Some(cell);
            let _ = enter.send(());
        }
        let native = bootstrap
            .native_root
            .as_mut()
            .ok_or(OperationError::State)?;
        let audit = native.join().await?.audit.clone();
        let completed = audit.outcome == v2::scratch::NativeOutcomeV1::Completed;
        let observe_id = native.observe_id;
        if state
            .asset
            .head
            .as_ref()
            .is_some_and(|h| h.operation_id == observe_id)
            && bootstrap.change.is_none()
            && role.transition.is_none()
        {
            bootstrap.root_ready = completed;
            return if completed {
                Ok(())
            } else {
                Err(OperationError::State)
            };
        }
        if bootstrap.change.is_none() {
            if role.transition.is_some() {
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
        }
        // Exact retries resolve the retained result/command, never open '/' again.
        self.finish_transition(bundle_id, role).await?;
        if completed {
            role.bootstrap
                .get_mut(&member)
                .ok_or(OperationError::State)?
                .root_ready = true;
            Ok(())
        } else {
            Err(OperationError::State)
        }
    }
}
