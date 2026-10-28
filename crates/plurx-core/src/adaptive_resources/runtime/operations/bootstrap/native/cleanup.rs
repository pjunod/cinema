//! One-attempt native retirement over the original held directory Files.
//! The enclosing runtime owes the exact durable Arm/Observe and ledger binding.
//! This module exports facts, never a physical retirement receipt or raw File.
use super::*;
use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::{AsRawFd, IntoRawFd};
use v2::scratch::bootstrap::{BootstrapResultV1, ChildEffect, InventoryCertainty, ScratchPlanV3};
use v2::scratch::{
    AssetObservationV1, DirectoryObservedV1, FileObservedV1, NativeOutcomeV1, OutputKey,
};

#[derive(Clone)]
enum CleanupAction {
    RemoveFile {
        file_id: v2::Id,
        basename: CString,
        native: v2::NativeObject,
        len: u64,
    },
    SyncChild,
    RemoveChild {
        component: CString,
        child_native: v2::NativeObject,
    },
    SyncParent,
    CloseHandles {
        exact_ids: Vec<v2::Id>,
    },
}

#[derive(Clone)]
pub(in crate::adaptive_resources::runtime::operations::bootstrap) enum CleanupFact {
    Asset {
        outcome: NativeOutcomeV1,
        observation: AssetObservationV1,
    },
    Bootstrap(BootstrapResultV1),
}

struct CleanupResult {
    directories: BTreeMap<v2::Id, File>,
    fact: Option<CleanupFact>,
    // These facts are retained for diagnosis only. A partial close has no
    // positive aggregate result and cannot refund any exact copy slot.
    known_closed: Vec<v2::Id>,
    unknown_close: Option<v2::Id>,
}

pub(in crate::adaptive_resources::runtime::operations::bootstrap) struct CleanupCell {
    action: CleanupAction,
    observed: Vec<DirectoryObservedV1>,
    pending: Option<JoinHandle<CleanupResult>>,
    result: Option<CleanupResult>,
    failed_join: bool,
    launched: bool,
    acknowledged: bool,
}

