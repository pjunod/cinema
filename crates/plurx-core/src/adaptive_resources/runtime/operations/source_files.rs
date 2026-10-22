//! Source acquisition in the original retained bundle, before opening a path.
//! A returned lease lends the SAME descriptor; its Arc and close authority stay
//! private to this counted custodian. It is neither cloneable nor deserializable.
use super::*;
use crate::source_identity::SourceIdentity;
use std::fs::File;
use std::os::fd::{AsRawFd, IntoRawFd, RawFd};
use std::os::unix::fs::MetadataExt;
use tokio::task::JoinHandle;

pub struct RetainedSourceLease {
    file: Arc<File>,
}
impl RetainedSourceLease {
    /// Borrow only. Inherited child copies require their own predeclared slots;
    /// this API supplies no descriptor duplication or acquisition permission.
    pub fn file(&self) -> &File {
        &self.file
    }
}

pub(crate) struct SourceOpenSpec {
    pub path: PathBuf,
    pub scanner_size: i64,
    pub scanner_mtime: i64,
}

pub(super) struct SourceCell {
    spec: Option<SourceOpenSpec>,
    plan: v2::MemberPlan,
    acquisition_id: v2::Id,
    observation_id: Uuid,
    pending: Option<JoinHandle<SourceResult>>,
    result: Option<SourceResult>,
    failed_join: bool,
    observed_committed: bool,
    lease_issued: bool,
    root_observation_id: Uuid,
    root_observed_committed: bool,
    selected_root_member: Option<Uuid>,
    pending_root: Option<(Uuid, v2::SourceCopy, v2::SourceObserved)>,
    parent_close_operation_id: Uuid,
    parent_close: ParentCloseState,
}

enum ParentCloseState {
    NotStarted,
    Registered {
        task: JoinHandle<ParentCloseOutcome>,
        enter: Option<tokio::sync::oneshot::Sender<()>>,
    },
    Closed,
    Failed(i32),
    Unknown,
}

enum ParentCloseOutcome {
    Closed,
    Failed(i32),
    NotEntered,
    Unknown,
}

pub(super) struct InheritedSource {
    /// Borrowed from the retained File; the caller must hold the original
    /// bundle lock through the spawn that inherits this descriptor.
    pub raw_fd: RawFd,
    pub acquisition_id: v2::Id,
    pub observation_id: Uuid,
    pub envelope: v2::Envelope,
    pub acknowledged: bool,
}
struct SourceResult {
    file: Option<Arc<File>>,
    observed: Option<v2::SourceObserved>,
}
impl SourceCell {
    /// Await only the original source-open worker if it was registered. The
    /// actual file/observation remain cached; this does not launch or observe it.
    pub(super) async fn join_registered(&mut self) -> Result<(), OperationError> {
        if self.failed_join {
            return Err(OperationError::State);
        }
        if self.pending.is_some() {
            self.join().await?;
        }
        Ok(())
    }

