//! First supported recovery import: a dormant retained finite VOD output on
//! the original Linux boot. This owns reopened native Files but cannot settle
//! an old producer, mint a live reservation, or turn disk rows into receipts.
#![cfg(target_os = "linux")]

use super::*;
use crate::resource_custody::codec_v2::{self as v2, scratch::OutputKey};
use crate::resource_custody::recovery::{PositivePreviousOwnerExit, RetainedSImportClaim};
use std::collections::BTreeSet;
use std::ffi::{CStr, CString};
use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

/// Original physical objects newly held by the current daemon under the full
/// restored S charge. This is not a journal adoption or settlement receipt.
/// The runtime integration owner must retain it through serving/cleanup.
pub(crate) struct ImportedRetainedS {
    claim: RetainedSImportClaim,
    directories: Mutex<Option<(File, File)>>,
}

impl ImportedRetainedS {
    pub(crate) fn obligation_id(&self) -> Uuid {
        self.claim.obligation_id()
    }
    pub(crate) fn file_count(&self) -> usize {
        self.claim.full().files.len()
    }
}

/// Nonserde authority retained by the journal's original queued adoption. The
/// same Arc also remains in the runtime import map; neither cancellation nor a
/// lost acknowledgment can drop the actual reopened parent and child owners.
/// Only native import below constructs this capability.
pub(crate) struct NativeRetainedSImportProof {
    owner: Arc<ImportedRetainedS>,
    current_owner_id: Uuid,
    binding: ReservationBinding,
}
impl NativeRetainedSImportProof {
    pub(crate) fn claim(&self) -> &RetainedSImportClaim {
        &self.owner.claim
    }
    pub(crate) fn journal_id(&self) -> Uuid {
        self.owner.claim.journal_id()
    }
    pub(crate) fn installation_id(&self) -> Uuid {
        self.owner.claim.installation_id()
    }
    pub(crate) fn obligation_id(&self) -> Uuid {
        self.owner.claim.obligation_id()
    }
    pub(crate) fn previous_owner_id(&self) -> Uuid {
        self.owner.claim.old_owner_id()
    }
    pub(crate) fn old_owner_id(&self) -> Uuid {
        self.previous_owner_id()
    }
    pub(crate) fn current_owner_id(&self) -> Uuid {
        self.current_owner_id
    }
    pub(crate) fn binding(&self) -> ReservationBinding {
        self.binding
    }
    pub(crate) fn state_head(&self) -> &v2::scratch::AssetEventRefV1 {
        self.owner.claim.current_head()
    }
    pub(crate) fn frozen_digest(&self) -> &str {
        self.owner.claim.inventory_sha256()
    }
}

/// Known current custody after durable adoption. The original imported owner
/// remains retained while its actual Files move into the ordinary S adapter.
/// This authorizes only the selected recovered S writer, never a new producer.
#[derive(Clone)]
pub(crate) struct RecoveredSAuthority {
    owner: Arc<ImportedRetainedS>,
    current_owner_id: Uuid,
    binding: ReservationBinding,
    adoption_operation_id: Uuid,
    adoption_sequence: i64,
}
impl std::fmt::Debug for RecoveredSAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecoveredSAuthority")
            .field("obligation", &self.obligation_id())
            .field("adoption_sequence", &self.adoption_sequence)
            .finish_non_exhaustive()
    }
}
impl RecoveredSAuthority {
    pub(super) fn claim(&self) -> &RetainedSImportClaim {
        &self.owner.claim
    }
    pub(crate) fn journal_id(&self) -> Uuid {
        self.owner.claim.journal_id()
    }
    pub(crate) fn installation_id(&self) -> Uuid {
        self.owner.claim.installation_id()
    }
    pub(crate) fn obligation_id(&self) -> Uuid {
        self.owner.claim.obligation_id()
    }
    pub(crate) fn original_owner_id(&self) -> Uuid {
        self.owner.claim.original_owner_id()
    }
    pub(crate) fn previous_owner_id(&self) -> Uuid {
        self.owner.claim.old_owner_id()
    }
    pub(crate) fn current_owner_id(&self) -> Uuid {
        self.current_owner_id
    }
    pub(crate) fn binding(&self) -> ReservationBinding {
        self.binding
    }
    pub(crate) fn adoption_operation_id(&self) -> Uuid {
        self.adoption_operation_id
    }
    pub(crate) fn adoption_sequence(&self) -> i64 {
        self.adoption_sequence
    }
    pub(crate) fn state_head(&self) -> &v2::scratch::AssetEventRefV1 {
        self.owner.claim.current_head()
    }
    pub(crate) fn frozen_digest(&self) -> &str {
        self.owner.claim.inventory_sha256()
    }
}
impl NativeRetainedSImportProof {
    pub(crate) fn adopted_authority(
        &self,
        ack: &crate::resource_custody::RecoveryAdoptionAck,
    ) -> Option<RecoveredSAuthority> {
        if ack.obligation_id != self.obligation_id()
            || ack.previous_owner_id != self.previous_owner_id()
            || ack.current_owner_id != self.current_owner_id()
            || ack.operation_id.is_nil()
            || ack.sequence <= 0
        {
            return None;
        }
        Some(RecoveredSAuthority {
            owner: self.owner.clone(),
            current_owner_id: self.current_owner_id,
            binding: self.binding,
            adoption_operation_id: ack.operation_id,
            adoption_sequence: ack.sequence,
        })
    }
}

