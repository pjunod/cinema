//! Actual original blocking root-open job; no fd/path authority from a DTO.
//! Component traversal/child creation must extend THIS retained cell, not reopen
//! the configured pathname or duplicate a handle outside the admitted slots.
use super::*;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use tokio::task::JoinHandle;
pub(super) mod cleanup;
pub(super) mod publication;
pub(super) mod readers;
pub(in crate::adaptive_resources::runtime::operations) mod steps;
pub(super) mod sync;
#[cfg(test)]
mod tests;

pub(super) struct RootCell {
    io_id: v2::Id,
    observe_id: v2::Id,
    join_id: v2::Id,
    pending: Option<JoinHandle<RootResult>>,
    result: Option<RootResult>,
    failed_join: bool,
    imported_retained: bool,
}
struct RootResult {
    // Even a partial fstat failure keeps the ACTUAL descriptor, not only JSON.
    files: BTreeMap<v2::Id, File>,
    audit: BootstrapResultV1,
}
pub(super) fn native_metadata_bytes(native: &v2::NativeObject) -> Option<usize> {
    let mut bytes = 0usize;
    for value in [
        &native.device,
        &native.inode,
        &native.mount_id,
        &native.filesystem_domain.mount_namespace,
    ] {
        if let v2::NativeValue::Unknown { reason } = value {
            bytes = bytes.checked_add(reason.capacity())?;
        }
    }
    bytes.checked_add(match &native.filesystem_domain.os_boot {
        crate::resource_custody::BootIdentity::Known { provider, .. } => provider.capacity(),
        crate::resource_custody::BootIdentity::Unknown { reason } => reason.capacity(),
    })
}

pub(super) fn directory_metadata_bytes(
    directory: &v2::scratch::DirectoryObservedV1,
) -> Option<usize> {
    directory
        .held_copy_ids
        .capacity()
        .checked_mul(std::mem::size_of::<v2::Id>())?
        .checked_add(native_metadata_bytes(&directory.native_object)?)
}
impl RootCell {
    pub(super) fn retained_heap_bytes(&self) -> Result<usize, OperationError> {
        if self.pending.is_some() || self.failed_join {
            return Err(OperationError::State);
        }
        let result = self.result.as_ref().ok_or(OperationError::State)?;
        let files = super::btree_requested_upper_bound::<v2::Id, File>(result.files.len())
            .ok_or(OperationError::State)?;
        let closed = result
            .audit
            .closed_copy_ids
            .capacity()
            .checked_mul(std::mem::size_of::<v2::Id>())
            .ok_or(OperationError::State)?;
        let acquired = result
            .audit
            .acquired
            .as_ref()
            .ok_or(OperationError::State)?;
        files
            .checked_add(closed)
            .and_then(|n| n.checked_add(directory_metadata_bytes(acquired)?))
            .ok_or(OperationError::State)
    }
    /// Import the two actual descriptors held by a journal-verified retained
    /// adoption. These are newly held native owners, not replayed historical
    /// root-open jobs. The adoption id is only their local baseline.
    #[cfg(target_os = "linux")]
    pub(super) fn from_recovered(
        parent: File,
        child: File,
        parent_observed: &v2::scratch::DirectoryObservedV1,
        child_observed: &v2::scratch::DirectoryObservedV1,
        adoption_id: v2::Id,
    ) -> Self {
        // RecoveredNativeParts can be consumed only after its source owner
        // validates both native objects and exact one-copy inventory. There
        // is no fallible step after moving either File out of that owner.
        let files = BTreeMap::from([
            (parent_observed.held_copy_ids[0], parent),
            (child_observed.held_copy_ids[0], child),
        ]);
        Self {
            io_id: adoption_id,
            observe_id: adoption_id,
            join_id: adoption_id,
            pending: None,
            result: Some(RootResult {
                files,
                audit: BootstrapResultV1 {
                    outcome: v2::scratch::NativeOutcomeV1::Completed,
                    acquired: Some(child_observed.clone()),
                    closed_copy_ids: vec![],
                    child_effect: ChildEffect::Unchanged,
                    inventory: InventoryCertainty::Exact,
                },
            }),
            failed_join: false,
            imported_retained: true,
        }
    }

