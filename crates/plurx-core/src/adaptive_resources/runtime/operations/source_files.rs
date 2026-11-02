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

    /// Shares the SAME original native descriptor with an already registered
    /// core read job. Arc cloning creates no file description or FD copy.
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current direct-source and fixed-planning native callers are Linux-only; preserve original File custody and portable refusal/close proof APIs on other platforms."
        )
    )]
    pub(in crate::adaptive_resources::runtime) fn shared_file(&self) -> Arc<File> {
        Arc::clone(&self.file)
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
}

/// Audit-only source child observation after the original P document process
/// has positively closed. No descriptor or File alias leaves the SourceCell.
#[cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "Current direct-source and fixed-planning native callers are Linux-only; preserve original File custody and portable refusal/close proof APIs on other platforms."
    )
)]
pub(in crate::adaptive_resources::runtime) struct FixedProbeChildObservation {
    pub observation_id: Uuid,
    pub envelope: v2::Envelope,
    pub acknowledged: bool,
}
struct SourceResult {
    file: Option<Arc<File>>,
    observed: Option<v2::SourceObserved>,
    direct_open_error: Option<std::io::ErrorKind>,
    verified_sha256: Option<[u8; 32]>,
    verified_elf: bool,
    // Instrumentation for the cache regressions; production authority is
    // the retained File identity and VerifiedExecutableCache entry itself.
    #[cfg(test)]
    verified_sha256_cached: bool,
}
enum SourceOpenPlan {
    Original(v2::SourcePlan),
    Direct(v2::DirectSourcePlanV1),
    FixedProbe(v2::FixedProbeDiscoverSourceV1),
}
/// Inline original-job frames used by the direct Source component bound.
/// Dynamic strings and serialized envelopes are charged separately there.
#[cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "Current direct-source and fixed-planning native callers are Linux-only; preserve original File custody and portable refusal/close proof APIs on other platforms."
    )
)]
pub(in crate::adaptive_resources::runtime) const fn direct_source_frame_bytes() -> usize {
    std::mem::size_of::<SourceCell>()
        + std::mem::size_of::<SourceResult>()
        + std::mem::size_of::<SourceOpenPlan>()
        + std::mem::size_of::<JoinHandle<SourceResult>>()
        + std::mem::size_of::<JoinHandle<ParentCloseOutcome>>()
}
impl SourceCell {
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current direct-source and fixed-planning native callers are Linux-only; preserve original File custody and portable refusal/close proof APIs on other platforms."
        )
    )]
    fn direct_known_open_error_kind(&self) -> Option<std::io::ErrorKind> {
        if !matches!(self.plan, v2::MemberPlan::DirectSourceV1 { .. })
            || self.pending.is_some()
            || self.failed_join
        {
            return None;
        }
        let result = self.result.as_ref()?;
        if result.file.is_some() || result.observed.is_some() {
            return None;
        }
        result.direct_open_error
    }
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current direct-source and fixed-planning native callers are Linux-only; preserve original File custody and portable refusal/close proof APIs on other platforms."
        )
    )]
    pub(in crate::adaptive_resources::runtime) fn direct_observed_identity(
        &self,
    ) -> Result<(u64, String), OperationError> {
        let v2::MemberPlan::DirectSourceV1 { source, .. } = &self.plan else {
            return Err(OperationError::State);
        };
        if !self.observed_committed || self.pending.is_some() {
            return Err(OperationError::State);
        }
        let result = self.result.as_ref().ok_or(OperationError::State)?;
        let file = result.file.as_ref().ok_or(OperationError::State)?;
        let observed = result.observed.as_ref().ok_or(OperationError::State)?;
        let metadata = file.metadata()?;
        let actual = SourceIdentity::from_metadata(&metadata).render_s1();
        if !metadata.is_file()
            || (matches!(
                source.expectation,
                v2::DirectSourceExpectationV1::ScannerExact
            ) && !SourceIdentity::scanner_matches(
                &metadata,
                source.scanner_size,
                source.scanner_mtime,
            ))
            || observed.holder_id != source.holder_id
            || observed.member_id != source.member_id
            || observed.source_association.media_file_id != source.media_file_id
            || observed.source_association.recipe_source_binding != source.recipe_source_binding
            || observed.source_association.object_version != actual
            || observed.native_object.device
                != (v2::NativeValue::Known {
                    value: metadata.dev(),
                })
            || observed.native_object.inode
                != (v2::NativeValue::Known {
                    value: metadata.ino(),
                })
            || observed.native_object.mount_id != bootstrap::native::steps::mount_id(file)
            || observed.native_object.filesystem_domain != source.filesystem_domain
        {
            return Err(OperationError::State);
        }
        Ok((metadata.len(), actual))
    }

    /// The scanner-bound P source's observed S1 comes from this retained
    /// original File. It is checked again before the root inherits the fd.
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current direct-source and fixed-planning native callers are Linux-only; preserve original File custody and portable refusal/close proof APIs on other platforms."
        )
    )]
    pub(in crate::adaptive_resources::runtime) fn probe_observed_identity(
        &self,
    ) -> Result<(u64, String), OperationError> {
        let v2::MemberPlan::FixedProbeDiscoverSourceV1 { source, .. } = &self.plan else {
            return Err(OperationError::State);
        };
        if !self.observed_committed || self.pending.is_some() || self.lease_issued {
            return Err(OperationError::State);
        }
        let result = self.result.as_ref().ok_or(OperationError::State)?;
        let file = result.file.as_ref().ok_or(OperationError::State)?;
        let observed = result.observed.as_ref().ok_or(OperationError::State)?;
        let metadata = file.metadata()?;
        let actual = SourceIdentity::from_metadata(&metadata).render_s1();
        if !metadata.is_file()
            || !SourceIdentity::scanner_matches(
                &metadata,
                source.scanner_size,
                source.scanner_mtime,
            )
            || observed.holder_id != source.holder_id
            || observed.member_id != source.member_id
            || observed.copies.as_slice() != &source.planned_copies[..1]
            || observed.source_association.media_file_id != source.media_file_id
            || observed.source_association.recipe_source_binding != source.recipe_source_binding
            || observed.source_association.object_version != actual
            || observed.native_object.device
                != (v2::NativeValue::Known {
                    value: metadata.dev(),
                })
            || observed.native_object.inode
                != (v2::NativeValue::Known {
                    value: metadata.ino(),
                })
            || observed.native_object.mount_id != bootstrap::native::steps::mount_id(file)
            || observed.native_object.filesystem_domain != source.filesystem_domain
        {
            return Err(OperationError::State);
        }
        Ok((metadata.len(), actual))
    }

    /// The original P task is registered under the same bundle lock before
    /// its entry latch opens, so this lease cannot escape an untracked task.
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current direct-source and fixed-planning native callers are Linux-only; preserve original File custody and portable refusal/close proof APIs on other platforms."
        )
    )]
    pub(in crate::adaptive_resources::runtime) fn take_probe_lease(
        &mut self,
    ) -> Result<(RetainedSourceLease, u64, String), OperationError> {
        let (length, object_version) = self.probe_observed_identity()?;
        let file = self
            .result
            .as_ref()
            .and_then(|result| result.file.as_ref())
            .ok_or(OperationError::State)?;
        let lease = RetainedSourceLease {
            file: Arc::clone(file),
        };
        self.lease_issued = true;
        Ok((lease, length, object_version))
    }

    /// Pin this SAME committed original File for an alias-barrier fixture.
    /// This does not consume the collector's one-shot lease or open an FD.
    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn planning_parent_alias_for_test(&self) -> Result<Arc<File>, OperationError> {
        if !matches!(self.plan, v2::MemberPlan::FixedProbeDiscoverSourceV1 { .. })
            || !self.observed_committed
            || self.pending.is_some()
            || self.spec.is_some()
            || self.lease_issued
            || !matches!(self.parent_close, ParentCloseState::NotStarted)
        {
            return Err(OperationError::State);
        }
        let file = self
            .result
            .as_ref()
            .and_then(|result| result.file.as_ref())
            .ok_or(OperationError::State)?;
        Ok(Arc::clone(file))
    }

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
        let plan = match &self.plan {
            v2::MemberPlan::SourceV1 { source, .. } => SourceOpenPlan::Original(source.clone()),
            v2::MemberPlan::DirectSourceV1 { source, .. } => SourceOpenPlan::Direct(source.clone()),
            v2::MemberPlan::FixedProbeDiscoverSourceV1 { source, .. } => {
                SourceOpenPlan::FixedProbe(source.clone())
            }
            _ => return Err(OperationError::State),
        };
        let runtime = tokio::runtime::Handle::try_current().map_err(std::io::Error::other)?;
        let spec = self.spec.take().ok_or(OperationError::State)?;
        let (enter, registered) = tokio::sync::oneshot::channel();
        self.pending = Some(runtime.spawn_blocking(move || {
            if registered.blocking_recv().is_err() {
                return SourceResult {
                    file: None,
                    observed: None,
                    direct_open_error: None,
                    verified_sha256: None,
                    verified_elf: false,
                    #[cfg(test)]
                    verified_sha256_cached: false,
                };
            }
            open_source(spec, plan)
        }));
        Ok(enter)
    }
}