/// Native ownership moved only after the exact known journal adoption. This
/// has no public constructor and carries the actual newly held descriptors.
pub(super) struct RecoveredNativeParts {
    pub(super) plan: v2::scratch::bootstrap::ScratchPlanV3,
    pub(super) fold: v2::scratch::bootstrap::BootstrapFold,
    pub(super) parent: File,
    pub(super) child: File,
    pub(super) parent_observed: v2::scratch::DirectoryObservedV1,
    pub(super) child_observed: v2::scratch::DirectoryObservedV1,
    pub(super) adoption_id: v2::Id,
}
impl RecoveredSAuthority {
    pub(super) fn take_native(&self) -> Result<RecoveredNativeParts, RuntimeOpenError> {
        let claim = &self.owner.claim;
        let full = claim.full();
        let plan = claim.plan().clone();
        let parent_observed = full
            .state
            .bootstrap
            .parent
            .as_ref()
            .ok_or_else(unavailable)?
            .observed
            .clone();
        let child_observed = full
            .state
            .asset
            .directory
            .as_ref()
            .ok_or_else(unavailable)?
            .clone();
        if parent_observed.held_copy_ids.len() != 1
            || child_observed.held_copy_ids.len() != 1
            || parent_observed.held_copy_ids[0] == child_observed.held_copy_ids[0]
        {
            return Err(unavailable());
        }
        let adoption_id = v2::Id::new(self.adoption_operation_id)?;
        let mut directories = self.owner.directories.lock().map_err(|_| unavailable())?;
        let (parent, child) = directories.as_ref().ok_or_else(unavailable)?;
        if !native_matches(
            parent,
            &parent_observed.native_object,
            &plan.filesystem_domain,
        ) || !native_matches(
            child,
            &child_observed.native_object,
            &plan.filesystem_domain,
        ) {
            return Err(unavailable());
        }
        // Lock and validate the complete owner before either one-shot take.
        // A failure above leaves both descriptors and target fold retained.
        let fold = claim.take_target_for_runtime()?;
        let (parent, child) = directories
            .take()
            .expect("validated retained directory owners");
        Ok(RecoveredNativeParts {
            plan,
            fold,
            parent,
            child,
            parent_observed,
            child_observed,
            adoption_id,
        })
    }
}