    pub(super) fn imported_retained(&self) -> bool {
        self.imported_retained
    }

    pub(super) fn initial_root_never_entered(&self) -> bool {
        self.pending.is_none()
            && !self.failed_join
            && self.result.as_ref().is_some_and(|result| {
                result.files.is_empty()
                    && result.audit.outcome == v2::scratch::NativeOutcomeV1::NeverEntered
                    && result.audit.acquired.is_none()
                    && result.audit.closed_copy_ids.is_empty()
                    && result.audit.child_effect == ChildEffect::None
                    && result.audit.inventory == InventoryCertainty::Exact
            })
    }

    pub(super) fn retained_child_ready(
        &self,
        expected_directories: &[v2::scratch::DirectoryObservedV1],
    ) -> bool {
        if self.pending.is_some() || self.failed_join {
            return false;
        }
        let Some(result) = self.result.as_ref() else {
            return false;
        };
        if expected_directories.is_empty()
            || result.audit.outcome != v2::scratch::NativeOutcomeV1::Completed
            || result.audit.inventory != InventoryCertainty::Exact
            || result.audit.acquired.as_ref() != expected_directories.last()
        {
            return false;
        }
        let copies: Vec<_> = expected_directories
            .iter()
            .flat_map(|directory| directory.held_copy_ids.iter().copied())
            .collect();
        if copies
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != copies.len()
            || result.files.len() != copies.len()
        {
            return false;
        }
        expected_directories.iter().all(|directory| {
            directory.held_copy_ids.iter().all(|copy| {
                let Some(file) = result.files.get(copy) else {
                    return false;
                };
                file.metadata().is_ok_and(|metadata| {
                    metadata.is_dir()
                        && directory.native_object.file_type == v2::FileType::Directory
                        && directory.native_object.device
                            == (v2::NativeValue::Known {
                                value: metadata.dev(),
                            })
                        && directory.native_object.inode
                            == (v2::NativeValue::Known {
                                value: metadata.ino(),
                            })
                        && directory.native_object.mount_id == steps::mount_id(file)
                })
            })
        })
    }

    /// Lend the SAME charged child descriptor to one registered reader-open job.
    /// No pathname fallback or descriptor duplication is possible.
    pub(super) fn lend_held_child(
        &mut self,
        directory: &v2::scratch::DirectoryObservedV1,
    ) -> Result<File, OperationError> {
        if self.pending.is_some() || self.failed_join || directory.held_copy_ids.len() != 1 {
            return Err(OperationError::State);
        }
        let held = self.result.as_mut().ok_or(OperationError::State)?;
        if held.audit.acquired.as_ref() != Some(directory)
            || held.audit.outcome != v2::scratch::NativeOutcomeV1::Completed
        {
            return Err(OperationError::State);
        }
        held.files
            .remove(&directory.held_copy_ids[0])
            .ok_or(OperationError::State)
    }

    /// Return the original lent descriptor after the original open job joins.
    /// A failed validation leaves the File in `file` for the caller to retain.
    pub(super) fn return_held_child(
        &mut self,
        directory: &v2::scratch::DirectoryObservedV1,
        file: &mut Option<File>,
    ) -> Result<(), OperationError> {
        if self.pending.is_some() || self.failed_join || directory.held_copy_ids.len() != 1 {
            return Err(OperationError::State);
        }
        let held = self.result.as_mut().ok_or(OperationError::State)?;
        let copy = directory.held_copy_ids[0];
        if held.audit.acquired.as_ref() != Some(directory) || held.files.contains_key(&copy) {
            return Err(OperationError::State);
        }
        let actual = file.as_ref().ok_or(OperationError::State)?;
        let metadata = actual.metadata()?;
        if !metadata.is_dir()
            || directory.native_object.device
                != (v2::NativeValue::Known {
                    value: metadata.dev(),
                })
            || directory.native_object.inode
                != (v2::NativeValue::Known {
                    value: metadata.ino(),
                })
            || directory.native_object.mount_id != steps::mount_id(actual)
        {
            return Err(OperationError::State);
        }
        held.files
            .insert(copy, file.take().expect("validated original child"));
        Ok(())
    }

