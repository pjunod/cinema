//! Source acquisition in the original retained bundle, before opening a path.
//! A returned lease lends the SAME descriptor; its Arc and close authority stay
//! private to this counted custodian. It is neither cloneable nor deserializable.
use super::*;
use crate::source_identity::SourceIdentity;
use std::fs::File;
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
}
struct SourceResult {
    file: Option<Arc<File>>,
    observed: Option<v2::SourceObserved>,
}
impl SourceCell {
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
        if !matches!(role.phase, Phase::AcquisitionPossible | Phase::Sealed) {
            return Err(OperationError::State);
        }
        let source = role.sources.get_mut(&member).ok_or(OperationError::State)?;
        if !source.observed_committed || source.lease_issued {
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

#[cfg(test)]
mod tests;