fn invalid() -> OperationError {
    OperationError::State
}
fn failed() -> NativeOutcomeV1 {
    NativeOutcomeV1::Failed {
        error_class: "retained native cleanup unresolved".into(),
    }
}
fn component(value: &str) -> Result<CString, OperationError> {
    if value.is_empty() || value == "." || value == ".." || value.contains('/') || value.len() > 255
    {
        return Err(invalid());
    }
    CString::new(value).map_err(|_| invalid())
}
fn basename(output: OutputKey) -> Result<CString, OperationError> {
    let name = match output {
        OutputKey::Init => "init.mp4".to_owned(),
        OutputKey::Segment { index } => format!("seg{index:05}.m4s"),
        OutputKey::Playlist => "index.m3u8".to_owned(),
        OutputKey::IdentityMetadata => "identity.json".to_owned(),
        OutputKey::EncodedProcessMetadata => "encoded-process".to_owned(),
        OutputKey::CompleteArtifactManifest => "complete-v1.json".to_owned(),
    };
    component(&name)
}
fn single_copy(directory: &DirectoryObservedV1) -> Result<v2::Id, OperationError> {
    if directory.held_copy_ids.len() != 1
        || directory.native_object.file_type != v2::FileType::Directory
    {
        return Err(invalid());
    }
    Ok(directory.held_copy_ids[0])
}
impl RootCell {
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn no_original_handles(
        &self,
    ) -> bool {
        self.pending.is_none()
            && !self.failed_join
            && self
                .result
                .as_ref()
                .is_some_and(|result| result.files.is_empty())
    }
}
impl CleanupCell {
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn launched(&self) -> bool {
        self.launched
    }
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn positive_close_acknowledged(
        &self,
    ) -> bool {
        self.acknowledged
            && self.pending.is_none()
            && !self.failed_join
            && matches!(&self.action, CleanupAction::CloseHandles { exact_ids }
                if self.result.as_ref().is_some_and(|result|
                    result.directories.is_empty()
                    && result.unknown_close.is_none()
                    && &result.known_closed == exact_ids
                    && matches!(&result.fact, Some(CleanupFact::Bootstrap(fact))
                        if fact.outcome == NativeOutcomeV1::Completed
                            && fact.closed_copy_ids.as_slice() == exact_ids.as_slice())))
    }
    fn new(action: CleanupAction, observed: Vec<DirectoryObservedV1>) -> Self {
        Self {
            action,
            observed,
            pending: None,
            result: None,
            failed_join: false,
            launched: false,
            acknowledged: false,
        }
    }
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn remove_owned_file(
        file: &FileObservedV1,
        child: &DirectoryObservedV1,
    ) -> Result<Self, OperationError> {
        single_copy(child)?;
        if file.temporary
            || !file.held_copy_ids.is_empty()
            || file.native_object.file_type != v2::FileType::Regular
            || file.native_object.filesystem_domain != child.native_object.filesystem_domain
        {
            return Err(invalid());
        }
        let output = file.published.ok_or_else(invalid)?;
        Ok(Self::new(
            CleanupAction::RemoveFile {
                file_id: file.file_id,
                basename: basename(output)?,
                native: file.native_object.clone(),
                len: file.logical_len,
            },
            vec![child.clone()],
        ))
    }
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn sync_child(
        child: &DirectoryObservedV1,
    ) -> Result<Self, OperationError> {
        single_copy(child)?;
        Ok(Self::new(CleanupAction::SyncChild, vec![child.clone()]))
    }
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn remove_child(
        plan: &ScratchPlanV3,
        parent: &DirectoryObservedV1,
        child: &DirectoryObservedV1,
    ) -> Result<Self, OperationError> {
        let parent_copy = single_copy(parent)?;
        let child_copy = single_copy(child)?;
        if parent_copy == child_copy
            || parent.native_object.filesystem_domain != child.native_object.filesystem_domain
        {
            return Err(invalid());
        }
        Ok(Self::new(
            CleanupAction::RemoveChild {
                component: component(&plan.bootstrap().child_component)?,
                child_native: child.native_object.clone(),
            },
            vec![parent.clone(), child.clone()],
        ))
    }
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn sync_parent(
        parent: &DirectoryObservedV1,
    ) -> Result<Self, OperationError> {
        single_copy(parent)?;
        Ok(Self::new(CleanupAction::SyncParent, vec![parent.clone()]))
    }
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn close_retained(
        observed: &[DirectoryObservedV1],
        exact_ids: &[v2::Id],
    ) -> Result<Self, OperationError> {
        if observed.is_empty() {
            return Err(invalid());
        }
        let mut copies = observed
            .iter()
            .map(single_copy)
            .collect::<Result<Vec<_>, _>>()?;
        copies.sort_unstable();
        copies.dedup();
        if copies.len() != observed.len() || copies != exact_ids {
            return Err(invalid());
        }
        Ok(Self::new(
            CleanupAction::CloseHandles { exact_ids: copies },
            observed.to_vec(),
        ))
    }