    /// Validate the complete original directory inventory, then lend only the
    /// action's exact subset. Unused originals remain here throughout the job.
    pub(super) fn lend_observed_directories(
        &mut self,
        observed: &[v2::scratch::DirectoryObservedV1],
        baseline: &[Option<&v2::scratch::DirectoryObservedV1>; 3],
    ) -> Result<BTreeMap<v2::Id, File>, OperationError> {
        if self.pending.is_some() || self.failed_join || observed.is_empty() {
            return Err(OperationError::State);
        }
        let held = self.result.as_mut().ok_or(OperationError::State)?;
        let last_known = held.audit.outcome == v2::scratch::NativeOutcomeV1::Completed
            && held.audit.acquired.as_ref() == baseline.iter().rev().flatten().copied().next();
        let never_entered = held.audit.outcome == v2::scratch::NativeOutcomeV1::NeverEntered
            && held.audit.acquired.is_none()
            && held.audit.closed_copy_ids.is_empty()
            && held.audit.child_effect == ChildEffect::None
            && held.audit.inventory == InventoryCertainty::Exact;
        // A cancelled next-step latch reports NeverEntered but leaves the
        // preceding original directory File in this cell. Its exact copy ID
        // and native identity are checked below before any File is moved.
        if (!last_known && !never_entered) || held.files.len() != baseline.iter().flatten().count()
        {
            return Err(OperationError::State);
        }
        let domain = &observed[0].native_object.filesystem_domain;
        let mut copies = std::collections::BTreeSet::new();
        for directory in baseline.iter().flatten() {
            if directory.held_copy_ids.len() != 1
                || &directory.native_object.filesystem_domain != domain
            {
                return Err(OperationError::State);
            }
            let copy = directory.held_copy_ids[0];
            if !copies.insert(copy) {
                return Err(OperationError::State);
            }
            let file = held.files.get(&copy).ok_or(OperationError::State)?;
            let metadata = file.metadata()?;
            if !metadata.is_dir()
                || directory.native_object.file_type != v2::FileType::Directory
                || directory.native_object.device
                    != (v2::NativeValue::Known {
                        value: metadata.dev(),
                    })
                || directory.native_object.inode
                    != (v2::NativeValue::Known {
                        value: metadata.ino(),
                    })
                || !matches!(
                    (&directory.native_object.mount_id, steps::mount_id(file)),
                    (v2::NativeValue::Known { value }, v2::NativeValue::Known { value: actual })
                        if *value == actual
                )
            {
                return Err(OperationError::State);
            }
        }
        // Subset membership includes the whole observed identity, not merely
        // a copy ID. Validate all requested moves before taking any File.
        copies.clear();
        for directory in observed {
            if !baseline
                .iter()
                .flatten()
                .any(|expected| *expected == directory)
                || !copies.insert(directory.held_copy_ids[0])
            {
                return Err(OperationError::State);
            }
        }
        Ok(copies
            .into_iter()
            .map(|copy| {
                (
                    copy,
                    held.files.remove(&copy).expect("prevalidated held File"),
                )
            })
            .collect())
    }