    async fn join(&mut self) -> Result<&SourceResult, OperationError> {
        if self.failed_join {
            return Err(OperationError::State);
        }
        if let Some(pending) = self.pending.as_mut() {
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
    fn launch(&mut self) -> Result<tokio::sync::oneshot::Sender<()>, OperationError> {
        if self.pending.is_some() || self.result.is_some() || self.failed_join {
            return Err(OperationError::State);
        }
        let v2::MemberPlan::SourceV1 { source, .. } = &self.plan else {
            return Err(OperationError::State);
        };
        let runtime = tokio::runtime::Handle::try_current().map_err(std::io::Error::other)?;
        let plan = source.clone();
        let spec = self.spec.take().ok_or(OperationError::State)?;
        let (enter, registered) = tokio::sync::oneshot::channel();
        self.pending = Some(runtime.spawn_blocking(move || {
            if registered.blocking_recv().is_err() {
                return SourceResult {
                    file: None,
                    observed: None,
                };
            }
            open_source(spec, plan)
        }));
        Ok(enter)
    }
}

fn open_source(spec: SourceOpenSpec, plan: v2::SourcePlan) -> SourceResult {
    let mut result = SourceResult {
        file: None,
        observed: None,
    };
    // No source path is opened unless the exact current domain is observed in
    // this original, registered native job. Domain equality is not restart proof.
    if !std::fs::metadata("/proc/self/ns/mnt").is_ok_and(|m| {
        matches!(plan.filesystem_domain.mount_namespace,
            v2::NativeValue::Known { value } if value == m.ino())
    }) {
        return result;
    }
    let Ok(file) = crate::fs_secure::open_read_nofollow_blocking(&spec.path) else {
        return result;
    };
    // Install the actual File before ANY later validation can fail.
    result.file = Some(Arc::new(file));
    let file = result.file.as_ref().expect("actual native source");
    let Ok(metadata) = file.metadata() else {
        return result;
    };
    if !metadata.is_file()
        || spec.scanner_size < 0
        || !SourceIdentity::scanner_matches(&metadata, spec.scanner_size, spec.scanner_mtime)
        || SourceIdentity::from_metadata(&metadata).render_s1()
            != plan.source_association.object_version
    {
        return result;
    }
    result.observed = Some(v2::SourceObserved {
        version: 1,
        holder_id: plan.holder_id,
        member_id: plan.member_id,
        copies: plan
            .planned_copies
            .iter()
            .filter(|copy| matches!(copy.owner, v2::CopyOwner::DaemonSourceCell { .. }))
            .cloned()
            .collect(),
        native_object: v2::NativeObject {
            file_type: v2::FileType::Regular,
            device: v2::NativeValue::Known {
                value: metadata.dev(),
            },
            inode: v2::NativeValue::Known {
                value: metadata.ino(),
            },
            mount_id: bootstrap::native::steps::mount_id(file),
            filesystem_domain: plan.filesystem_domain,
        },
        access: v2::SourceAccess::ReadOnlyRegular,
        source_association: plan.source_association,
    });
    result
}

impl NodeResourceRuntime {
    /// Synchronous registration in admitted ownership; takes no already-open
    /// File and can never bless an acquisition that happened before prepare.
    pub(crate) fn queue_source_open(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        request: &mut Option<SourceOpenSpec>,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let spec = request.as_ref().ok_or(OperationError::State)?;
        if !cfg!(target_os = "linux")
            || spec.scanner_size < 0
            || !spec.path.is_absolute()
            || spec.path.as_os_str().len() > 4096
        {
            return Err(OperationError::State);
        }
        let mut bundle = owner.state.try_lock().map_err(|_| OperationError::State)?;
        if !bundle.installed {
            return Err(OperationError::State);
        }
        self.eligible(&bundle)?;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        if role.refunded
            || role.transition.is_some()
            || role.sources.contains_key(&member)
            || role.sources.len() >= 64
            || !matches!(role.phase, Phase::Prepared | Phase::AcquisitionPossible)
        {
            return Err(OperationError::State);
        }
        let plan = role
            .typed_members
            .as_ref()
            .and_then(|members| members.iter().find(|m| m.member_id() == member))
            .cloned()
            .ok_or(OperationError::State)?;
        let v2::MemberPlan::SourceV1 { slot, source } = &plan else {
            return Err(OperationError::State);
        };
        let path = spec.path.to_str().ok_or(OperationError::State)?;
        if !slot.protected_paths.iter().any(|p| p == path)
            || source.filesystem_domain.platform != v2::FilesystemPlatform::Linux
            || source.filesystem_domain.daemon_owner_id.uuid() != self.owner.owner_id
            || source.filesystem_domain.os_boot != self.owner.os_boot
            || !matches!(self.owner.os_boot, BootIdentity::Known { .. })
        {
            return Err(OperationError::State);
        }
        let mut copies = source
            .planned_copies
            .iter()
            .filter(|copy| matches!(copy.owner, v2::CopyOwner::DaemonSourceCell { .. }));
        let copy = copies.next().ok_or(OperationError::State)?;
        if copies.next().is_some() {
            return Err(OperationError::State);
        }
        let acquisition_id = copy.acquisition_operation_id;
        role.sources.insert(
            member,
            SourceCell {
                spec: request.take(),
                plan,
                acquisition_id,
                observation_id: Uuid::new_v4(),
                pending: None,
                result: None,
                failed_join: false,
                observed_committed: false,
                lease_issued: false,
                root_observation_id: Uuid::new_v4(),
                root_observed_committed: false,
                selected_root_member: None,
                pending_root: None,
                parent_close_operation_id: Uuid::new_v4(),
                parent_close: ParentCloseState::NotStarted,
            },
        );
        Ok(())
    }

    /// Known prepare/arm precedes the native open. The returned nonclone lease
    /// appears only after the original native observation is durably committed.
    pub(crate) async fn open_prepared_source(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        deadline: Instant,
    ) -> Result<RetainedSourceLease, OperationError> {
        self.acquire_prepared_source(owner, index, member, deadline)
            .await?;
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        if !bundle.installed {
            return Err(OperationError::State);
        }
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        let source = role.sources.get_mut(&member).ok_or(OperationError::State)?;
        if role.refunded
            || !matches!(role.phase, Phase::AcquisitionPossible | Phase::Sealed)
            || !source.observed_committed
            || source.lease_issued
        {
            return Err(OperationError::State);
        }
        let file = source
            .result
            .as_ref()
            .and_then(|r| r.file.as_ref())
            .ok_or(OperationError::State)?;
        source.lease_issued = true;
        Ok(RetainedSourceLease {
            file: Arc::clone(file),
        })
    }

    /// Runs source prepare/arm/open/observe and durable observation, without
    /// exporting the retained File through a lease.
    pub(in crate::adaptive_resources::runtime) async fn acquire_prepared_source(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        if !bundle.installed {
            return Err(OperationError::State);
        }
        let needs_entry = bundle
            .roles
            .get(index)
            .and_then(|r| r.sources.get(&member))
            .is_some_and(|source| source.spec.is_some());
        if needs_entry {
            self.eligible(&bundle)?;
        }
        let bundle_id = bundle.id;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        if role.refunded {
            return Err(OperationError::State);
        }
        if let Some(transition) = &role.transition {
            if !matches!(transition.effect, Effect::Arm(m) | Effect::Source(m) if m == member) {
                return Err(OperationError::State);
            }
            self.finish_transition(bundle_id, role).await?;
        }
        let source = role.sources.get(&member).ok_or(OperationError::State)?;
        if !role.armed.contains(&member) {
            if !matches!(role.phase, Phase::Prepared | Phase::AcquisitionPossible)
                || Instant::now() >= deadline
            {
                return Err(OperationError::Deadline);
            }
            start_named_transition(
                &self.journal,
                role,
                source.acquisition_id.uuid(),
                Mutation::Arm {
                    members: vec![member],
                },
                Effect::Arm(member),
                deadline,
            )?;
            self.finish_transition(bundle_id, role).await?;
        }
        let source = role.sources.get_mut(&member).ok_or(OperationError::State)?;
        if source.spec.is_some() {
            if !matches!(role.phase, Phase::AcquisitionPossible) || Instant::now() >= deadline {
                return Err(OperationError::Deadline);
            }
            let enter = source.launch()?;
            self.release_native_entry(role.binding, enter)?;
        }
        source.join().await?;
        if !source.observed_committed {
            let result = source.result.as_ref().ok_or(OperationError::State)?;
            let observed = result
                .observed
                .as_ref()
                .ok_or(OperationError::State)?
                .clone();
            let operation = source.observation_id;
            let envelope = v2::Envelope {
                version: 2,
                planned: source.plan.clone(),
                observed: Some(v2::Observed::SourceV1 { source: observed }),
            };
            start_named_transition(
                &self.journal,
                role,
                operation,
                Mutation::RecordTypedAcquired { envelope },
                Effect::Source(member),
                deadline,
            )?;
            self.finish_transition(bundle_id, role).await?;
        }
        if !matches!(role.phase, Phase::AcquisitionPossible | Phase::Sealed)
            || !role
                .sources
                .get(&member)
                .is_some_and(|source| source.observed_committed)
        {
            return Err(OperationError::State);
        }
        Ok(())
    }
}

impl SourceCell {
    pub(super) fn inherited_source(
        &mut self,
        root_member: Uuid,
    ) -> Result<InheritedSource, OperationError> {
        if !self.observed_committed || self.lease_issued {
            return Err(OperationError::State);
        }
        let v2::MemberPlan::SourceV1 { source: plan, .. } = &self.plan else {
            return Err(OperationError::State);
        };
        let result = self.result.as_ref().ok_or(OperationError::State)?;
        let file = result.file.as_ref().ok_or(OperationError::State)?;
        let source_observed = result.observed.as_ref().ok_or(OperationError::State)?;
        let metadata = file.metadata()?;
        let native = &source_observed.native_object;
        if !metadata.is_file()
            || source_observed.holder_id != plan.holder_id
            || source_observed.member_id != plan.member_id
            || source_observed.source_association != plan.source_association
            || source_observed.access != v2::SourceAccess::ReadOnlyRegular
            || native.file_type != v2::FileType::Regular
            || native.filesystem_domain != plan.filesystem_domain
            || native.device
                != (v2::NativeValue::Known {
                    value: metadata.dev(),
                })
            || native.inode
                != (v2::NativeValue::Known {
                    value: metadata.ino(),
                })
            || native.mount_id != bootstrap::native::steps::mount_id(file)
            || SourceIdentity::from_metadata(&metadata).render_s1()
                != source_observed.source_association.object_version
        {
            return Err(OperationError::State);
        }
        let planned_graph_id = match &plan.recovery {
            v2::SourceRecovery::LocalSourceHandlesV1 { graph_id, .. } => *graph_id,
        };
        let mut source_copies = source_observed.copies.iter().filter(|copy| {
            matches!(copy.owner,
                v2::CopyOwner::DaemonSourceCell { owner_id, graph_id, member_id }
                    if owner_id == plan.filesystem_domain.daemon_owner_id
                        && graph_id == planned_graph_id
                        && member_id == plan.member_id)
        });
        let source_copy = source_copies.next().ok_or(OperationError::State)?;
        if source_copies.next().is_some() || !plan.planned_copies.contains(source_copy) {
            return Err(OperationError::State);
        }
        let mut root_copies = plan.planned_copies.iter().filter(|copy| {
            matches!(copy.owner,
                v2::CopyOwner::OwnedRoot { graph_id, root_member_id: planned_root }
                    if graph_id == planned_graph_id && planned_root.uuid() == root_member)
                && copy.parent_copy_id == Some(source_copy.copy_id)
        });
        let root_copy = root_copies.next().ok_or(OperationError::State)?;
        if root_copies.next().is_some() {
            return Err(OperationError::State);
        }
        let observed = if let Some((selected, staged_copy, staged_observed)) = &self.pending_root {
            if *selected != root_member || staged_copy != root_copy {
                return Err(OperationError::State);
            }
            staged_observed.clone()
        } else if self.root_observed_committed {
            if self.selected_root_member != Some(root_member)
                || !source_observed.copies.contains(root_copy)
            {
                return Err(OperationError::State);
            }
            source_observed.clone()
        } else {
            if source_observed
                .copies
                .iter()
                .any(|copy| copy.copy_id == root_copy.copy_id)
            {
                return Err(OperationError::State);
            }
            let mut observed = source_observed.clone();
            observed.copies.push(root_copy.clone());
            self.pending_root = Some((root_member, root_copy.clone(), observed.clone()));
            observed
        };
        if self.root_observed_committed && self.selected_root_member != Some(root_member) {
            return Err(OperationError::State);
        }
        Ok(InheritedSource {
            raw_fd: file.as_raw_fd(),
            acquisition_id: root_copy.acquisition_operation_id,
            observation_id: self.root_observation_id,
            envelope: v2::Envelope {
                version: 2,
                planned: self.plan.clone(),
                observed: Some(v2::Observed::SourceV1 { source: observed }),
            },
            acknowledged: self.root_observed_committed,
        })
    }

    /// Closes only this retained parent descriptor. A failed/ambiguous close is
    /// terminal because retrying its numeric fd could close a recycled handle.
    pub(super) async fn close_parent(&mut self) -> Result<(), OperationError> {
        match &self.parent_close {
            ParentCloseState::Closed => return Ok(()),
            ParentCloseState::Failed(errno) => {
                return Err(std::io::Error::from_raw_os_error(*errno).into());
            }
            ParentCloseState::Unknown => return Err(OperationError::State),
            ParentCloseState::NotStarted | ParentCloseState::Registered { .. } => {}
        }

        if matches!(&self.parent_close, ParentCloseState::NotStarted) {
            // Refuse before taking the File on unsupported platforms or when
            // the source still has another exported owner.
            if !cfg!(target_os = "linux")
                || self.lease_issued
                || !self.observed_committed
                || self.pending.is_some()
                || self.spec.is_some()
            {
                return Err(OperationError::State);
            }
            let runtime = tokio::runtime::Handle::try_current().map_err(std::io::Error::other)?;
            let result = self.result.as_mut().ok_or(OperationError::State)?;
            if result.observed.is_none() {
                return Err(OperationError::State);
            }
            let file = result.file.take().ok_or(OperationError::State)?;
            let file = match Arc::try_unwrap(file) {
                Ok(file) => file,
                Err(file) => {
                    result.file = Some(file);
                    return Err(OperationError::State);
                }
            };
            let (enter, registered) = tokio::sync::oneshot::channel();
            let task = runtime.spawn_blocking(move || {
                if registered.blocking_recv().is_err() {
                    return ParentCloseOutcome::NotEntered;
                }
                let raw_fd = file.into_raw_fd();
                // SAFETY: this worker owns the original File allocation, has
                // converted it to a raw descriptor, and calls close once.
                let result = unsafe { libc::close(raw_fd) };
                if result == 0 {
                    ParentCloseOutcome::Closed
                } else if let Some(errno) = std::io::Error::last_os_error().raw_os_error() {
                    ParentCloseOutcome::Failed(errno)
                } else {
                    ParentCloseOutcome::Unknown
                }
            });
            self.parent_close = ParentCloseState::Registered {
                task,
                enter: Some(enter),
            };
        }

        if let ParentCloseState::Registered { enter, .. } = &mut self.parent_close {
            if let Some(enter) = enter.take() {
                // The task handle and latch are already in this SourceCell.
                // A failed send is joined below and recorded as unknown.
                let _ = enter.send(());
            }
        }

        let outcome = match &mut self.parent_close {
            ParentCloseState::Registered { task, .. } => match task.await {
                Ok(outcome) => outcome,
                Err(_) => ParentCloseOutcome::Unknown,
            },
            _ => return Err(OperationError::State),
        };
        self.parent_close = match outcome {
            ParentCloseOutcome::Closed => ParentCloseState::Closed,
            ParentCloseOutcome::Failed(errno) => ParentCloseState::Failed(errno),
            ParentCloseOutcome::NotEntered | ParentCloseOutcome::Unknown => {
                ParentCloseState::Unknown
            }
        };
        match &self.parent_close {
            ParentCloseState::Closed => Ok(()),
            ParentCloseState::Failed(errno) => {
                Err(std::io::Error::from_raw_os_error(*errno).into())
            }
            ParentCloseState::Unknown => Err(OperationError::State),
            ParentCloseState::NotStarted | ParentCloseState::Registered { .. } => {
                Err(OperationError::State)
            }
        }
    }

    pub(super) fn parent_closed(&self) -> bool {
        matches!(&self.parent_close, ParentCloseState::Closed)
    }

    pub(super) fn parent_close_operation_id(&self) -> Uuid {
        self.parent_close_operation_id
    }
}

pub(super) fn acknowledge(
    role: &mut OwnedRole,
    member: Uuid,
    operation: Uuid,
) -> Result<(), OperationError> {
    let source = role.sources.get_mut(&member).ok_or(OperationError::State)?;
    if source.observation_id != operation
        || source
            .result
            .as_ref()
            .is_none_or(|r| r.file.is_none() || r.observed.is_none())
    {
        return Err(OperationError::State);
    }
    source.observed_committed = true;
    Ok(())
}

pub(super) fn acknowledge_root(
    role: &mut OwnedRole,
    member: Uuid,
    operation: Uuid,
) -> Result<(), OperationError> {
    let source = role.sources.get_mut(&member).ok_or(OperationError::State)?;
    if source.root_observation_id != operation
        || !source.observed_committed
        || source.lease_issued
        || source
            .result
            .as_ref()
            .is_none_or(|result| result.file.is_none() || result.observed.is_none())
    {
        return Err(OperationError::State);
    }
    if source.root_observed_committed {
        return if source.selected_root_member.is_some() && source.pending_root.is_none() {
            Ok(())
        } else {
            Err(OperationError::State)
        };
    }
    let (root_member, root_copy, observed) = source
        .pending_root
        .as_ref()
        .cloned()
        .ok_or(OperationError::State)?;
    let v2::MemberPlan::SourceV1 { source: plan, .. } = &source.plan else {
        return Err(OperationError::State);
    };
    let base = source
        .result
        .as_ref()
        .and_then(|result| result.observed.as_ref())
        .ok_or(OperationError::State)?;
    let planned_graph_id = match &plan.recovery {
        v2::SourceRecovery::LocalSourceHandlesV1 { graph_id, .. } => *graph_id,
    };
    let mut source_copies = base.copies.iter().filter(|copy| {
        matches!(copy.owner,
            v2::CopyOwner::DaemonSourceCell { owner_id, graph_id, member_id }
                if owner_id == plan.filesystem_domain.daemon_owner_id
                    && graph_id == planned_graph_id
                    && member_id == plan.member_id)
    });
    let source_copy = source_copies.next().ok_or(OperationError::State)?;
    let added: Vec<_> = observed
        .copies
        .iter()
        .filter(|copy| !base.copies.contains(copy))
        .collect();
    if added.len() != 1
        || added[0] != &root_copy
        || !matches!(added[0].owner,
            v2::CopyOwner::OwnedRoot { graph_id, root_member_id }
                if graph_id == planned_graph_id && root_member_id.uuid() == root_member)
        || source_copies.next().is_some()
        || !plan.planned_copies.contains(&root_copy)
        || added[0].parent_copy_id != Some(source_copy.copy_id)
    {
        return Err(OperationError::State);
    }
    source
        .result
        .as_mut()
        .ok_or(OperationError::State)?
        .observed = Some(observed);
    source.selected_root_member = Some(root_member);
    source.root_observed_committed = true;
    source.pending_root = None;
    Ok(())
}

#[cfg(test)]
mod tests;