fn open_source(spec: SourceOpenSpec, plan: SourceOpenPlan) -> SourceResult {
    match plan {
        SourceOpenPlan::Original(plan) => open_source_with_cache(spec, plan, &VERIFIED_EXECUTABLES),
        SourceOpenPlan::Direct(plan) => open_direct_source(spec, plan),
        SourceOpenPlan::FixedProbe(plan) => open_fixed_probe_source(spec, plan),
    }
}

fn open_fixed_probe_source(
    spec: SourceOpenSpec,
    plan: v2::FixedProbeDiscoverSourceV1,
) -> SourceResult {
    open_scanner_source(
        spec,
        plan.holder_id,
        plan.member_id,
        plan.media_file_id,
        plan.recipe_source_binding,
        plan.absolute_path,
        plan.scanner_size,
        plan.scanner_mtime,
        true,
        plan.filesystem_domain,
        plan.planned_copies
            .into_iter()
            .filter(|copy| matches!(copy.owner, v2::CopyOwner::DaemonSourceCell { .. }))
            .collect(),
    )
}

fn open_direct_source(spec: SourceOpenSpec, plan: v2::DirectSourcePlanV1) -> SourceResult {
    let scanner_exact = matches!(
        plan.expectation,
        v2::DirectSourceExpectationV1::ScannerExact
    );
    open_scanner_source(
        spec,
        plan.holder_id,
        plan.member_id,
        plan.media_file_id,
        plan.recipe_source_binding,
        plan.absolute_path,
        plan.scanner_size,
        plan.scanner_mtime,
        scanner_exact,
        plan.filesystem_domain,
        plan.planned_copies,
    )
}

#[allow(clippy::too_many_arguments)]
fn open_scanner_source(
    spec: SourceOpenSpec,
    holder_id: v2::Id,
    member_id: v2::Id,
    media_file_id: String,
    recipe_source_binding: String,
    absolute_path: String,
    scanner_size: i64,
    scanner_mtime: i64,
    scanner_exact: bool,
    filesystem_domain: v2::FilesystemDomain,
    planned_copies: Vec<v2::SourceCopy>,
) -> SourceResult {
    let mut result = SourceResult {
        file: None,
        observed: None,
        direct_open_error: None,
        verified_sha256: None,
        verified_elf: false,
        #[cfg(test)]
        verified_sha256_cached: false,
    };
    if spec.expected_sha256.is_some()
        || spec.path.to_str() != Some(absolute_path.as_str())
        || spec.scanner_size != scanner_size
        || spec.scanner_mtime != scanner_mtime
        || !std::fs::metadata("/proc/self/ns/mnt").is_ok_and(|metadata| {
            matches!(filesystem_domain.mount_namespace,
                v2::NativeValue::Known { value } if value == metadata.ino())
        })
    {
        return result;
    }
    let file = match crate::fs_secure::open_read_nofollow_blocking(&spec.path) {
        Ok(file) => file,
        Err(error) => {
            result.direct_open_error = Some(error.kind());
            return result;
        }
    };
    result.file = Some(Arc::new(file));
    let file = result.file.as_ref().expect("original direct native file");
    let Ok(metadata) = file.metadata() else {
        return result;
    };
    if !metadata.is_file()
        || (scanner_exact
            && !SourceIdentity::scanner_matches(&metadata, spec.scanner_size, spec.scanner_mtime))
    {
        return result;
    }
    let actual_s1 = SourceIdentity::from_metadata(&metadata).render_s1();
    result.observed = Some(v2::SourceObserved {
        version: 1,
        holder_id,
        member_id,
        copies: planned_copies,
        native_object: v2::NativeObject {
            file_type: v2::FileType::Regular,
            device: v2::NativeValue::Known {
                value: metadata.dev(),
            },
            inode: v2::NativeValue::Known {
                value: metadata.ino(),
            },
            mount_id: bootstrap::native::steps::mount_id(file),
            filesystem_domain,
        },
        access: v2::SourceAccess::ReadOnlyRegular,
        source_association: v2::SourceAssociation {
            media_file_id,
            object_version: actual_s1,
            recipe_source_binding,
        },
    });
    result
}