    /// The worker and ALL original Files are held here before the entry latch
    /// is returned. No callback, path reopen, or fd duplication is involved.
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn launch(
        &mut self,
        root: &mut RootCell,
        baseline: &[Option<&DirectoryObservedV1>; 3],
    ) -> Result<tokio::sync::oneshot::Sender<()>, OperationError> {
        if self.launched || self.pending.is_some() || self.result.is_some() || self.failed_join {
            return Err(invalid());
        }
        let runtime = tokio::runtime::Handle::try_current().map_err(io::Error::other)?;
        let action = self.action.clone();
        let observed = self.observed.clone();
        let directories = root.lend_observed_directories(&self.observed, baseline)?;
        let (enter, registered) = tokio::sync::oneshot::channel();
        self.pending = Some(runtime.spawn_blocking(move || {
            if registered.blocking_recv().is_err() {
                return CleanupResult {
                    directories,
                    fact: Some(never_entered_fact(&action)),
                    known_closed: vec![],
                    unknown_close: None,
                };
            }
            run(action, observed, directories)
        }));
        self.launched = true;
        Ok(enter)
    }
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) async fn join_registered(
        &mut self,
    ) -> Result<(), OperationError> {
        if self.failed_join {
            return Err(invalid());
        }
        if let Some(pending) = self.pending.as_mut() {
            let joined = pending.await;
            self.pending = None;
            match joined {
                Ok(result) => self.result = Some(result),
                Err(_) => {
                    self.failed_join = true;
                    return Err(invalid());
                }
            }
        }
        if self.result.is_some() {
            Ok(())
        } else {
            Err(invalid())
        }
    }
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn fact(
        &self,
    ) -> Option<CleanupFact> {
        self.result.as_ref().and_then(|r| r.fact.clone())
    }
    #[expect(
        dead_code,
        reason = "Partial-close diagnostic consumption is not connected; the retained result still owns unattempted Files and cannot issue an aggregate receipt."
    )]
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn partial_close_facts(
        &self,
    ) -> Option<(&[v2::Id], Option<v2::Id>)> {
        self.result
            .as_ref()
            .map(|r| (r.known_closed.as_slice(), r.unknown_close))
    }
    /// After durable Observe, return all still-live original Files. In the
    /// all-closed case there are none; failed close preserves only unattempted
    /// Files and leaves the entire charge unresolved.
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn acknowledge_after_observe(
        &mut self,
        root: &mut RootCell,
    ) -> Result<(), OperationError> {
        if self.acknowledged || self.failed_join || self.pending.is_some() {
            return Err(invalid());
        }
        let result = self.result.as_mut().ok_or_else(invalid)?;
        if result.fact.is_none() {
            return Err(invalid());
        }
        let still_held: Vec<_> = self
            .observed
            .iter()
            .filter(|d| {
                d.held_copy_ids.len() == 1 && result.directories.contains_key(&d.held_copy_ids[0])
            })
            .cloned()
            .collect();
        if !still_held.is_empty() {
            root.return_observed_directories(&still_held, &mut result.directories)?;
        }
        if !result.directories.is_empty() {
            return Err(invalid());
        }
        self.acknowledged = true;
        Ok(())
    }
}

fn never_entered_fact(action: &CleanupAction) -> CleanupFact {
    match action {
        CleanupAction::RemoveFile { .. } | CleanupAction::SyncChild => CleanupFact::Asset {
            outcome: NativeOutcomeV1::NeverEntered,
            observation: AssetObservationV1::NoAcquisition,
        },
        CleanupAction::RemoveChild { .. }
        | CleanupAction::SyncParent
        | CleanupAction::CloseHandles { .. } => CleanupFact::Bootstrap(BootstrapResultV1 {
            outcome: NativeOutcomeV1::NeverEntered,
            acquired: None,
            closed_copy_ids: vec![],
            child_effect: ChildEffect::None,
            inventory: InventoryCertainty::Exact,
        }),
    }
}

