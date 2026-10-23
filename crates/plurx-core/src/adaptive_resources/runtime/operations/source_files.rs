//! Source acquisition in the original retained bundle, before opening a path.
//! A returned lease lends the SAME descriptor; its Arc and close authority stay
//! private to this counted custodian. It is neither cloneable nor deserializable.
use super::*;
use crate::source_identity::SourceIdentity;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::os::fd::{AsRawFd, IntoRawFd, RawFd};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::Mutex;
use tokio::task::JoinHandle;

const VERIFIED_EXECUTABLE_CACHE_CAPACITY: usize = 8;

#[derive(Clone, Copy)]
struct VerifiedExecutable {
    identity: SourceIdentity,
    digest: [u8; 32],
}

struct VerifiedExecutableCache {
    entries: [Option<VerifiedExecutable>; VERIFIED_EXECUTABLE_CACHE_CAPACITY],
    next: usize,
}

impl VerifiedExecutableCache {
    const fn new() -> Self {
        Self {
            entries: [None; VERIFIED_EXECUTABLE_CACHE_CAPACITY],
            next: 0,
        }
    }

    fn contains(&self, identity: SourceIdentity, digest: &[u8; 32]) -> bool {
        self.entries
            .iter()
            .flatten()
            .any(|entry| entry.identity == identity && &entry.digest == digest)
    }

    fn insert(&mut self, identity: SourceIdentity, digest: [u8; 32]) {
        self.entries[self.next] = Some(VerifiedExecutable { identity, digest });
        self.next = (self.next + 1) % VERIFIED_EXECUTABLE_CACHE_CAPACITY;
    }
}

static VERIFIED_EXECUTABLES: Mutex<VerifiedExecutableCache> =
    Mutex::new(VerifiedExecutableCache::new());

fn cached_executable_matches(
    file: &File,
    expected_identity: SourceIdentity,
    expected_digest: &[u8; 32],
    cache: &Mutex<VerifiedExecutableCache>,
) -> bool {
    let Ok(before) = file.metadata() else {
        return false;
    };
    if SourceIdentity::from_metadata(&before) != expected_identity {
        return false;
    }
    let cached = cache
        .lock()
        .map(|cache| cache.contains(expected_identity, expected_digest))
        .unwrap_or(false);
    if !cached {
        return false;
    }
    file.metadata().is_ok_and(|after| {
        SourceIdentity::from_metadata(&after) == expected_identity
            && SourceIdentity::from_metadata(&after) == SourceIdentity::from_metadata(&before)
    })
}

pub(in crate::adaptive_resources::runtime) fn executable_recipe_binding(
    digest: &[u8; 32],
) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push(HEX[usize::from(byte >> 4)] as char);
        encoded.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    format!("software-avc-executable-sha256:{encoded}")
}

const EXECUTABLE_RECIPE_PREFIX: &str = "software-avc-executable-sha256:";

fn digest_binding_matches(binding: &str, expected: Option<[u8; 32]>) -> bool {
    match expected {
        Some(digest) => binding == executable_recipe_binding(&digest),
        None => !binding.starts_with(EXECUTABLE_RECIPE_PREFIX),
    }
}

fn is_native_elf(file: &File, len: u64) -> bool {
    if len < 20 {
        return false;
    }
    let mut header = [0u8; 20];
    let mut offset = 0usize;
    while offset < header.len() {
        let Ok(read) = file.read_at(&mut header[offset..], offset as u64) else {
            return false;
        };
        if read == 0 {
            return false;
        }
        offset += read;
    }
    if &header[..4] != b"\x7fELF"
        || header[6] != 1
        || header[5] != if cfg!(target_endian = "little") { 1 } else { 2 }
        || header[4]
            != if cfg!(target_pointer_width = "64") {
                2
            } else {
                1
            }
    {
        return false;
    }
    let u16_at = |bytes: [u8; 2]| {
        if cfg!(target_endian = "little") {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        }
    };
    let file_type = u16_at([header[16], header[17]]);
    let machine = u16_at([header[18], header[19]]);
    let native_machine = if cfg!(target_arch = "x86_64") {
        62
    } else if cfg!(target_arch = "aarch64") {
        183
    } else if cfg!(target_arch = "x86") {
        3
    } else if cfg!(target_arch = "arm") {
        40
    } else if cfg!(target_arch = "riscv64") {
        243
    } else if cfg!(target_arch = "s390x") {
        22
    } else if cfg!(target_arch = "powerpc64") {
        21
    } else {
        return false;
    };
    matches!(file_type, 2 | 3) && machine == native_machine
}

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
    pub expected_sha256: Option<[u8; 32]>,
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
    pub verified_sha256: Option<[u8; 32]>,
    pub verified_elf: bool,
    pub verified_sha256_cached: bool,
}
struct SourceResult {
    file: Option<Arc<File>>,
    observed: Option<v2::SourceObserved>,
    verified_sha256: Option<[u8; 32]>,
    verified_elf: bool,
    verified_sha256_cached: bool,
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
                    verified_sha256: None,
                    verified_elf: false,
                    verified_sha256_cached: false,
                };
            }
            open_source(spec, plan)
        }));
        Ok(enter)
    }
}