fn open_source_with_cache(
    spec: SourceOpenSpec,
    plan: v2::SourcePlan,
    cache: &Mutex<VerifiedExecutableCache>,
) -> SourceResult {
    let mut result = SourceResult {
        file: None,
        observed: None,
        direct_open_error: None,
        verified_sha256: None,
        verified_elf: false,
        #[cfg(test)]
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
            #[cfg(test)]
            {
                result.verified_sha256_cached = true;
            }
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
    /// Diagnostic only: a joined original nofollow-open error before any FD
    /// existed. This does not authorize physical closure or a refund.
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current direct-source and fixed-planning native callers are Linux-only; preserve original File custody and portable refusal/close proof APIs on other platforms."
        )
    )]
    pub(in crate::adaptive_resources::runtime) fn direct_source_known_open_error_kind(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
    ) -> Option<std::io::ErrorKind> {
        self.validate_preparation(owner).ok()?;
        let bundle = owner.state.try_lock().ok()?;
        let source = bundle.roles.get(index)?.sources.get(&member)?;
        source.direct_known_open_error_kind()
    }
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
        let path = spec.path.to_str().ok_or(OperationError::State)?;
        let (slot, domain, copies, binding_ok) = match &plan {
            v2::MemberPlan::SourceV1 { slot, source } => (
                slot,
                &source.filesystem_domain,
                &source.planned_copies,
                digest_binding_matches(
                    &source.source_association.recipe_source_binding,
                    spec.expected_sha256,
                ),
            ),
            v2::MemberPlan::DirectSourceV1 { slot, source } => (
                slot,
                &source.filesystem_domain,
                &source.planned_copies,
                source.absolute_path == path
                    && source.scanner_size == spec.scanner_size
                    && source.scanner_mtime == spec.scanner_mtime
                    && spec.expected_sha256.is_none(),
            ),
            v2::MemberPlan::FixedProbeDiscoverSourceV1 { slot, source } => (
                slot,
                &source.filesystem_domain,
                &source.planned_copies,
                source.absolute_path == path
                    && source.scanner_size == spec.scanner_size
                    && source.scanner_mtime == spec.scanner_mtime
                    && spec.expected_sha256.is_none(),
            ),
            _ => return Err(OperationError::State),
        };
        if !slot.protected_paths.iter().any(|p| p == path)
            || domain.platform != v2::FilesystemPlatform::Linux
            || domain.daemon_owner_id.uuid() != self.owner.owner_id
            || domain.os_boot != self.owner.os_boot
            || !matches!(self.owner.os_boot, BootIdentity::Known { .. })
            || !binding_ok
        {
            return Err(OperationError::State);
        }
        let mut copies = copies
            .iter()
            .filter(|copy| matches!(copy.owner, v2::CopyOwner::DaemonSourceCell { .. }));
        let copy = copies.next().ok_or(OperationError::State)?;
        if copies.next().is_some() {
            return Err(OperationError::State);
        }
        let acquisition_id = copy.acquisition_operation_id;
        if role.finite_vod.as_ref().is_some_and(|finite| {
            !finite.source_members.contains(&member)
                || finite.registered_source_members.contains(&member)
        }) {
            return Err(OperationError::State);
        }
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
        if let Some(finite) = role.finite_vod.as_mut() {
            finite.registered_source_members.insert(member);
        }
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

    /// The one direct lease and its committed actual identity leave the same
    /// locked SourceCell together. No cancellation point follows lease issue.
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current direct-source and fixed-planning native callers are Linux-only; preserve original File custody and portable refusal/close proof APIs on other platforms."
        )
    )]
    pub(in crate::adaptive_resources::runtime) async fn open_prepared_direct_source(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        deadline: Instant,
    ) -> Result<(RetainedSourceLease, u64, String), OperationError> {
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
            || source.lease_issued
        {
            return Err(OperationError::State);
        }
        let (length, object_version) = source.direct_observed_identity()?;
        let file = source
            .result
            .as_ref()
            .and_then(|result| result.file.as_ref())
            .ok_or(OperationError::State)?;
        let lease = RetainedSourceLease {
            file: Arc::clone(file),
        };
        source.lease_issued = true;
        Ok((lease, length, object_version))
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
                observed: Some(match &source.plan {
                    v2::MemberPlan::SourceV1 { .. } => v2::Observed::SourceV1 { source: observed },
                    v2::MemberPlan::DirectSourceV1 { .. } => {
                        v2::Observed::DirectSourceV1 { source: observed }
                    }
                    v2::MemberPlan::FixedProbeDiscoverSourceV1 { .. } => {
                        v2::Observed::FixedProbeDiscoverSourceV1 { source: observed }
                    }
                    _ => return Err(OperationError::State),
                }),
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

/// A slot without an original SourceCell cannot enter an open worker. The
/// caller must prove that exact slot was unregistered and unarmed under the
/// sealed original no-root graph before using this data-only audit.
pub(in crate::adaptive_resources::runtime::operations) fn finite_unregistered_closed_fact(
    member: &v2::MemberPlan,
    closure: &crate::adaptive_resources::physical_graph::finite_vod::FiniteVodNeverRootClosure,
) -> Result<v2::ClosedFact, OperationError> {
    let v2::MemberPlan::SourceV1 { slot, source } = member else {
        return Err(OperationError::State);
    };
    let v2::SourceRecovery::LocalSourceHandlesV1 {
        graph_id,
        owner_id,
        filesystem_domain,
    } = &source.recovery;
    if source.member_id != slot.member_id
        || graph_id.uuid() != closure.generation()
        || *owner_id != source.filesystem_domain.daemon_owner_id
        || filesystem_domain != &source.filesystem_domain
        || source.planned_copies.len() != 2
    {
        return Err(OperationError::State);
    }
    let mut parents = source.planned_copies.iter().filter(|copy| {
        matches!(copy.owner,
            v2::CopyOwner::DaemonSourceCell {
                owner_id: held_owner,
                graph_id: held_graph,
                member_id,
            } if held_owner == *owner_id
                && held_graph == *graph_id
                && member_id == source.member_id
                && copy.parent_copy_id.is_none())
    });
    let parent = parents.next().ok_or(OperationError::State)?;
    if parents.next().is_some() {
        return Err(OperationError::State);
    }
    let mut children = source.planned_copies.iter().filter(|copy| {
        matches!(copy.owner,
            v2::CopyOwner::OwnedRoot {
                graph_id: held_graph,
                root_member_id,
            } if held_graph == *graph_id
                && root_member_id.uuid() == closure.root_member())
            && copy.parent_copy_id == Some(parent.copy_id)
            && copy.acquisition_operation_id.uuid() == closure.acquisition_id()
    });
    let child = children.next().ok_or(OperationError::State)?;
    if children.next().is_some() || parent.copy_id == child.copy_id {
        return Err(OperationError::State);
    }
    let operation_id = v2::Id::new(closure.close_operation_id())?;
    Ok(v2::ClosedFact::SourceV1 {
        holder_id: source.holder_id,
        copies: vec![
            v2::CopyClosed::ProvenNeverAcquired {
                copy: parent.clone(),
                operation_id,
            },
            v2::CopyClosed::ProvenNeverAcquired {
                copy: child.clone(),
                operation_id,
            },
        ],
    })
}

/// The retained original source spec and an unarmed missing SourceCell are
/// checked by the runtime while holding the sealed bundle. No open worker ever
/// owned this slot; both copy dispositions are absence audits under that proof.
#[cfg(target_os = "linux")]
pub(super) fn fixed_probe_unregistered_no_task_closed_fact(
    plan: &v2::MemberPlan,
    proof: &crate::adaptive_resources::physical_graph::fixed_probe::FixedPlanningNoTaskGraph,
) -> Result<v2::ClosedFact, OperationError> {
    let v2::MemberPlan::FixedProbeDiscoverSourceV1 { source, .. } = plan else {
        return Err(OperationError::State);
    };
    if !proof.source_matches(source) {
        return Err(OperationError::State);
    }
    Ok(proof.source_fact(v2::CopyClosed::ProvenNeverAcquired {
        copy: proof.parent_copy().clone(),
        operation_id: proof.operation_id(),
    }))
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
        self.inherited_source_inner(root_member, false)
    }

    #[cfg(target_os = "linux")]
    pub(in crate::adaptive_resources::runtime) fn fixed_probe_child_observation(
        &mut self,
        closure: &crate::adaptive_resources::physical_graph::fixed_probe::FixedProbeNativeClosure,
    ) -> Result<FixedProbeChildObservation, OperationError> {
        let v2::MemberPlan::FixedProbeDiscoverSourceV1 { source, .. } = &self.plan else {
            return Err(OperationError::State);
        };
        if source.member_id.uuid() != closure.source_member()
            || closure.document_identity().pid == 0
            || closure.document_identity().start_marker == 0
        {
            return Err(OperationError::State);
        }
        let inherited = self.inherited_source_inner(closure.document_member(), true)?;
        Ok(FixedProbeChildObservation {
            observation_id: inherited.observation_id,
            envelope: inherited.envelope,
            acknowledged: inherited.acknowledged,
        })
    }

    fn inherited_source_inner(
        &mut self,
        root_member: Uuid,
        allow_issued_lease: bool,
    ) -> Result<InheritedSource, OperationError> {
        if !self.observed_committed || (self.lease_issued && !allow_issued_lease) {
            return Err(OperationError::State);
        }
        let (
            expected_holder_id,
            expected_member_id,
            association,
            filesystem_domain,
            planned_copies,
            recovery,
            fixed_probe,
        ) = match &self.plan {
            v2::MemberPlan::SourceV1 { source, .. } => (
                source.holder_id,
                source.member_id,
                source.source_association.clone(),
                &source.filesystem_domain,
                &source.planned_copies,
                &source.recovery,
                false,
            ),
            v2::MemberPlan::FixedProbeDiscoverSourceV1 { source, .. } => (
                source.holder_id,
                source.member_id,
                v2::SourceAssociation {
                    media_file_id: source.media_file_id.clone(),
                    object_version: self
                        .result
                        .as_ref()
                        .and_then(|result| result.observed.as_ref())
                        .ok_or(OperationError::State)?
                        .source_association
                        .object_version
                        .clone(),
                    recipe_source_binding: source.recipe_source_binding.clone(),
                },
                &source.filesystem_domain,
                &source.planned_copies,
                &source.recovery,
                true,
            ),
            _ => return Err(OperationError::State),
        };
        let result = self.result.as_ref().ok_or(OperationError::State)?;
        let file = result.file.as_ref().ok_or(OperationError::State)?;
        let source_observed = result.observed.as_ref().ok_or(OperationError::State)?;
        let metadata = file.metadata()?;
        let native = &source_observed.native_object;
        if !metadata.is_file()
            || source_observed.holder_id != expected_holder_id
            || source_observed.member_id != expected_member_id
            || source_observed.source_association != association
            || source_observed.access != v2::SourceAccess::ReadOnlyRegular
            || native.file_type != v2::FileType::Regular
            || native.filesystem_domain != *filesystem_domain
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
        let planned_graph_id = match recovery {
            v2::SourceRecovery::LocalSourceHandlesV1 { graph_id, .. } => *graph_id,
        };
        let mut source_copies = source_observed.copies.iter().filter(|copy| {
            matches!(copy.owner,
                v2::CopyOwner::DaemonSourceCell { owner_id, graph_id, member_id: copy_member_id }
                    if owner_id == filesystem_domain.daemon_owner_id
                        && graph_id == planned_graph_id
                        && copy_member_id == expected_member_id)
        });
        let source_copy = source_copies.next().ok_or(OperationError::State)?;
        if source_copies.next().is_some() || !planned_copies.contains(source_copy) {
            return Err(OperationError::State);
        }
        let mut root_copies = planned_copies.iter().filter(|copy| {
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
                observed: Some(if fixed_probe {
                    v2::Observed::FixedProbeDiscoverSourceV1 { source: observed }
                } else {
                    v2::Observed::SourceV1 { source: observed }
                }),
            },
            acknowledged: self.root_observed_committed,
            verified_sha256: result.verified_sha256,
            verified_elf: result.verified_elf,
        })
    }

    /// Closes only this retained parent descriptor. A failed/ambiguous close is
    /// terminal because retrying its numeric fd could close a recycled handle.
    pub(super) async fn close_parent(&mut self) -> Result<(), OperationError> {
        self.close_parent_inner(true, false).await
    }

    /// The direct profile has no inherited/root copy. Join its original open,
    /// then close the exact original File after its one issued lease drains.
    /// A surviving lease makes Arc::try_unwrap refuse without consuming it.
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current direct-source and fixed-planning native callers are Linux-only; preserve original File custody and portable refusal/close proof APIs on other platforms."
        )
    )]
    pub(in crate::adaptive_resources::runtime) async fn close_direct_parent(
        &mut self,
    ) -> Result<(), OperationError> {
        let v2::MemberPlan::DirectSourceV1 { source, .. } = &self.plan else {
            return Err(OperationError::State);
        };
        if source.planned_copies.len() != 1
            || !matches!(
                source.planned_copies[0].owner,
                v2::CopyOwner::DaemonSourceCell { .. }
            )
            || self.root_observed_committed
            || self.selected_root_member.is_some()
            || self.pending_root.is_some()
            || self.failed_join
        {
            return Err(OperationError::State);
        }
        // A registered closer already owns the File. Its result slot is
        // intentionally empty until that SAME task reports closure; absence
        // from the slot cannot bypass its pending or terminal disposition.
        if !matches!(self.parent_close, ParentCloseState::NotStarted) {
            return self.close_parent_inner(false, true).await;
        }
        self.join_registered().await?;
        if self
            .result
            .as_ref()
            .is_some_and(|result| result.file.is_some())
        {
            self.close_parent_inner(false, true).await?;
        }
        Ok(())
    }

    /// Audit the one original source copy from retained native state, never
    /// from a caller's claim that a pathname or descriptor has disappeared.
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current direct-source and fixed-planning native callers are Linux-only; preserve original File custody and portable refusal/close proof APIs on other platforms."
        )
    )]
    pub(in crate::adaptive_resources::runtime) fn direct_closed_fact(
        &self,
        absence_operation_id: Uuid,
    ) -> Result<v2::ClosedFact, OperationError> {
        let v2::MemberPlan::DirectSourceV1 { source, .. } = &self.plan else {
            return Err(OperationError::State);
        };
        let [copy] = source.planned_copies.as_slice() else {
            return Err(OperationError::State);
        };
        if !matches!(copy.owner, v2::CopyOwner::DaemonSourceCell { .. })
            || self.pending.is_some()
            || self.failed_join
            || self.root_observed_committed
            || self.selected_root_member.is_some()
            || self.pending_root.is_some()
        {
            return Err(OperationError::State);
        }
        let fact = if self.parent_closed() {
            if self
                .result
                .as_ref()
                .is_none_or(|result| result.file.is_some())
            {
                return Err(OperationError::State);
            }
            v2::CopyClosed::ActualOwnedClose {
                copy: copy.clone(),
                operation_id: v2::Id::new(self.parent_close_operation_id)?,
            }
        } else if matches!(self.parent_close, ParentCloseState::NotStarted)
            && (self.spec.is_some() && self.result.is_none()
                || self.spec.is_none()
                    && self
                        .result
                        .as_ref()
                        .is_some_and(|result| result.file.is_none() && result.observed.is_none()))
        {
            v2::CopyClosed::ProvenNeverAcquired {
                copy: copy.clone(),
                operation_id: v2::Id::new(absence_operation_id)?,
            }
        } else {
            return Err(OperationError::State);
        };
        Ok(v2::ClosedFact::DirectSourceV1 {
            holder_id: source.holder_id,
            copies: vec![fact],
        })
    }

    /// A sealed no-root graph proves that no child inherited this source.
    /// An original open may still have returned a File before metadata rejected
    /// it, so close that SAME File even without a durable SourceObserved.
    pub(super) async fn close_parent_before_root(
        &mut self,
        closure: &crate::adaptive_resources::physical_graph::finite_vod::FiniteVodNeverRootClosure,
    ) -> Result<(), OperationError> {
        self.no_root_copy_plan(closure)?;
        if !matches!(self.parent_close, ParentCloseState::NotStarted) {
            return self.close_parent_inner(false, false).await;
        }
        self.join_registered().await?;
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
        self.close_parent_inner(false, false).await
    }

    async fn close_parent_inner(
        &mut self,
        require_observed: bool,
        allow_issued_lease: bool,
    ) -> Result<(), OperationError> {
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
                || (self.lease_issued && !allow_issued_lease)
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
            #[cfg(all(
                test,
                feature = "fixtures",
                target_os = "linux",
                any(target_arch = "x86_64", target_arch = "aarch64")
            ))]
            let close_operation = self.parent_close_operation_id;
            let task = runtime.spawn_blocking(move || {
                if registered.blocking_recv().is_err() {
                    return ParentCloseOutcome::NotEntered;
                }
                let raw_fd = file.into_raw_fd();
                // SAFETY: this worker owns the original File allocation, has
                // converted it to a raw descriptor, and calls close once.
                let result = unsafe { libc::close(raw_fd) };
                #[cfg(all(
                    test,
                    feature = "fixtures",
                    target_os = "linux",
                    any(target_arch = "x86_64", target_arch = "aarch64")
                ))]
                {
                    let outcome = if result == 0 {
                        ParentCloseOutcome::Closed
                    } else if let Some(errno) = std::io::Error::last_os_error().raw_os_error() {
                        ParentCloseOutcome::Failed(errno)
                    } else {
                        ParentCloseOutcome::Unknown
                    };
                    super::native_result_fixture::after_result(
                        super::native_result_fixture::Kind::SourceParentClose,
                        close_operation,
                    );
                    outcome
                }
                #[cfg(not(all(
                    test,
                    feature = "fixtures",
                    target_os = "linux",
                    any(target_arch = "x86_64", target_arch = "aarch64")
                )))]
                {
                    if result == 0 {
                        ParentCloseOutcome::Closed
                    } else if let Some(errno) = std::io::Error::last_os_error().raw_os_error() {
                        ParentCloseOutcome::Failed(errno)
                    } else {
                        ParentCloseOutcome::Unknown
                    }
                }
            });
            #[cfg(all(
                test,
                feature = "fixtures",
                target_os = "linux",
                any(target_arch = "x86_64", target_arch = "aarch64")
            ))]
            super::native_result_fixture::bind_job(
                super::native_result_fixture::Kind::SourceParentClose,
                close_operation,
                task.id(),
            );
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

    #[cfg(all(
        test,
        feature = "fixtures",
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    pub(super) fn fixture_parent_close_job(&self) -> Option<tokio::task::Id> {
        match &self.parent_close {
            ParentCloseState::Registered { task, .. } => Some(task.id()),
            _ => None,
        }
    }

    #[cfg(test)]
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

    /// The rolling-copy parser is a distinct fixed actor. It shares the
    /// SourceV1 copy inventory and exact parent/root close audit only after
    /// its own original parser and root closure has been retained.
    #[cfg(target_os = "linux")]
    pub(in crate::adaptive_resources::runtime) fn copy_closed_fact(
        &self,
        closure: &crate::adaptive_resources::physical_graph::copy_vod::CopyProcessClosure,
    ) -> Result<v2::ClosedFact, OperationError> {
        self.finite_closed_fact_with(FiniteProcessClosedProof::CopyJoined(closure))
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

    /// Read-only sticky failure classification, never close/refund evidence.
    /// A cancelled waiter leaves its SAME opener/closer handle retained and
    /// remains retryable. Only consumed join failure or terminal close failure
    /// is permanent; retrying a failed numeric close could hit a recycled fd.
    #[cfg(target_os = "linux")]
    pub(in crate::adaptive_resources::runtime) fn copy_cleanup_permanently_unknown(&self) -> bool {
        if self.pending.is_some()
            || matches!(&self.parent_close, ParentCloseState::Registered { .. })
        {
            return false;
        }
        self.failed_join
            || matches!(
                &self.parent_close,
                ParentCloseState::Failed(_) | ParentCloseState::Unknown
            )
    }

    /// Read only the SAME durably acknowledged root-copy metadata. Cleanup
    /// may already have moved the original File into its retained closer, so
    /// this must never borrow a descriptor, fstat or reconstruct an envelope.
    /// A true result is acknowledgment data only, never native-close proof.
    #[cfg(target_os = "linux")]
    pub(in crate::adaptive_resources::runtime) fn copy_root_observation_committed(
        &self,
        root_member: Uuid,
        acquisition_id: Uuid,
    ) -> Result<bool, OperationError> {
        let v2::MemberPlan::SourceV1 { source: plan, .. } = &self.plan else {
            return Err(OperationError::State);
        };
        let v2::SourceRecovery::LocalSourceHandlesV1 { graph_id, .. } = &plan.recovery;
        let mut roots = plan.planned_copies.iter().filter(|copy| {
            matches!(copy.owner, v2::CopyOwner::OwnedRoot { graph_id: selected, root_member_id }
                if selected == *graph_id && root_member_id.uuid() == root_member)
        });
        let root = roots.next().ok_or(OperationError::State)?;
        if roots.next().is_some()
            || root.acquisition_operation_id.uuid() != acquisition_id
            || plan.planned_copies.len() != 2
        {
            return Err(OperationError::State);
        }
        if !self.root_observed_committed {
            if self.selected_root_member.is_some()
                || self
                    .pending_root
                    .as_ref()
                    .is_some_and(|(member, copy, _)| *member != root_member || copy != root)
            {
                return Err(OperationError::State);
            }
            return Ok(false);
        }
        let observed = self
            .result
            .as_ref()
            .and_then(|result| result.observed.as_ref())
            .ok_or(OperationError::State)?;
        if !self.observed_committed
            || self.selected_root_member != Some(root_member)
            || self.pending_root.is_some()
            || observed.version != 1
            || observed.holder_id != plan.holder_id
            || observed.member_id != plan.member_id
            || observed.source_association != plan.source_association
            || observed.access != plan.access
            || observed.native_object.filesystem_domain != plan.filesystem_domain
            || observed.copies != plan.planned_copies
        {
            return Err(OperationError::State);
        }
        Ok(true)
    }

    /// Borrow only metadata from the SAME acknowledged source envelope for
    /// pure settlement preflight. An open result before journal acknowledgment
    /// is not a durable observation. The File/close worker is never accessed.
    #[cfg(target_os = "linux")]
    pub(in crate::adaptive_resources::runtime) fn copy_observed_for_settlement(
        &self,
        expected: &v2::MemberPlan,
    ) -> Result<Option<&v2::SourceObserved>, OperationError> {
        if &self.plan != expected || !matches!(expected, v2::MemberPlan::SourceV1 { .. }) {
            return Err(OperationError::State);
        }
        if !self.observed_committed {
            if self.root_observed_committed || self.selected_root_member.is_some() {
                return Err(OperationError::State);
            }
            return Ok(None);
        }
        if self.root_observed_committed {
            let root = self.selected_root_member.ok_or(OperationError::State)?;
            let v2::MemberPlan::SourceV1 { source: plan, .. } = expected else {
                return Err(OperationError::State);
            };
            let acquisition = plan
                .planned_copies
                .iter()
                .find_map(|copy| {
                    matches!(copy.owner, v2::CopyOwner::OwnedRoot { root_member_id, .. }
                    if root_member_id.uuid() == root)
                    .then_some(copy.acquisition_operation_id.uuid())
                })
                .ok_or(OperationError::State)?;
            if !self.copy_root_observation_committed(root, acquisition)? {
                return Err(OperationError::State);
            }
        } else if self.selected_root_member.is_some() {
            return Err(OperationError::State);
        }
        self.result
            .as_ref()
            .and_then(|result| result.observed.as_ref())
            .map(Some)
            .ok_or(OperationError::State)
    }

    /// Negative fixture corruption AFTER real native closure/proof minting.
    /// This touches no File/FD or closure capability; the existing fact
    /// validator must reject parent/root close-operation identity collision.
    #[cfg(all(test, target_os = "linux"))]
    pub(in crate::adaptive_resources::runtime::operations) fn fixture_parent_close_operation_collision(
        &mut self,
        root_close_operation: Uuid,
    ) {
        assert!(self.parent_closed());
        self.parent_close_operation_id = root_close_operation;
    }

    /// The SAME copy root's native closure, with no registered parser, closes
    /// only the inventoried inherited copy after this cell's checked parent
    /// close. Unobserved root-copy enrichment still must match its exact slot.
    #[cfg(target_os = "linux")]
    pub(in crate::adaptive_resources::runtime) fn copy_root_only_closed_fact(
        &self,
        closure: &crate::adaptive_resources::physical_graph::copy_vod::CopyRootOnlyClosure,
    ) -> Result<v2::ClosedFact, OperationError> {
        self.finite_closed_fact_with(FiniteProcessClosedProof::CopyRootOnly(closure))
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
                #[cfg(target_os = "linux")]
                FiniteProcessClosedProof::CopyJoined(closure) => (
                    closure.generation(),
                    closure.root_member(),
                    closure.acquisition_id(),
                    closure.close_operation_id(),
                    true,
                ),
                #[cfg(target_os = "linux")]
                FiniteProcessClosedProof::CopyRootOnly(closure) => (
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

    /// Validate the immutable source selection against the original sealed
    /// no-task graph. This says nothing about whether its parent open ran.
    #[cfg(target_os = "linux")]
    fn fixed_probe_no_task_plan(
        &self,
        proof: &crate::adaptive_resources::physical_graph::fixed_probe::FixedPlanningNoTaskGraph,
    ) -> Result<(), OperationError> {
        self.fixed_probe_native_absence_plan(proof, false)
    }

    #[cfg(target_os = "linux")]
    fn fixed_probe_native_absence_plan(
        &self,
        proof: &crate::adaptive_resources::physical_graph::fixed_probe::FixedPlanningNoTaskGraph,
        joined_original_lease: bool,
    ) -> Result<(), OperationError> {
        let v2::MemberPlan::FixedProbeDiscoverSourceV1 { source, .. } = &self.plan else {
            return Err(OperationError::State);
        };
        if !proof.source_matches(source)
            || self.lease_issued && !joined_original_lease
            || self.failed_join
            || self.root_observed_committed
            || self.selected_root_member.is_some()
            || self.pending_root.is_some()
            || self
                .result
                .as_ref()
                .and_then(|result| result.observed.as_ref())
                .is_some_and(|observed| {
                    observed.copies.as_slice() != std::slice::from_ref(proof.parent_copy())
                        || observed.member_id != source.member_id
                        || observed.holder_id != source.holder_id
                })
        {
            return Err(OperationError::State);
        }
        Ok(())
    }

    /// Join only the already registered opener, then close only its original
    /// File. Never enter a fresh open merely to obtain cleanup evidence.
    #[cfg(target_os = "linux")]
    pub(super) async fn close_fixed_probe_parent_before_task(
        &mut self,
        proof: &crate::adaptive_resources::physical_graph::fixed_probe::FixedPlanningNoTaskGraph,
    ) -> Result<(), OperationError> {
        self.fixed_probe_no_task_plan(proof)?;
        if !matches!(self.parent_close, ParentCloseState::NotStarted) {
            return self.close_parent_inner(false, false).await;
        }
        self.join_registered().await?;
        if self.spec.is_some() {
            return if self.result.is_none() {
                Ok(())
            } else {
                Err(OperationError::State)
            };
        }
        let result = self.result.as_ref().ok_or(OperationError::State)?;
        if result.file.is_none() {
            return if result.observed.is_none() {
                Ok(())
            } else {
                Err(OperationError::State)
            };
        }
        self.close_parent_inner(false, false).await
    }

    #[cfg(target_os = "linux")]
    fn fixed_probe_registered_nonentry_plan(
        &self,
        proof: &crate::adaptive_resources::physical_graph::fixed_probe::FixedPlanningNoTaskGraph,
        joined: &super::planning_tasks::JoinedPlanningTaskNeverEntered,
        binding: ReservationBinding,
        join_operation: Uuid,
    ) -> Result<(), OperationError> {
        if !self.lease_issued
            || !proof.registered_probe_matches(binding, joined.member())
            || !joined.matches(binding, joined.member(), Some(join_operation))
        {
            return Err(OperationError::State);
        }
        self.fixed_probe_native_absence_plan(proof, true)
    }

    /// A registered probe issued its original held-source lease. Only the
    /// SAME positively joined receiver-error witness authorizes checking that
    /// original lease for extinction; Arc::try_unwrap still refuses aliases.
    #[cfg(target_os = "linux")]
    pub(super) async fn close_fixed_probe_parent_after_registered_nonentry(
        &mut self,
        proof: &crate::adaptive_resources::physical_graph::fixed_probe::FixedPlanningNoTaskGraph,
        joined: &super::planning_tasks::JoinedPlanningTaskNeverEntered,
        binding: ReservationBinding,
        join_operation: Uuid,
    ) -> Result<(), OperationError> {
        self.fixed_probe_registered_nonentry_plan(proof, joined, binding, join_operation)?;
        self.join_registered().await?;
        self.close_parent_inner(true, true).await
    }

    #[cfg(target_os = "linux")]
    pub(super) fn fixed_probe_registered_nonentry_closed_fact(
        &self,
        proof: &crate::adaptive_resources::physical_graph::fixed_probe::FixedPlanningNoTaskGraph,
        joined: &super::planning_tasks::JoinedPlanningTaskNeverEntered,
        binding: ReservationBinding,
        join_operation: Uuid,
    ) -> Result<v2::ClosedFact, OperationError> {
        self.fixed_probe_registered_nonentry_plan(proof, joined, binding, join_operation)?;
        self.fixed_probe_native_absence_closed_fact(proof, true)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn with_fixed_probe_unentered_observation(
        &mut self,
        preflight: impl FnOnce(&Option<v2::Observed>) -> Result<(), OperationError>,
    ) -> Result<(), OperationError> {
        if !self.observed_committed
            || self.root_observed_committed
            || self.pending_root.is_some()
            || !self.parent_closed()
        {
            return Err(OperationError::State);
        }
        // Lend the SAME acknowledged metadata without cloning its dynamic
        // strings/copy vector. Synchronous RAII restores it even if preflight
        // rejects or unwinds; no original owner is moved across an await.
        struct Restore<'a> {
            slot: &'a mut Option<v2::SourceObserved>,
            envelope: Option<v2::Observed>,
        }
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                if let Some(v2::Observed::FixedProbeDiscoverSourceV1 { source }) =
                    self.envelope.take()
                {
                    *self.slot = Some(source);
                }
            }
        }
        let slot = &mut self.result.as_mut().ok_or(OperationError::State)?.observed;
        let source = slot.take().ok_or(OperationError::State)?;
        let restore = Restore {
            slot,
            envelope: Some(v2::Observed::FixedProbeDiscoverSourceV1 { source }),
        };
        preflight(&restore.envelope)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn fixed_probe_no_task_closed_fact(
        &self,
        proof: &crate::adaptive_resources::physical_graph::fixed_probe::FixedPlanningNoTaskGraph,
    ) -> Result<v2::ClosedFact, OperationError> {
        self.fixed_probe_native_absence_closed_fact(proof, false)
    }

    #[cfg(target_os = "linux")]
    fn fixed_probe_native_absence_closed_fact(
        &self,
        proof: &crate::adaptive_resources::physical_graph::fixed_probe::FixedPlanningNoTaskGraph,
        joined_original_lease: bool,
    ) -> Result<v2::ClosedFact, OperationError> {
        self.fixed_probe_native_absence_plan(proof, joined_original_lease)?;
        if self.pending.is_some() {
            return Err(OperationError::State);
        }
        let parent = if self.parent_closed()
            && self
                .result
                .as_ref()
                .is_some_and(|result| result.file.is_none())
        {
            v2::CopyClosed::ActualOwnedClose {
                copy: proof.parent_copy().clone(),
                operation_id: v2::Id::new(self.parent_close_operation_id)?,
            }
        } else if matches!(self.parent_close, ParentCloseState::NotStarted)
            && (self.spec.is_some() && self.result.is_none()
                || self.spec.is_none()
                    && self
                        .result
                        .as_ref()
                        .is_some_and(|result| result.file.is_none() && result.observed.is_none()))
        {
            v2::CopyClosed::ProvenNeverAcquired {
                copy: proof.parent_copy().clone(),
                operation_id: proof.operation_id(),
            }
        } else {
            return Err(OperationError::State);
        };
        Ok(proof.source_fact(parent))
    }

    /// The original P task has joined and dropped its held-source aliases.
    /// Close this SourceCell's SAME original File after the document-root
    /// observation was committed from the native close capability.
    #[cfg(target_os = "linux")]
    pub(in crate::adaptive_resources::runtime) async fn close_fixed_probe_parent(
        &mut self,
        closure: &crate::adaptive_resources::physical_graph::fixed_probe::FixedProbeNativeClosure,
    ) -> Result<(), OperationError> {
        let v2::MemberPlan::FixedProbeDiscoverSourceV1 { source, .. } = &self.plan else {
            return Err(OperationError::State);
        };
        if source.member_id.uuid() != closure.source_member()
            || !self.root_observed_committed
            || self.selected_root_member != Some(closure.document_member())
            || self.pending_root.is_some()
            || self.failed_join
        {
            return Err(OperationError::State);
        }
        if !matches!(self.parent_close, ParentCloseState::NotStarted) {
            return self.close_parent_inner(true, true).await;
        }
        self.join_registered().await?;
        self.close_parent_inner(true, true).await
    }

    #[cfg(target_os = "linux")]
    pub(in crate::adaptive_resources::runtime) fn fixed_probe_closed_fact(
        &self,
        closure: &crate::adaptive_resources::physical_graph::fixed_probe::FixedProbeNativeClosure,
    ) -> Result<v2::ClosedFact, OperationError> {
        let v2::MemberPlan::FixedProbeDiscoverSourceV1 { source, .. } = &self.plan else {
            return Err(OperationError::State);
        };
        if source.member_id.uuid() != closure.source_member()
            || !self.parent_closed()
            || self.pending.is_some()
            || self.failed_join
            || !self.observed_committed
            || !self.root_observed_committed
            || self.selected_root_member != Some(closure.document_member())
            || self.pending_root.is_some()
            || source.planned_copies.len() != 2
            || self.parent_close_operation_id == closure.document_close_operation_id()
        {
            return Err(OperationError::State);
        }
        let observed = self
            .result
            .as_ref()
            .filter(|result| result.file.is_none())
            .and_then(|result| result.observed.as_ref())
            .ok_or(OperationError::State)?;
        if observed.copies != source.planned_copies
            || observed.holder_id != source.holder_id
            || observed.member_id != source.member_id
            || observed.source_association.media_file_id != source.media_file_id
            || observed.source_association.recipe_source_binding != source.recipe_source_binding
        {
            return Err(OperationError::State);
        }
        let parent = &source.planned_copies[0];
        let child = &source.planned_copies[1];
        if !matches!(parent.owner, v2::CopyOwner::DaemonSourceCell { member_id, .. }
                if member_id == source.member_id)
            || child.parent_copy_id != Some(parent.copy_id)
        {
            return Err(OperationError::State);
        }
        let child_closed = closure.closed_document_source_copy(child)?;
        Ok(v2::ClosedFact::FixedProbeDiscoverSourceV1 {
            holder_id: source.holder_id,
            copies: vec![
                v2::CopyClosed::ActualOwnedClose {
                    copy: parent.clone(),
                    operation_id: v2::Id::new(self.parent_close_operation_id)?,
                },
                child_closed,
            ],
        })
    }
}

enum FiniteProcessClosedProof<'a> {
    Joined(&'a crate::adaptive_resources::physical_graph::finite_vod::FiniteVodProcessClosure),
    RootOnly(&'a crate::adaptive_resources::physical_graph::finite_vod::FiniteVodRootOnlyClosure),
    #[cfg(target_os = "linux")]
    CopyJoined(&'a crate::adaptive_resources::physical_graph::copy_vod::CopyProcessClosure),
    #[cfg(target_os = "linux")]
    CopyRootOnly(&'a crate::adaptive_resources::physical_graph::copy_vod::CopyRootOnlyClosure),
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
    // The planning source issued its one probe lease to the original P task;
    // its child observation is committed only after that task joined.
    let lease_allowed = matches!(
        source.plan,
        v2::MemberPlan::FixedProbeDiscoverSourceV1 { .. }
    );
    if source.root_observation_id != operation
        || !source.observed_committed
        || (source.lease_issued && !lease_allowed)
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
    let (planned_copies, recovery, filesystem_domain, plan_member_id) = match &source.plan {
        v2::MemberPlan::SourceV1 { source: plan, .. } => (
            &plan.planned_copies,
            &plan.recovery,
            &plan.filesystem_domain,
            plan.member_id,
        ),
        v2::MemberPlan::FixedProbeDiscoverSourceV1 { source: plan, .. } => (
            &plan.planned_copies,
            &plan.recovery,
            &plan.filesystem_domain,
            plan.member_id,
        ),
        _ => return Err(OperationError::State),
    };
    let base = source
        .result
        .as_ref()
        .and_then(|result| result.observed.as_ref())
        .ok_or(OperationError::State)?;
    let planned_graph_id = match recovery {
        v2::SourceRecovery::LocalSourceHandlesV1 { graph_id, .. } => *graph_id,
    };
    let mut source_copies = base.copies.iter().filter(|copy| {
        matches!(copy.owner,
            v2::CopyOwner::DaemonSourceCell { owner_id, graph_id, member_id }
                if owner_id == filesystem_domain.daemon_owner_id
                    && graph_id == planned_graph_id
                    && member_id == plan_member_id)
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
        || !planned_copies.contains(&root_copy)
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