/// The original queued command and receiver stay runtime-owned across every
/// borrowed wait. No retry chooses another operation UUID or imports again.
pub(super) struct RecoveryAdoption {
    operation_id: Uuid,
    obligation_id: Uuid,
    previous_owner_id: Uuid,
    proof: Option<NativeRetainedSImportProof>,
    exit: Option<PositivePreviousOwnerExit>,
    pending: Option<crate::resource_custody::PendingOperation>,
    result: Option<Result<crate::resource_custody::OperationResult, CustodyError>>,
    acknowledged: bool,
    handle: Option<(crate::resource_custody::ObligationHandle, i64)>,
    preparation: Option<super::operations::OwnedPreparation>,
}
impl RecoveryAdoption {
    fn submit(
        &mut self,
        journal: &CustodyJournal,
        deadline: Instant,
    ) -> Result<(), RuntimeOpenError> {
        if self.pending.is_some() || self.acknowledged {
            return Err(unavailable());
        }
        if let Some(proof) = self.proof.take() {
            let exit = self.exit.take().ok_or_else(unavailable)?;
            match journal.recover_retained_s(self.operation_id, proof, exit, deadline) {
                Ok(pending) => self.pending = Some(pending),
                Err((error, proof, exit)) => {
                    self.proof = Some(proof);
                    self.exit = Some(exit);
                    self.result = Some(Err(error.clone()));
                    return Err(error.into());
                }
            }
        } else {
            self.pending = Some(journal.retry(self.operation_id, deadline)?);
        }
        self.result = None;
        Ok(())
    }
    async fn resolve(&mut self, runtime: &NodeResourceRuntime) -> Result<(), RuntimeOpenError> {
        if self.acknowledged {
            return Ok(());
        }
        if let Some(pending) = self.pending.as_mut() {
            let result = pending.wait_retained().await;
            self.pending = None;
            self.result = Some(result);
        }
        let result = self.result.as_ref().ok_or_else(unavailable)?;
        let ack = match result {
            Ok(crate::resource_custody::OperationResult::RecoveredSAdoption(ack)) => ack,
            Err(error) => return Err(error.clone().into()),
            _ => return Err(unavailable()),
        };
        if ack.operation_id != self.operation_id
            || ack.obligation_id != self.obligation_id
            || ack.previous_owner_id != self.previous_owner_id
            || ack.current_owner_id != runtime.owner.owner_id
            || ack.sequence <= 0
        {
            return Err(unavailable());
        }
        // This only drops the completed queue proof. The actual native owner
        // remains in recovered_s and the complete restored ledger charge stays.
        if self.handle.is_none() {
            self.handle = Some(runtime.journal.take_recovered_s_handle(self.operation_id)?);
        }
        runtime.journal.acknowledge(self.operation_id)?;
        self.acknowledged = true;
        Ok(())
    }
}

fn unavailable() -> RuntimeOpenError {
    RuntimeOpenError::Accounting(AdmissionRefusal::Unavailable)
}

fn component(name: &str) -> Result<CString, RuntimeOpenError> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.len() > 255 {
        return Err(unavailable());
    }
    CString::new(name).map_err(|_| unavailable())
}

fn open_dir(parent: &File, name: &str) -> Result<File, RuntimeOpenError> {
    let name = component(name)?;
    // SAFETY: the parent File stays held, name is one NUL-terminated component,
    // and success returns a newly owned descriptor.
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
        return Err(unavailable());
    }
    // SAFETY: successful openat yields one owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn mount_id(file: &File) -> Option<u64> {
    let mut stat = std::mem::MaybeUninit::<libc::statx>::zeroed();
    // SAFETY: file is held, AT_EMPTY_PATH selects its fd, stat is writable.
    let rc = unsafe {
        libc::statx(
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            libc::STATX_MNT_ID,
            stat.as_mut_ptr(),
        )
    };
    if rc != 0 {
        return None;
    }
    // SAFETY: successful statx initialized this struct.
    let stat = unsafe { stat.assume_init() };
    (stat.stx_mask & libc::STATX_MNT_ID != 0).then_some(stat.stx_mnt_id)
}

fn native_matches(file: &File, expected: &v2::NativeObject, domain: &v2::FilesystemDomain) -> bool {
    let Ok(meta) = file.metadata() else {
        return false;
    };
    let kind_matches = match &expected.file_type {
        v2::FileType::Directory => meta.is_dir(),
        v2::FileType::Regular => meta.is_file(),
    };
    kind_matches
        && expected.filesystem_domain == *domain
        && matches!(&expected.device, v2::NativeValue::Known { value } if *value == meta.dev())
        && matches!(&expected.inode, v2::NativeValue::Known { value } if *value == meta.ino())
        && matches!(&expected.mount_id, v2::NativeValue::Known { value } if Some(*value) == mount_id(file))
}

fn filename(output: OutputKey) -> String {
    match output {
        OutputKey::Init => "init.mp4".into(),
        OutputKey::Segment { index } => crate::fmp4::segment_name(u64::from(index)),
        OutputKey::IdentityMetadata => "identity.json".into(),
        OutputKey::EncodedProcessMetadata => "encoded-process".into(),
    }
}