fn held<'a>(directories: &'a BTreeMap<v2::Id, File>, observed: &DirectoryObservedV1) -> &'a File {
    directories
        .get(&observed.held_copy_ids[0])
        .expect("prevalidated lent directory")
}
fn run(
    action: CleanupAction,
    observed: Vec<DirectoryObservedV1>,
    directories: BTreeMap<v2::Id, File>,
) -> CleanupResult {
    let mut result = CleanupResult {
        directories,
        fact: None,
        known_closed: vec![],
        unknown_close: None,
    };
    match action {
        CleanupAction::RemoveFile {
            file_id,
            basename,
            native,
            len,
        } => {
            let child = held(&result.directories, &observed[0]);
            let valid = entry_matches(child, &basename, &native, len, libc::S_IFREG);
            let removed =
                valid && unsafe { libc::unlinkat(child.as_raw_fd(), basename.as_ptr(), 0) } == 0;
            result.fact = Some(CleanupFact::Asset {
                outcome: if removed {
                    NativeOutcomeV1::Completed
                } else {
                    failed()
                },
                observation: if removed {
                    AssetObservationV1::Removed { file_id }
                } else {
                    AssetObservationV1::RemovalUnresolved { file_id }
                },
            });
        }
        CleanupAction::SyncChild => {
            let child = held(&result.directories, &observed[0]);
            let synced = unsafe { libc::fsync(child.as_raw_fd()) } == 0;
            result.fact = Some(CleanupFact::Asset {
                outcome: if synced {
                    NativeOutcomeV1::Completed
                } else {
                    failed()
                },
                observation: AssetObservationV1::Synced,
            });
        }
        CleanupAction::RemoveChild {
            component,
            child_native,
        } => {
            let parent = held(&result.directories, &observed[0]);
            let child = held(&result.directories, &observed[1]);
            let child_len = child.metadata().map_or(0, |m| m.len());
            let valid = entry_matches(parent, &component, &child_native, child_len, libc::S_IFDIR);
            let removed = valid
                && unsafe {
                    libc::unlinkat(parent.as_raw_fd(), component.as_ptr(), libc::AT_REMOVEDIR)
                } == 0;
            result.fact = Some(CleanupFact::Bootstrap(BootstrapResultV1 {
                outcome: if removed {
                    NativeOutcomeV1::Completed
                } else {
                    failed()
                },
                acquired: None,
                closed_copy_ids: vec![],
                child_effect: if removed {
                    ChildEffect::Removed
                } else if valid {
                    ChildEffect::Unknown
                } else {
                    ChildEffect::Unchanged
                },
                inventory: if removed {
                    InventoryCertainty::Exact
                } else {
                    InventoryCertainty::Unknown
                },
            }));
        }
        CleanupAction::SyncParent => {
            let parent = held(&result.directories, &observed[0]);
            let synced = unsafe { libc::fsync(parent.as_raw_fd()) } == 0;
            result.fact = Some(CleanupFact::Bootstrap(BootstrapResultV1 {
                outcome: if synced {
                    NativeOutcomeV1::Completed
                } else {
                    failed()
                },
                acquired: None,
                closed_copy_ids: vec![],
                child_effect: ChildEffect::None,
                inventory: if synced {
                    InventoryCertainty::Exact
                } else {
                    InventoryCertainty::Unknown
                },
            }));
        }
        CleanupAction::CloseHandles { exact_ids } => {
            for copy in &exact_ids {
                let file = result
                    .directories
                    .remove(copy)
                    .expect("prevalidated exact File");
                let fd = file.into_raw_fd();
                // SAFETY: this is the sole owned fd after into_raw_fd, called
                // exactly once. A negative result is terminally uncertain.
                let rc = unsafe { libc::close(fd) };
                if rc != 0 {
                    result.unknown_close = Some(*copy);
                    break;
                }
                result.known_closed.push(*copy);
            }
            let all_closed = result.unknown_close.is_none() && result.known_closed == exact_ids;
            result.fact = Some(CleanupFact::Bootstrap(BootstrapResultV1 {
                outcome: if all_closed {
                    NativeOutcomeV1::Completed
                } else {
                    failed()
                },
                acquired: None,
                closed_copy_ids: if all_closed { exact_ids } else { vec![] },
                child_effect: ChildEffect::None,
                inventory: if all_closed {
                    InventoryCertainty::Exact
                } else {
                    InventoryCertainty::Unknown
                },
            }));
        }
    }
    result
}