fn open_source(spec: SourceOpenSpec, plan: v2::SourcePlan) -> SourceResult {
    open_source_with_cache(spec, plan, &VERIFIED_EXECUTABLES)
}

fn open_source_with_cache(
    spec: SourceOpenSpec,
    plan: v2::SourcePlan,
    cache: &Mutex<VerifiedExecutableCache>,
) -> SourceResult {
    let mut result = SourceResult {
        file: None,
        observed: None,
        verified_sha256: None,
        verified_elf: false,
        verified_sha256_cached: false,
    };
    // No source path is opened unless the exact current domain is observed in
    // this original, registered native job. Domain equality is not restart proof.
    if !std::fs::metadata("/proc/self/ns/mnt").is_ok_and(|m| {
        matches!(plan.filesystem_domain.mount_namespace,
            v2::NativeValue::Known { value } if value == m.ino())
    }) {
        return result;
    }
    if !digest_binding_matches(
        &plan.source_association.recipe_source_binding,
        spec.expected_sha256,
    ) {
        return result;
    }
    if spec.expected_sha256.is_some() && !(1..=512 * 1024 * 1024).contains(&spec.scanner_size) {
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
    if let Some(expected) = spec.expected_sha256 {
        let before = SourceIdentity::from_metadata(&metadata);
        if !is_native_elf(file, metadata.len()) {
            return result;
        }
        if cached_executable_matches(file, before, &expected, cache) {
            result.verified_sha256 = Some(expected);
            result.verified_elf = true;
            result.verified_sha256_cached = true;
        } else {
            let mut hasher = Sha256::new();
            let mut buffer = [0u8; 64 * 1024];
            let mut offset = 0u64;
            while offset < metadata.len() {
                let wanted = ((metadata.len() - offset).min(buffer.len() as u64)) as usize;
                let Ok(read) = file.read_at(&mut buffer[..wanted], offset) else {
                    return result;
                };
                if read == 0 {
                    return result;
                }
                hasher.update(&buffer[..read]);
                offset += read as u64;
            }
            let Ok(after_metadata) = file.metadata() else {
                return result;
            };
            let after = SourceIdentity::from_metadata(&after_metadata);
            let digest: [u8; 32] = hasher.finalize().into();
            if before != after || digest != expected {
                return result;
            }
            if let Ok(mut cache) = cache.lock() {
                cache.insert(before, digest);
            }
            result.verified_sha256 = Some(digest);
            result.verified_elf = true;
        }
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
            || (spec.expected_sha256.is_some()
                && !(1..=512 * 1024 * 1024).contains(&spec.scanner_size))
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
            || !digest_binding_matches(
                &source.source_association.recipe_source_binding,
                spec.expected_sha256,
            )
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
    /// Recheck the SAME retained native file against its durably observed S1
    /// identity. A pathname lookup cannot substitute another object here.
    pub(super) fn unchanged_for_publication(&self) -> Result<(), OperationError> {
        let v2::MemberPlan::SourceV1 { source: plan, .. } = &self.plan else {
            return Err(OperationError::State);
        };
        if !self.observed_committed
            || !self.root_observed_committed
            || self.selected_root_member.is_none()
            || self.pending.is_some()
            || self.spec.is_some()
            || self.failed_join
            || self.lease_issued
            || self.pending_root.is_some()
            || !matches!(&self.parent_close, ParentCloseState::NotStarted)
        {
            return Err(OperationError::State);
        }
        let result = self.result.as_ref().ok_or(OperationError::State)?;
        let file = result.file.as_ref().ok_or(OperationError::State)?;
        let observed = result.observed.as_ref().ok_or(OperationError::State)?;
        let metadata = file.metadata()?;
        let native = &observed.native_object;
        if !metadata.is_file()
            || observed.holder_id != plan.holder_id
            || observed.member_id != plan.member_id
            || observed.source_association != plan.source_association
            || observed.access != v2::SourceAccess::ReadOnlyRegular
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
                != observed.source_association.object_version
        {
            return Err(OperationError::State);
        }
        Ok(())
    }

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
            verified_sha256: result.verified_sha256,
            verified_elf: result.verified_elf,
            verified_sha256_cached: result.verified_sha256_cached,
        })
    }

    /// Closes only this retained parent descriptor. A failed/ambiguous close is
    /// terminal because retrying its numeric fd could close a recycled handle.
    pub(super) async fn close_parent(&mut self) -> Result<(), OperationError> {
        self.close_parent_inner(true).await
    }

    /// A sealed no-root graph proves that no child inherited this source.
    /// An original open may still have returned a File before metadata rejected
    /// it, so close that SAME File even without a durable SourceObserved.
    pub(super) async fn close_parent_before_root(
        &mut self,
        closure: &crate::adaptive_resources::physical_graph::finite_vod::FiniteVodNeverRootClosure,
    ) -> Result<(), OperationError> {
        self.no_root_copy_plan(closure)?;
        self.join_registered().await?;
        if self.parent_closed() {
            return Ok(());
        }
        if self.spec.is_some() {
            return Ok(());
        }
        let result = self.result.as_ref().ok_or(OperationError::State)?;
        if result.file.is_none() {
            return if result.observed.is_none() {
                Ok(())
            } else {
                Err(OperationError::State)
            };
        }
        self.close_parent_inner(false).await
    }

    async fn close_parent_inner(&mut self, require_observed: bool) -> Result<(), OperationError> {
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
                || (require_observed && !self.observed_committed)
                || self.pending.is_some()
                || self.spec.is_some()
            {
                return Err(OperationError::State);
            }
            let runtime = tokio::runtime::Handle::try_current().map_err(std::io::Error::other)?;
            let result = self.result.as_mut().ok_or(OperationError::State)?;
            if require_observed && result.observed.is_none() {
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

    fn no_root_copy_plan(
        &self,
        closure: &crate::adaptive_resources::physical_graph::finite_vod::FiniteVodNeverRootClosure,
    ) -> Result<(v2::SourceCopy, v2::SourceCopy, v2::Id), OperationError> {
        let v2::MemberPlan::SourceV1 { source: plan, .. } = &self.plan else {
            return Err(OperationError::State);
        };
        let v2::SourceRecovery::LocalSourceHandlesV1 {
            graph_id, owner_id, ..
        } = &plan.recovery;
        if graph_id.uuid() != closure.generation()
            || *owner_id != plan.filesystem_domain.daemon_owner_id
            || self.lease_issued
            || self.root_observed_committed
            || self.selected_root_member.is_some()
            || self.failed_join
            || plan.planned_copies.len() != 2
        {
            return Err(OperationError::State);
        }
        let mut originals = plan.planned_copies.iter().filter(|copy| {
            matches!(copy.owner,
                v2::CopyOwner::DaemonSourceCell { owner_id: held_owner, graph_id: held_graph, member_id }
                    if held_owner == *owner_id && held_graph == *graph_id && member_id == plan.member_id)
        });
        let original = originals.next().ok_or(OperationError::State)?;
        if originals.next().is_some() {
            return Err(OperationError::State);
        }
        let mut children = plan.planned_copies.iter().filter(|copy| {
            matches!(copy.owner,
                v2::CopyOwner::OwnedRoot { graph_id: held_graph, root_member_id }
                    if held_graph == *graph_id && root_member_id.uuid() == closure.root_member())
                && copy.parent_copy_id == Some(original.copy_id)
                && copy.acquisition_operation_id.uuid() == closure.acquisition_id()
        });
        let child = children.next().ok_or(OperationError::State)?;
        if children.next().is_some()
            || original.copy_id == child.copy_id
            || self
                .pending_root
                .as_ref()
                .is_some_and(|(root, staged, _)| *root != closure.root_member() || staged != child)
            || self
                .result
                .as_ref()
                .and_then(|r| r.observed.as_ref())
                .is_some_and(|observed| {
                    observed
                        .copies
                        .iter()
                        .any(|copy| copy.copy_id == child.copy_id)
                })
        {
            return Err(OperationError::State);
        }
        Ok((original.clone(), child.clone(), plan.holder_id))
    }

    /// Exact original parent close or proven no acquisition, plus absence of
    /// the planned root copy under the separate sealed original graph witness.
    pub(in crate::adaptive_resources::runtime) fn finite_never_root_closed_fact(
        &self,
        closure: &crate::adaptive_resources::physical_graph::finite_vod::FiniteVodNeverRootClosure,
    ) -> Result<v2::ClosedFact, OperationError> {
        let (original, child, holder_id) = self.no_root_copy_plan(closure)?;
        if self.pending.is_some() {
            return Err(OperationError::State);
        }
        let original_fact = if self.parent_closed() {
            v2::CopyClosed::ActualOwnedClose {
                copy: original,
                operation_id: v2::Id::new(self.parent_close_operation_id)?,
            }
        } else if matches!(&self.parent_close, ParentCloseState::NotStarted)
            && (self.spec.is_some() && self.result.is_none()
                || self.spec.is_none()
                    && self
                        .result
                        .as_ref()
                        .is_some_and(|result| result.file.is_none() && result.observed.is_none()))
        {
            v2::CopyClosed::ProvenNeverAcquired {
                copy: original,
                operation_id: v2::Id::new(closure.close_operation_id())?,
            }
        } else {
            return Err(OperationError::State);
        };
        Ok(v2::ClosedFact::SourceV1 {
            holder_id,
            copies: vec![
                original_fact,
                v2::CopyClosed::ProvenNeverAcquired {
                    copy: child,
                    operation_id: v2::Id::new(closure.close_operation_id())?,
                },
            ],
        })
    }

    pub(super) fn parent_closed(&self) -> bool {
        matches!(&self.parent_close, ParentCloseState::Closed)
    }

    pub(super) fn parent_close_operation_id(&self) -> Uuid {
        self.parent_close_operation_id
    }

    /// Derive the source-member close audit only after this cell's original
    /// descriptor close and the graph's exact finite-process closure succeed.
    /// This records those facts; it does not grant a transfer or refund.
    pub(in crate::adaptive_resources::runtime) fn finite_closed_fact(
        &self,
        closure: &crate::adaptive_resources::physical_graph::finite_vod::FiniteVodProcessClosure,
    ) -> Result<v2::ClosedFact, OperationError> {
        self.finite_closed_fact_with(FiniteProcessClosedProof::Joined(closure))
    }

    /// A root-only graph can close an inherited copy even when the root-copy
    /// enrichment was not durably observed. Its original native witness is
    /// distinct from the joined-writer witness above.
    pub(in crate::adaptive_resources::runtime) fn finite_root_only_closed_fact(
        &self,
        closure: &crate::adaptive_resources::physical_graph::finite_vod::FiniteVodRootOnlyClosure,
    ) -> Result<v2::ClosedFact, OperationError> {
        self.finite_closed_fact_with(FiniteProcessClosedProof::RootOnly(closure))
    }

    fn finite_closed_fact_with(
        &self,
        proof: FiniteProcessClosedProof<'_>,
    ) -> Result<v2::ClosedFact, OperationError> {
        let (generation, root_member, acquisition_id, close_operation_id, joined_writer) =
            match proof {
                FiniteProcessClosedProof::Joined(closure) => (
                    closure.generation(),
                    closure.root_member(),
                    closure.acquisition_id(),
                    closure.close_operation_id(),
                    true,
                ),
                FiniteProcessClosedProof::RootOnly(closure) => (
                    closure.generation(),
                    closure.root_member(),
                    closure.acquisition_id(),
                    closure.close_operation_id(),
                    false,
                ),
            };
        if !self.parent_closed()
            || self.pending.is_some()
            || self.spec.is_some()
            || self.failed_join
            || self.lease_issued
            || !self.observed_committed
            || (joined_writer && (!self.root_observed_committed || self.pending_root.is_some()))
            || (self.root_observed_committed && self.selected_root_member != Some(root_member))
            || (!self.root_observed_committed
                && self
                    .pending_root
                    .as_ref()
                    .is_none_or(|(selected, _, _)| *selected != root_member))
        {
            return Err(OperationError::State);
        }
        let result = self.result.as_ref().ok_or(OperationError::State)?;
        if result.file.is_some() {
            return Err(OperationError::State);
        }
        let observed = result.observed.as_ref().ok_or(OperationError::State)?;
        let v2::MemberPlan::SourceV1 { source: plan, .. } = &self.plan else {
            return Err(OperationError::State);
        };
        let planned_graph = match &plan.recovery {
            v2::SourceRecovery::LocalSourceHandlesV1 { graph_id, .. } => *graph_id,
        };
        if generation != planned_graph.uuid()
            || observed.version != 1
            || observed.holder_id != plan.holder_id
            || observed.member_id != plan.member_id
            || observed.source_association != plan.source_association
            || observed.access != v2::SourceAccess::ReadOnlyRegular
            || plan.planned_copies.len() != 2
            || (joined_writer && observed.copies != plan.planned_copies)
        {
            return Err(OperationError::State);
        }

        let mut source_copies = plan.planned_copies.iter().filter(|copy| {
            matches!(copy.owner,
                v2::CopyOwner::DaemonSourceCell { owner_id, graph_id, member_id }
                    if owner_id == plan.filesystem_domain.daemon_owner_id
                        && graph_id == planned_graph
                        && member_id == plan.member_id)
        });
        let source_copy = source_copies.next().ok_or(OperationError::State)?;
        if source_copies.next().is_some()
            || source_copy.parent_copy_id.is_some()
            || source_copy.acquisition_operation_id != self.acquisition_id
        {
            return Err(OperationError::State);
        }

        let mut root_copies = plan.planned_copies.iter().filter(|copy| {
            matches!(copy.owner,
                v2::CopyOwner::OwnedRoot { graph_id, root_member_id }
                    if graph_id == planned_graph
                    && root_member_id.uuid() == root_member)
        });
        let root_copy = root_copies.next().ok_or(OperationError::State)?;
        if root_copies.next().is_some()
            || root_copy.parent_copy_id != Some(source_copy.copy_id)
            || root_copy.acquisition_operation_id.uuid() != acquisition_id
        {
            return Err(OperationError::State);
        }
        if !joined_writer
            && (!observed.copies.contains(source_copy)
                || (self.root_observed_committed && !observed.copies.contains(root_copy))
                || (!self.root_observed_committed
                    && self.pending_root.as_ref().is_none_or(
                        |(_, staged_copy, staged_observed)| {
                            staged_copy != root_copy
                                || !staged_observed.copies.contains(root_copy)
                                || !staged_observed.copies.contains(source_copy)
                        },
                    ))
                || observed
                    .copies
                    .iter()
                    .any(|copy| !plan.planned_copies.contains(copy)))
        {
            return Err(OperationError::State);
        }

        let source_close = v2::Id::new(self.parent_close_operation_id)?;
        let root_close = v2::Id::new(close_operation_id)?;
        if source_close == root_close {
            return Err(OperationError::State);
        }
        let copies = plan
            .planned_copies
            .iter()
            .map(|copy| {
                let operation_id = if copy == source_copy {
                    source_close
                } else if copy == root_copy {
                    root_close
                } else {
                    return Err(OperationError::State);
                };
                Ok(v2::CopyClosed::ActualOwnedClose {
                    copy: copy.clone(),
                    operation_id,
                })
            })
            .collect::<Result<Vec<_>, OperationError>>()?;
        Ok(v2::ClosedFact::SourceV1 {
            holder_id: plan.holder_id,
            copies,
        })
    }
}

enum FiniteProcessClosedProof<'a> {
    Joined(&'a crate::adaptive_resources::physical_graph::finite_vod::FiniteVodProcessClosure),
    RootOnly(&'a crate::adaptive_resources::physical_graph::finite_vod::FiniteVodRootOnlyClosure),
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