/// Match recorded native identity and allocation without acquiring another
/// descriptor. Future counted readers must do their own native open/recheck.
fn regular_entry_matches(
    child: &File,
    name: &str,
    expected: &v2::scratch::FileObservedV1,
    domain: &v2::FilesystemDomain,
) -> bool {
    let Ok(name) = component(name) else {
        return false;
    };
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: child stays held, name is one NUL-terminated component, and the
    // output buffer is writable. AT_SYMLINK_NOFOLLOW rejects a substituted link.
    if unsafe {
        libc::fstatat(
            child.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return false;
    }
    // SAFETY: successful fstatat initialized stat.
    let stat = unsafe { stat.assume_init() };
    if stat.st_blocks < 0 {
        return false;
    }
    let Some(allocated) = (stat.st_blocks as u64).checked_mul(512) else {
        return false;
    };
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG
        || stat.st_size < 0
        || stat.st_size as u64 != expected.logical_len
        || expected.native_object.file_type != v2::FileType::Regular
        || expected.native_object.filesystem_domain != *domain
        || expected.native_object.device
            != (v2::NativeValue::Known {
                value: stat.st_dev as u64,
            })
        || expected.native_object.inode
            != (v2::NativeValue::Known {
                value: stat.st_ino as u64,
            })
        || expected.reported_allocated_bytes != (v2::NativeValue::Known { value: allocated })
        || expected.charged_bytes < allocated
    {
        return false;
    }
    let mut extra = std::mem::MaybeUninit::<libc::statx>::zeroed();
    // SAFETY: same held child/name; statx supplies recorded mount identity.
    if unsafe {
        libc::statx(
            child.as_raw_fd(),
            name.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_MNT_ID,
            extra.as_mut_ptr(),
        )
    } != 0
    {
        return false;
    }
    // SAFETY: successful statx initialized extra.
    let extra = unsafe { extra.assume_init() };
    extra.stx_mask & libc::STATX_MNT_ID != 0
        && extra.stx_ino == stat.st_ino as u64
        && extra.stx_size == expected.logical_len
        && expected.native_object.mount_id
            == (v2::NativeValue::Known {
                value: extra.stx_mnt_id,
            })
}

/// Read the exact held directory without opening an uncharged directory fd.
/// Linux getdents64 supplies only names; each named file is subsequently
/// identified descriptor-relative without acquiring an extra file handle.
fn exact_entries(child: &File, expected: &BTreeSet<String>) -> Result<(), RuntimeOpenError> {
    let mut found = BTreeSet::new();
    let mut buffer = [0u8; 8192];
    loop {
        // SAFETY: child is a held directory fd and buffer is writable.
        let n = unsafe {
            libc::syscall(
                libc::SYS_getdents64,
                child.as_raw_fd(),
                buffer.as_mut_ptr(),
                buffer.len(),
            )
        };
        if n < 0 {
            return Err(unavailable());
        }
        if n == 0 {
            break;
        }
        let n = usize::try_from(n).map_err(|_| unavailable())?;
        let mut offset = 0usize;
        while offset < n {
            if n - offset < 20 {
                return Err(unavailable());
            }
            let reclen = u16::from_ne_bytes([buffer[offset + 16], buffer[offset + 17]]) as usize;
            if reclen < 20 || reclen > n - offset {
                return Err(unavailable());
            }
            let name = CStr::from_bytes_until_nul(&buffer[offset + 19..offset + reclen])
                .map_err(|_| unavailable())?;
            let name = name.to_str().map_err(|_| unavailable())?;
            if name != "."
                && name != ".."
                && (!expected.contains(name)
                    || found.len() >= expected.len()
                    || !found.insert(name.to_owned()))
            {
                return Err(unavailable());
            }
            offset += reclen;
        }
    }
    if &found != expected {
        return Err(unavailable());
    }
    Ok(())
}

impl NodeResourceRuntime {
    /// Select once from the same opened journal, retain native owners and the
    /// original adoption operation before enqueue. Failed imports stay protected
    /// and fully charged; they cannot fall back to pathname/legacy ownership.
    pub(crate) fn begin_recovered_s_imports(
        &self,
        deadline: Instant,
    ) -> Result<(), RuntimeOpenError> {
        for (claim, exit) in self.journal.take_recovery_candidates() {
            let id = claim.obligation_id();
            let previous_owner_id = exit.owner_id();
            let proof = match self.install_dormant_retained_s(claim, &exit) {
                Ok(proof) => proof,
                Err(_) => continue,
            };
            let operation = RecoveryAdoption {
                operation_id: Uuid::new_v4(),
                obligation_id: id,
                previous_owner_id,
                proof: Some(proof),
                exit: Some(exit),
                pending: None,
                result: None,
                acknowledged: false,
                handle: None,
                preparation: None,
            };
            let operation = Arc::new(tokio::sync::Mutex::new(operation));
            self.recovered_adoptions
                .lock()
                .map_err(|_| unavailable())?
                .insert(id, operation.clone());
            // No await: this is startup's single import owner and the record was
            // installed before the first possible queue entry.
            let mut operation = operation.try_lock().map_err(|_| unavailable())?;
            let _ = operation.submit(&self.journal, deadline);
        }
        Ok(())
    }

    pub(crate) async fn reconcile_recovered_s_imports(
        &self,
        deadline: Instant,
    ) -> Result<(), RuntimeOpenError> {
        let operations: Vec<_> = self
            .recovered_adoptions
            .lock()
            .map_err(|_| unavailable())?
            .values()
            .cloned()
            .collect();
        for operation in operations {
            let mut operation = operation.lock().await;
            if !operation.acknowledged && operation.pending.is_none() {
                operation.submit(&self.journal, deadline)?;
            }
            operation.resolve(self).await?;
            if operation.preparation.is_none() {
                let (handle, sequence) = operation.handle.as_ref().ok_or_else(unavailable)?;
                let preparation = self
                    .install_recovered_finite_preparation(handle.clone(), *sequence)
                    .map_err(|_| unavailable())?;
                operation.preparation = Some(preparation);
            }
        }
        Ok(())
    }

    /// Select only the exact imported/adopted, complete fixed-software cache.
    /// The recipe key alone is insufficient: compare the original journal's
    /// media object association before returning a live response selection.
    pub(super) async fn recovered_finite_selection(
        &self,
        rendition_id: &str,
        media_file_id: &str,
        object_version: &str,
        source_members: usize,
        segment_count: u32,
    ) -> Result<Option<(super::operations::OwnedPreparation, Uuid, PathBuf, bool)>, RuntimeOpenError>
    {
        let ids: Vec<_> = self
            .recovered
            .retained_assets
            .iter()
            .filter_map(|asset| {
                let name = match &asset.custody {
                    v2::RetainedAssetsDocument::V1(custody) => &custody.rendition_id,
                    v2::RetainedAssetsDocument::V2(custody) => &custody.rendition_id,
                };
                (name == rendition_id).then_some(asset.obligation_id)
            })
            .collect();
        if ids.is_empty() {
            return Ok(None);
        }
        let [id] = ids.as_slice() else {
            return Err(unavailable());
        };
        let (member, path, marker) = {
            let owners = self.recovered_s.lock().map_err(|_| unavailable())?;
            let owner = owners.get(id).ok_or_else(unavailable)?;
            let claim = &owner.claim;
            let [source] = claim.media_sources() else {
                return Err(unavailable());
            };
            if source.media_file_id != media_file_id
                || source.object_version != object_version
                || source.recipe_source_binding != rendition_id
                || claim.media_source_member_count() != source_members
                || segment_count == 0
            {
                return Err(unavailable());
            }
            let mut segments = BTreeSet::new();
            let mut init = false;
            let mut identity = false;
            let mut marker = false;
            for file in &claim.full().files {
                match file.published.ok_or_else(unavailable)? {
                    OutputKey::Init => init = file.logical_len > 0,
                    OutputKey::IdentityMetadata => identity = file.logical_len > 0,
                    OutputKey::EncodedProcessMetadata => marker = true,
                    OutputKey::Segment { index } => {
                        if index >= segment_count
                            || file.logical_len == 0
                            || !segments.insert(index)
                        {
                            return Err(unavailable());
                        }
                    }
                }
            }
            if !init || !identity || !marker || segments.len() != segment_count as usize {
                return Err(unavailable());
            }
            (
                claim.member_id().uuid(),
                PathBuf::from(&claim.plan().bootstrap().protected_child_path),
                marker,
            )
        };
        let operation = self
            .recovered_adoptions
            .lock()
            .map_err(|_| unavailable())?
            .get(id)
            .cloned()
            .ok_or_else(unavailable)?;
        let operation = operation.lock().await;
        if !operation.acknowledged {
            return Err(unavailable());
        }
        let preparation = operation.preparation.clone().ok_or_else(unavailable)?;
        Ok(Some((preparation, member, path, marker)))
    }

    /// Synchronous registration retains actual reopened owners before any
    /// adoption wait. Serialize repeated/concurrent imports of this runtime;
    /// this is a recovery-owner mutex, never the scheduler/ledger lock.
    pub(crate) fn install_dormant_retained_s(
        &self,
        claim: RetainedSImportClaim,
        exit: &PositivePreviousOwnerExit,
    ) -> Result<NativeRetainedSImportProof, RuntimeOpenError> {
        let id = claim.obligation_id();
        let mut owners = self.recovered_s.lock().map_err(|_| unavailable())?;
        if owners.contains_key(&id) {
            return Err(unavailable());
        }
        let binding = *self.recovered_bindings.get(&id).ok_or_else(unavailable)?;
        let imported = Arc::new(self.import_dormant_retained_s(claim, exit)?);
        owners.insert(id, imported.clone());
        Ok(NativeRetainedSImportProof {
            owner: imported,
            current_owner_id: self.owner.owner_id,
            binding,
        })
    }

    /// A complete native import still leaves the original S entry UnknownHeld.
    /// The caller must retain the returned Files before any journal adoption or
    /// serving. A failed import never credits capacity or unprotects paths.
    fn import_dormant_retained_s(
        &self,
        claim: RetainedSImportClaim,
        exit: &PositivePreviousOwnerExit,
    ) -> Result<ImportedRetainedS, RuntimeOpenError> {
        let id = claim.obligation_id();
        let binding = *self.recovered_bindings.get(&id).ok_or_else(unavailable)?;
        let state = self.ledger.inner.lock().map_err(|_| unavailable())?;
        let entry = state
            .entries
            .get(&binding.reservation)
            .ok_or_else(unavailable)?;
        if entry.binding != binding
            || entry.durable != Some(id)
            || entry.resources != claim.resources()
            || entry.resources.handles < 2
            || entry.state != ReservationState::UnknownHeld
            || claim.old_owner_id() != exit.owner_id()
            || claim.journal_id() != self.recovered.journal_id
            || claim.installation_id() != self.recovered.installation_id
            || !exit.matches(&self.recovered, &self.owner, claim.old_owner_id())
        {
            return Err(unavailable());
        }
        drop(state);
        let plan = claim.plan();
        let full = claim.full();
        let root = plan.bootstrap();
        if self.rendition_root.as_deref() != Some(Path::new(&root.authority.resolved_absolute_root))
            || root.authority.installation_id.uuid() != self.recovered.installation_id
            || plan.filesystem_domain.daemon_owner_id.uuid() != claim.original_owner_id()
            || plan.filesystem_domain.os_boot != self.owner.os_boot
            || !matches!(&plan.filesystem_domain.mount_namespace, v2::NativeValue::Known { value }
                if std::fs::metadata("/proc/self/ns/mnt").is_ok_and(|m| m.ino() == *value))
            || !full.readers.is_empty()
            || full.files.len() > 1024
            || full.files.len()
                > usize::try_from(plan.limits.resident_file_objects).map_err(|_| unavailable())?
        {
            return Err(unavailable());
        }
        let mut dir = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open("/")
            .map_err(|_| unavailable())?;
        for name in &root.components {
            let next = open_dir(&dir, name)?;
            // Explicit once-only close of the original intermediate handle.
            // Unknown close never becomes an installed/imported S owner.
            let fd = dir.into_raw_fd();
            if unsafe { libc::close(fd) } != 0 {
                return Err(unavailable());
            }
            dir = next;
        }
        let parent_observed = &full
            .state
            .bootstrap
            .parent
            .as_ref()
            .ok_or_else(unavailable)?
            .observed;
        if parent_observed.held_copy_ids.len() != 1
            || !native_matches(
                &dir,
                &parent_observed.native_object,
                &plan.filesystem_domain,
            )
        {
            return Err(unavailable());
        }
        let child = open_dir(&dir, &root.child_component)?;
        let child_observed = full
            .state
            .asset
            .directory
            .as_ref()
            .ok_or_else(unavailable)?;
        if child_observed.held_copy_ids.len() != 1
            || !native_matches(
                &child,
                &child_observed.native_object,
                &plan.filesystem_domain,
            )
            || mount_id(&dir) != mount_id(&child)
            || dir.metadata().map_err(|_| unavailable())?.dev()
                != child.metadata().map_err(|_| unavailable())?.dev()
        {
            return Err(unavailable());
        }
        let mut names = BTreeSet::new();
        for observed in &full.files {
            if observed.temporary || !observed.held_copy_ids.is_empty() {
                return Err(unavailable());
            }
            let output = observed.published.ok_or_else(unavailable)?;
            if !plan.outputs.contains(&output) {
                return Err(unavailable());
            }
            let name = filename(output);
            if !names.insert(name.clone()) {
                return Err(unavailable());
            }
            if !regular_entry_matches(&child, &name, observed, &plan.filesystem_domain) {
                return Err(unavailable());
            }
        }
        exact_entries(&child, &names)?;
        Ok(ImportedRetainedS {
            claim,
            directories: Mutex::new(Some((dir, child))),
        })
    }
}