/// A precheck in the held namespace. It is not a conditional unlink primitive:
/// this first fixed profile trusts its owned namespace; an external conflicting
/// mutation or any failed check is classified unresolved, never retried.
// libc device/inode widths differ across supported Unix/Linux ABIs; retain
// the same explicit identity projection rather than specializing to x86_64.
#[allow(clippy::unnecessary_cast)]
fn entry_matches(
    directory: &File,
    name: &CStr,
    expected: &v2::NativeObject,
    len: u64,
    kind: libc::mode_t,
) -> bool {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: directory and canonical NUL-terminated component remain owned;
    // stat points at writable storage. No descriptor is opened or duplicated.
    if unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return false;
    }
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != kind
        || expected.device
            != (v2::NativeValue::Known {
                value: stat.st_dev as u64,
            })
        || expected.inode
            != (v2::NativeValue::Known {
                value: stat.st_ino as u64,
            })
        || stat.st_size < 0
        || stat.st_size as u64 != len
    {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        let mut extra = std::mem::MaybeUninit::<libc::statx>::zeroed();
        let rc = unsafe {
            libc::statx(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
                libc::STATX_MNT_ID,
                extra.as_mut_ptr(),
            )
        };
        if rc == 0 {
            let extra = unsafe { extra.assume_init() };
            return extra.stx_mask & libc::STATX_MNT_ID != 0
                && extra.stx_ino == stat.st_ino as u64
                && extra.stx_size == len
                && expected.mount_id
                    == v2::NativeValue::Known {
                        value: extra.stx_mnt_id,
                    };
        }
    }
    false
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::io::Write;

    fn id() -> v2::Id {
        v2::Id::new(Uuid::new_v4()).unwrap()
    }
    fn domain() -> v2::FilesystemDomain {
        v2::FilesystemDomain {
            platform: v2::FilesystemPlatform::Linux,
            domain_id: id(),
            daemon_owner_id: id(),
            os_boot: BootIdentity::Known {
                value: Uuid::new_v4(),
                provider: "fixture".into(),
                version: 1,
            },
            mount_namespace: v2::NativeValue::Unknown {
                reason: "native unit fixture".into(),
            },
        }
    }
    fn observed(file: &File, copy: v2::Id, domain: v2::FilesystemDomain) -> DirectoryObservedV1 {
        let metadata = file.metadata().unwrap();
        DirectoryObservedV1 {
            directory_id: id(),
            acquisition_io_id: id(),
            held_copy_ids: vec![copy],
            native_object: v2::NativeObject {
                file_type: v2::FileType::Directory,
                device: v2::NativeValue::Known {
                    value: metadata.dev(),
                },
                inode: v2::NativeValue::Known {
                    value: metadata.ino(),
                },
                mount_id: steps::mount_id(file),
                filesystem_domain: domain,
            },
        }
    }
    fn fixture() -> tempfile::TempDir {
        let target =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/cleanup-fixtures");
        std::fs::create_dir_all(&target).unwrap();
        tempfile::Builder::new()
            .prefix("cleanup-")
            .tempdir_in(target)
            .unwrap()
    }

    #[test]
    fn fixed_name_removal_rejects_replaced_inode_without_unlinking_it() {
        let temp = fixture();
        let directory = File::open(temp.path()).unwrap();
        let copy = id();
        let child = observed(&directory, copy, domain());
        let path = temp.path().join("init.mp4");
        let mut old = File::create(&path).unwrap();
        old.write_all(b"old!").unwrap();
        let metadata = old.metadata().unwrap();
        let native = v2::NativeObject {
            file_type: v2::FileType::Regular,
            device: v2::NativeValue::Known {
                value: metadata.dev(),
            },
            inode: v2::NativeValue::Known {
                value: metadata.ino(),
            },
            mount_id: steps::mount_id(&old),
            filesystem_domain: child.native_object.filesystem_domain.clone(),
        };
        std::fs::remove_file(&path).unwrap();
        let mut replacement = File::create(&path).unwrap();
        replacement.write_all(b"new!").unwrap();
        let file_id = id();
        let result = run(
            CleanupAction::RemoveFile {
                file_id,
                basename: CString::new("init.mp4").unwrap(),
                native,
                len: 4,
            },
            vec![child],
            BTreeMap::from([(copy, directory)]),
        );
        assert!(matches!(result.fact, Some(CleanupFact::Asset {
            outcome: NativeOutcomeV1::Failed { .. },
            observation: AssetObservationV1::RemovalUnresolved { file_id: observed },
        }) if observed == file_id));
        assert_eq!(std::fs::read(&path).unwrap(), b"new!");
        assert!(result.directories.contains_key(&copy));
    }

    #[test]
    fn all_original_close_calls_must_return_zero_for_positive_fact() {
        let temp = fixture();
        let directory = File::open(temp.path()).unwrap();
        let fd = directory.as_raw_fd();
        let copy = id();
        let observed = observed(&directory, copy, domain());
        let result = run(
            CleanupAction::CloseHandles {
                exact_ids: vec![copy],
            },
            vec![observed],
            BTreeMap::from([(copy, directory)]),
        );
        assert!(matches!(
            result.fact,
            Some(CleanupFact::Bootstrap(BootstrapResultV1 {
                outcome: NativeOutcomeV1::Completed,
                ..
            }))
        ));
        assert_eq!(result.known_closed, vec![copy]);
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
        assert!(result.directories.is_empty());
    }
}