    /// Restore original lent directories only after the matching cleanup
    /// Observe is durable. A rejection leaves all Files in the caller's map.
    pub(super) fn return_observed_directories(
        &mut self,
        observed: &[v2::scratch::DirectoryObservedV1],
        files: &mut BTreeMap<v2::Id, File>,
    ) -> Result<(), OperationError> {
        if self.pending.is_some() || self.failed_join || observed.len() != files.len() {
            return Err(OperationError::State);
        }
        let held = self.result.as_mut().ok_or(OperationError::State)?;
        for directory in observed {
            if directory.held_copy_ids.len() != 1 {
                return Err(OperationError::State);
            }
            let copy = directory.held_copy_ids[0];
            let file = files.get(&copy).ok_or(OperationError::State)?;
            if held.files.contains_key(&copy) {
                return Err(OperationError::State);
            }
            let metadata = file.metadata()?;
            if !metadata.is_dir()
                || directory.native_object.device
                    != (v2::NativeValue::Known {
                        value: metadata.dev(),
                    })
                || directory.native_object.inode
                    != (v2::NativeValue::Known {
                        value: metadata.ino(),
                    })
                || directory.native_object.mount_id != steps::mount_id(file)
            {
                return Err(OperationError::State);
            }
        }
        for directory in observed {
            let copy = directory.held_copy_ids[0];
            held.files.insert(
                copy,
                files.remove(&copy).expect("prevalidated returned File"),
            );
        }
        Ok(())
    }

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
            imported_retained: false,
        };
        // The caller must register this original handle in OwnedRole before it
        // releases `enter`. If registration fails, no native call is entered.
        Ok((cell, enter))
    }
    /// Await only the original root job already registered in this cell.
    /// A joined audit/result remains retained here for the normal Observe path.
    pub(super) async fn join_registered(&mut self) -> Result<(), OperationError> {
        if self.failed_join {
            return Err(OperationError::State);
        }
        if self.pending.is_some() {
            self.join().await?;
        }
        Ok(())
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
        self.observe_scratch_bootstrap_root(owner, index, member, deadline, true)
            .await
    }

    /// Cancellation joins an already-entered original root-open job, or
    /// registers its still-unlaunched arm with a closed latch. The latter is
    /// an actual NeverEntered result, never a guessed missing descriptor.
    pub(in crate::adaptive_resources::runtime::operations) async fn observe_finite_bootstrap_root(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.observe_scratch_bootstrap_root(owner, index, member, deadline, false)
            .await
    }

    async fn observe_scratch_bootstrap_root(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        deadline: Instant,
        allow_entry: bool,
    ) -> Result<(), OperationError> {
        if !cfg!(target_os = "linux") {
            return Err(OperationError::State);
        }
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        if !bundle.installed {
            return Err(OperationError::State);
        }
        // Only fresh native entry requires current admission eligibility.
        // Finite cancellation joins the original root or drops a newly
        // registered latch to obtain NeverEntered; it never sends entry.
        if allow_entry {
            self.eligible(&bundle)?;
        }
        let bundle_id = bundle.id;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        if role.refunded
            || (!matches!(role.phase, Phase::AcquisitionPossible | Phase::Sealed)
                && (allow_entry || role.phase != Phase::UnknownHeld))
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
            || bootstrap.plan.filesystem_domain.platform != v2::FilesystemPlatform::Linux
            || bootstrap.plan.filesystem_domain.daemon_owner_id.uuid() != self.owner.owner_id
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
            if allow_entry {
                self.release_native_entry(role.binding, enter)?;
            } else {
                drop(enter);
            }
        }
        let native = bootstrap
            .native_root
            .as_mut()
            .ok_or(OperationError::State)?;
        let audit = native.join().await?.audit.clone();
        let completed = audit.outcome == v2::scratch::NativeOutcomeV1::Completed;
        let never_entered = !allow_entry && native.initial_root_never_entered();
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
            return if completed || never_entered {
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
        } else if never_entered {
            Ok(())
        } else {
            Err(OperationError::State)
        }
    }
}
