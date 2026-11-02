//! Admitted, rootless Source playback from one original retained descriptor.
//! A direct body selects positional reads; it never receives a cloneable File.

#[cfg(all(test, target_os = "linux"))]
mod tests;

use super::operations::plans::{
    DirectReadDomainRef, DirectSourcePlan, TypedGraphPlan, TypedMemberPlan,
};
use super::operations::source_files::{RetainedSourceLease, SourceOpenSpec};
use super::operations::{OperationError, OwnedPreparation};
use super::response_memory::{FiniteResponseBuffer, ResponseBudget};
use super::NodeResourceRuntime;
use crate::adaptive_resources::{
    AdmissionRefusal, FamilyDemand, FamilyRole, FamilyRoleDemand, ResourceVector, WorkloadDemand,
};
use crate::process::observation::Metric;
use crate::resource_custody::{codec_v2 as v2, BootIdentity};
use crate::sharing_source_sessions::SourceDispatchAssignment;
use crate::source_identity::SourceIdentity;
use std::fmt;
use std::path::PathBuf;
#[cfg(any(test, feature = "fixtures"))]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Condvar,
};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{oneshot, Mutex as AsyncMutex, Notify, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use uuid::Uuid;

const MAX_BODIES: usize = 64;
const MAX_CHUNK_BYTES: usize = 64 * 1024;
const MAX_CHUNKS: usize = 64;
const MAX_PATH_BYTES: usize = 4096;
const MAX_MEDIA_ID_BYTES: usize = 4096;
const MAX_BOOT_PROVIDER_BYTES: usize = 128;
const MAX_S1_BYTES: usize = 160;
// The one-member plan is projected into preparation, manifest, member row,
// SourceCell, native job, observation, journal result, and settlement. Count
// their retry-held forms with this fixed higher clone ceiling.
const MAX_SOURCE_TEXT_COPIES: usize = 32;
// This direct manifest has one slot, one copy, path/media strings each at
// most 4096 bytes, boot provider at most 128 bytes, and observed S1 at most
// 160 bytes. Thus every direct plan, envelope, operation, result and terminal
// settlement is strictly below the journal's 64 KiB JSON limit. Keep room for
// their bounded preparation, queue and retry copies together.
const MAX_CODEC_JSON_BYTES: usize = 64 * 1024;
const MAX_SOURCE_CODEC_COPIES: usize = 8;
const MAX_DOMAIN_CODEC_COPIES: usize = 4;
// Arc allocates its strong/weak counts beside each payload. The fixed direct
// profile has one owner, application slot, budget, retained File and bundle;
// each registered read job adds one more Arc payload.
const ARC_HEADER_BYTES: usize = 2 * std::mem::size_of::<usize>();

fn invalid() -> DirectSourceError {
    DirectSourceError(OperationError::State)
}
// Used only while constructing the direct plan, before either branch calls
// prepare_typed_families. Never use this classifier on a preparation result.
fn invalid_before_reservation() -> DirectSourceError {
    DirectSourceError(OperationError::BeforeReservation(
        super::operations::PreReservationRefusal::Admission(AdmissionRefusal::InvalidBundle),
    ))
}
fn before_reservation_error(error: OperationError) -> DirectSourceError {
    match error {
        OperationError::BeforeReservation(_) => DirectSourceError(error),
        _ => invalid_before_reservation(),
    }
}
fn id() -> Result<v2::Id, DirectSourceError> {
    v2::Id::new(Uuid::new_v4()).map_err(|_| invalid_before_reservation())
}

#[derive(Debug)]
pub struct DirectSourceError(OperationError);
impl DirectSourceError {
    /// Only an explicitly tagged refusal at a site before the original R.
    /// Generic Admission, Draining, State and physical errors may occur after
    /// reservation; their absence of a returned owner proves no nonentry.
    pub fn definitely_before_reservation(&self) -> bool {
        matches!(self.0, OperationError::BeforeReservation(_))
    }
}
impl fmt::Display for DirectSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}
impl std::error::Error for DirectSourceError {}
impl From<OperationError> for DirectSourceError {
    fn from(error: OperationError) -> Self {
        Self(error)
    }
}
impl From<AdmissionRefusal> for DirectSourceError {
    fn from(error: AdmissionRefusal) -> Self {
        Self(error.into())
    }
}

pub struct DirectSourceRequest {
    pub binding: DirectSourceBinding,
    pub path: PathBuf,
    pub media_file_id: String,
    pub scanner_size: i64,
    pub scanner_mtime: i64,
    pub demand: WorkloadDemand,
    pub max_bodies: usize,
    pub chunk_bytes: usize,
    pub outstanding_chunks: usize,
    /// Exact daemon controller/registry/future capacity measured by the
    /// trusted call site and held through its actual metadata Drop.
    pub application_owner_bytes: u64,
}

/// Existing authorization remains with the daemon's sharing assignment or
/// authenticated local MediaFile lookup. This nonserde value only records the
/// provenance of the selected, scanner-bound path before the admitted open.
#[allow(
    clippy::large_enum_variant,
    reason = "This in-place pre-admission provenance value is consumed into the source plan before reservation; boxing Sharing adds a heap owner outside the current request projections."
)]
pub enum DirectSourceBinding {
    Sharing(SourceDispatchAssignment),
    Local {
        authenticated_user_id: i64,
        file_id: i64,
    },
    FileGrant {
        verified_grant_id: Uuid,
        user_id: i64,
        file_id: i64,
    },
}

impl DirectSourceRequest {
    /// Caller-visible lower bound. The runtime rechecks this against the
    /// complete current demand before reserving the real journal role.
    pub fn minimum_memory_bytes(
        path_len: usize,
        media_id_len: usize,
        max_bodies: usize,
        chunk_bytes: usize,
        outstanding_chunks: usize,
        application_owner_bytes: u64,
    ) -> Option<u64> {
        if path_len == 0
            || path_len > MAX_PATH_BYTES
            || media_id_len == 0
            || media_id_len > MAX_MEDIA_ID_BYTES
            || max_bodies == 0
            || max_bodies > MAX_BODIES
            || chunk_bytes == 0
            || chunk_bytes > MAX_CHUNK_BYTES
            || outstanding_chunks < max_bodies
            || outstanding_chunks > MAX_CHUNKS
        {
            return None;
        }
        let chunks =
            FiniteResponseBuffer::required_partition_bytes(chunk_bytes, outstanding_chunks)?;
        let text_per_projection = path_len
            .checked_add(media_id_len)?
            .checked_add(2 * 64)?
            .checked_add(MAX_BOOT_PROVIDER_BYTES)?
            .checked_add(MAX_S1_BYTES)?;
        let text = text_per_projection.checked_mul(MAX_SOURCE_TEXT_COPIES)?;
        let bodies = max_bodies.checked_mul(std::mem::size_of::<DirectSourceBody>())?;
        let read_job_frame = std::mem::size_of::<ReadJob>()
            .checked_add(std::mem::size_of::<ReadJobState>())?
            .checked_add(std::mem::size_of::<
                JoinHandle<Result<FiniteResponseBuffer, DirectSourceError>>,
            >())?
            .checked_add(std::mem::size_of::<Arc<std::fs::File>>())?
            .checked_add(std::mem::size_of::<FiniteResponseBuffer>())?
            .checked_add(MAX_S1_BYTES)?
            .checked_add(ARC_HEADER_BYTES)?
            .checked_add(std::mem::size_of::<Uuid>())?
            .checked_add(std::mem::size_of::<Arc<ReadJob>>())?;
        let jobs = outstanding_chunks.checked_mul(read_job_frame)?;
        let frames = std::mem::size_of::<DirectSourceOwner>()
            .checked_add(std::mem::size_of::<DirectInner>())?
            .checked_add(std::mem::size_of::<DirectState>())?
            .checked_add(std::mem::size_of::<ApplicationLeaseState>())?
            .checked_add(std::mem::size_of::<DirectSourceApplicationLease>())?
            .checked_add(std::mem::size_of::<DirectSourceRetirementTicket>())?
            .checked_add(std::mem::size_of::<SourceOpenSpec>())?
            .checked_add(std::mem::size_of::<DirectSourcePlan>())?
            .checked_add(std::mem::size_of::<TypedGraphPlan>())?
            .checked_add(std::mem::size_of::<TypedMemberPlan>())?
            .checked_add(super::operations::direct_source::direct_source_role_frame_bytes())?
            .checked_add(bodies)?
            .checked_add(jobs)?
            .checked_add(std::mem::size_of::<std::fs::File>())?
            .checked_add(5 * ARC_HEADER_BYTES)?
            .checked_add(MAX_SOURCE_CODEC_COPIES * MAX_CODEC_JSON_BYTES)?
            .checked_add(text)?;
        chunks
            .checked_add(u64::try_from(frames).ok()?)?
            .checked_add(application_owner_bytes)
    }
}

struct DirectState {
    spec: Option<SourceOpenSpec>,
    lease: Option<RetainedSourceLease>,
    length: Option<u64>,
    object_version: Option<String>,
    closing: bool,
    settled: bool,
    source_settled: bool,
    native_closed: bool,
    source_forgotten: bool,
    bodies: usize,
    read_jobs: [Option<(Uuid, Arc<ReadJob>)>; MAX_CHUNKS],
}

/// One node-local, journal-charged CPU execution lane. Every source joins an
/// exact obligation in this domain; no request can mint its own CPU allowance.
pub(super) struct DirectReadDomain {
    preparation: OwnedPreparation,
    obligation_id: Uuid,
    member: Uuid,
    domain_id: Uuid,
    semaphore: Arc<Semaphore>,
    state: Mutex<DomainState>,
}
/// Nonserde positive closure selection constructed only after every exact
/// member's original native read jobs and SourceCell have closed and its
/// typed source settlement is known. No request can manufacture this token.
pub(in crate::adaptive_resources::runtime) struct DirectReadDomainReady {
    domain: Arc<DirectReadDomain>,
}
impl DirectReadDomainReady {
    pub(in crate::adaptive_resources::runtime) fn matches(
        &self,
        obligation_id: Uuid,
        member: Uuid,
        domain_id: Uuid,
    ) -> bool {
        self.domain.obligation_id == obligation_id
            && self.domain.member == member
            && self.domain.domain_id == domain_id
            && self.domain.semaphore.is_closed()
            && self
                .domain
                .state
                .lock()
                .is_ok_and(|state| state.closing && state.members.iter().all(Option::is_none))
    }
}
struct DomainState {
    members: [Option<Uuid>; 64],
    closing: bool,
    settled: bool,
    forgotten: bool,
}

impl DirectReadDomain {
    fn minimum_memory_bytes() -> Option<u64> {
        let frames = std::mem::size_of::<Self>()
            .checked_add(std::mem::size_of::<DomainState>())?
            .checked_add(std::mem::size_of::<Semaphore>())?
            .checked_add(std::mem::size_of::<OwnedPreparation>())?
            .checked_add(super::operations::direct_source::direct_source_role_frame_bytes())?
            .checked_add(3 * ARC_HEADER_BYTES)?
            .checked_add(MAX_DOMAIN_CODEC_COPIES * MAX_CODEC_JSON_BYTES)?;
        u64::try_from(frames).ok()
    }
}

/// The original registered blocking task stays in this cell even if its
/// request future times out or is cancelled. A later retirement joins it.
struct ReadJob {
    state: AsyncMutex<ReadJobState>,
}
struct ReadJobState {
    task: Option<JoinHandle<Result<FiniteResponseBuffer, DirectSourceError>>>,
    completed: Option<Result<FiniteResponseBuffer, DirectSourceError>>,
    unknown: bool,
}
impl ReadJob {
    async fn join(&self) -> Result<(), DirectSourceError> {
        let mut state = self.state.lock().await;
        if state.unknown {
            return Err(invalid());
        }
        if let Some(task) = state.task.as_mut() {
            let joined = task.await;
            state.task = None;
            match joined {
                Ok(result) => state.completed = Some(result),
                Err(_) => {
                    // Panic or an unobserved task outcome is not native close
                    // proof. Retain the role and refuse settlement.
                    state.unknown = true;
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }

    fn take_completed(&self) -> Result<FiniteResponseBuffer, DirectSourceError> {
        let mut state = self.state.try_lock().map_err(|_| invalid())?;
        if state.unknown || state.task.is_some() {
            return Err(invalid());
        }
        state.completed.take().ok_or_else(invalid)?
    }
}

struct DirectInner {
    state: Mutex<DirectState>,
    changed: Notify,
    open_gate: AsyncMutex<()>,
    finish_gate: AsyncMutex<()>,
    budget: Arc<ResponseBudget>,
    max_bodies: usize,
    chunk_bytes: usize,
    application: Arc<ApplicationLeaseState>,
    #[cfg(any(test, feature = "fixtures"))]
    read_gate: Mutex<Option<Arc<DirectSourceReadGate>>>,
}
struct ApplicationLeaseState {
    state: Mutex<ApplicationLeaseStatus>,
    changed: Notify,
}
struct ApplicationLeaseStatus {
    held: bool,
}
pub struct DirectSourceApplicationLease {
    state: Arc<ApplicationLeaseState>,
}
impl Drop for DirectSourceApplicationLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.state.lock() {
            state.held = false;
            drop(state);
            self.state.changed.notify_waiters();
        }
    }
}
pub struct DirectSourceRetirementTicket {
    runtime: Arc<NodeResourceRuntime>,
    preparation: OwnedPreparation,
    member: Uuid,
    role_index: usize,
    domain: Arc<DirectReadDomain>,
    inner: Arc<DirectInner>,
}

/// Fixture-only barrier in the actual registered positional read job. It
/// provides observations, not a native-closure or settlement capability.
#[cfg(any(test, feature = "fixtures"))]
pub struct DirectSourceReadGate {
    release: Mutex<bool>,
    wake: Condvar,
    entered: AtomicBool,
    complete: AtomicBool,
    file_closed: AtomicBool,
}
#[cfg(any(test, feature = "fixtures"))]
impl DirectSourceReadGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            release: Mutex::new(false),
            wake: Condvar::new(),
            entered: AtomicBool::new(false),
            complete: AtomicBool::new(false),
            file_closed: AtomicBool::new(false),
        })
    }
    pub fn entered(&self) -> bool {
        self.entered.load(Ordering::Acquire)
    }
    pub fn complete(&self) -> bool {
        self.complete.load(Ordering::Acquire)
    }
    pub fn file_closed(&self) -> bool {
        self.file_closed.load(Ordering::Acquire)
    }
    pub fn release(&self) {
        if let Ok(mut released) = self.release.lock() {
            *released = true;
            self.wake.notify_all();
        }
    }
    fn park_real_job(&self) {
        self.entered.store(true, Ordering::Release);
        if let Ok(released) = self.release.lock() {
            let _ = self.wake.wait_timeout_while(
                released,
                std::time::Duration::from_secs(30),
                |released| !*released,
            );
        }
    }
}

pub struct DirectSourceOwner {
    runtime: Arc<NodeResourceRuntime>,
    preparation: OwnedPreparation,
    member: Uuid,
    role_index: usize,
    domain: Arc<DirectReadDomain>,
    inner: Arc<DirectInner>,
}

/// One actually counted body. Its drop ends future reads, while returned
/// chunks independently retain their response-memory credit through Bytes.
pub struct DirectSourceBody {
    inner: Arc<DirectInner>,
}
impl Drop for DirectSourceBody {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock().expect("direct source body");
        state.bodies -= 1;
        drop(state);
        self.inner.changed.notify_waiters();
    }
}

impl NodeResourceRuntime {
    /// Synchronous charge and durable-prepare registration precede every
    /// selected file open. The plan contains scanner facts, not invented S1.
    pub fn prepare_direct_source(
        self: &Arc<Self>,
        request: DirectSourceRequest,
        deadline: Instant,
    ) -> Result<(DirectSourceOwner, DirectSourceApplicationLease), DirectSourceError> {
        let DirectSourceRequest {
            binding,
            path,
            media_file_id,
            scanner_size,
            scanner_mtime,
            demand,
            max_bodies,
            chunk_bytes,
            outstanding_chunks,
            application_owner_bytes,
        } = request;
        let path_text = path.to_str().ok_or_else(invalid_before_reservation)?;
        let minimum_memory = DirectSourceRequest::minimum_memory_bytes(
            path_text.len(),
            media_file_id.len(),
            max_bodies,
            chunk_bytes,
            outstanding_chunks,
            application_owner_bytes,
        )
        .ok_or_else(invalid_before_reservation)?;
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_millis()).ok())
            .ok_or_else(invalid_before_reservation)?;
        if Instant::now() >= deadline
            || !path.is_absolute()
            || media_file_id.capacity() > MAX_MEDIA_ID_BYTES
            || scanner_size < 0
            || !matches!(&self.owner.os_boot,
                BootIdentity::Known { provider, .. } if provider.len() <= MAX_BOOT_PROVIDER_BYTES)
        {
            return Err(invalid_before_reservation());
        }
        let (provenance, expectation) = match binding {
            DirectSourceBinding::Sharing(assignment) => {
                if media_file_id != assignment.binding().file_id().as_str()
                    || assignment.binding().is_released()
                    || assignment.validate_observation_freshness(now_ms).is_err()
                {
                    return Err(invalid_before_reservation());
                }
                (
                    v2::DirectSourceProvenanceV1::Sharing {
                        assignment_custody_identity: assignment.custody_identity(),
                    },
                    v2::DirectSourceExpectationV1::ScannerExact,
                )
            }
            DirectSourceBinding::Local {
                authenticated_user_id,
                file_id,
            } => {
                if authenticated_user_id <= 0
                    || file_id <= 0
                    || media_file_id != file_id.to_string()
                {
                    return Err(invalid_before_reservation());
                }
                (
                    v2::DirectSourceProvenanceV1::Local {
                        authenticated_user_id,
                        file_id,
                    },
                    v2::DirectSourceExpectationV1::AuthorizedLocalPath,
                )
            }
            DirectSourceBinding::FileGrant {
                verified_grant_id,
                user_id,
                file_id,
            } => {
                if verified_grant_id.is_nil()
                    || user_id <= 0
                    || file_id <= 0
                    || media_file_id != file_id.to_string()
                {
                    return Err(invalid_before_reservation());
                }
                (
                    v2::DirectSourceProvenanceV1::FileGrant {
                        verified_grant_id: v2::Id::new(verified_grant_id)
                            .map_err(|_| invalid_before_reservation())?,
                        user_id,
                        file_id,
                    },
                    v2::DirectSourceExpectationV1::AuthorizedLocalPath,
                )
            }
        };
        let partition =
            FiniteResponseBuffer::required_partition_bytes(chunk_bytes, outstanding_chunks)
                .ok_or_else(invalid_before_reservation)?;
        let Metric::Known(resources) = demand.resources else {
            return Err(invalid_before_reservation());
        };
        if demand.trial
            || resources.handles < 1
            || resources.peak_memory_bytes < minimum_memory
            || resources.cpu_ns_per_second != 0
            || resources.processes != 0
            || resources.hardware_slots != 0
            || resources.tuner_slots != 0
            || resources.scratch_bytes != 0
        {
            return Err(invalid_before_reservation());
        }
        // All variable input lengths and the demanded complete component fit
        // before these direct-plan strings are cloned or native source entry
        // can occur. Caller-owned input allocations predate this request.
        let path_string = path_text.to_owned();
        if path_string.capacity() > MAX_PATH_BYTES {
            return Err(invalid_before_reservation());
        }
        let retained_path = PathBuf::from(path_string.clone());
        if retained_path.capacity() > MAX_PATH_BYTES {
            return Err(invalid_before_reservation());
        }
        // The request's possibly over-capacity PathBuf is not retained by the
        // prepaid SourceCell; only this measured canonical copy is installed.
        drop(path);
        let actual_component = DirectSourceRequest::minimum_memory_bytes(
            path_string.capacity().max(retained_path.capacity()),
            media_file_id.capacity(),
            max_bodies,
            chunk_bytes,
            outstanding_chunks,
            application_owner_bytes,
        )
        .ok_or_else(invalid_before_reservation)?;
        if resources.peak_memory_bytes < actual_component {
            return Err(invalid_before_reservation());
        }
        let budget = ResponseBudget::new(
            usize::try_from(partition).map_err(|_| invalid_before_reservation())?,
        )
        .map_err(|_| invalid_before_reservation())?;
        let namespace =
            std::fs::metadata("/proc/self/ns/mnt").map_err(|_| invalid_before_reservation())?;
        use std::os::unix::fs::MetadataExt;
        let domain = v2::FilesystemDomain {
            platform: v2::FilesystemPlatform::Linux,
            domain_id: id()?,
            daemon_owner_id: v2::Id::new(self.owner.owner_id)
                .map_err(|_| invalid_before_reservation())?,
            os_boot: self.owner.os_boot.clone(),
            mount_namespace: v2::NativeValue::Known {
                value: namespace.ino(),
            },
        };
        let assignment_identity = provenance
            .binding()
            .map_err(|_| invalid_before_reservation())?;
        if assignment_identity.len() != 64 || assignment_identity.capacity() > 64 {
            return Err(invalid_before_reservation());
        }
        let member = id()?;
        let mut domains = self
            .direct_read_domain
            .lock()
            .map_err(|_| invalid_before_reservation())?;
        let existing = domains.as_ref().cloned();
        if existing.as_ref().is_some_and(|domain| {
            domain.state.lock().map_or(true, |state| {
                state.closing
                    || state.members.iter().all(Option::is_some)
                    || state
                        .members
                        .iter()
                        .any(|entry| *entry == Some(member.uuid()))
            })
        }) {
            return Err(invalid_before_reservation());
        }
        let snapshot = self
            .ledger
            .snapshot()
            .map_err(|_| invalid_before_reservation())?;
        if snapshot.limit_epoch != demand.limit_epoch {
            return Err(invalid_before_reservation());
        }
        let domain_cpu = match snapshot.capacity.cpu_ns_per_second {
            Metric::Known(value) if value > 0 => value.min(1_000_000_000),
            _ => return Err(invalid_before_reservation()),
        };
        let new_domain_ids = if existing.is_none() {
            Some((id()?, id()?))
        } else {
            None
        };
        let source = DirectSourcePlan {
            holder_id: id()?,
            media_file_id,
            recipe_source_binding: assignment_identity.clone(),
            provenance,
            absolute_path: path_string.clone(),
            scanner_size,
            scanner_mtime,
            expectation,
            filesystem_domain: domain,
            copy_id: id()?,
            acquisition_operation_id: id()?,
            read_domain: existing
                .as_ref()
                .map_or(DirectReadDomainRef::BatchRole(0), |domain| {
                    DirectReadDomainRef::Existing(domain.obligation_id)
                }),
        };
        let provenance_capacity = match &source.provenance {
            v2::DirectSourceProvenanceV1::Sharing {
                assignment_custody_identity,
            } => assignment_custody_identity.capacity(),
            v2::DirectSourceProvenanceV1::Local { .. }
            | v2::DirectSourceProvenanceV1::FileGrant { .. } => 0,
        };
        let boot_capacity = match &source.filesystem_domain.os_boot {
            BootIdentity::Known { provider, .. } => provider.capacity(),
            BootIdentity::Unknown { .. } => return Err(invalid_before_reservation()),
        };
        if source.media_file_id.capacity() > MAX_MEDIA_ID_BYTES
            || source.absolute_path.capacity() > MAX_PATH_BYTES
            || source.recipe_source_binding.capacity() > 64
            || provenance_capacity > 64
            || boot_capacity > MAX_BOOT_PROVIDER_BYTES
        {
            return Err(invalid_before_reservation());
        }
        let source_family = self.new_family().map_err(before_reservation_error)?;
        let source_roles = [FamilyRoleDemand {
            role: FamilyRole::Companion,
            demand,
            retained: None,
        }];
        let source_plan = TypedGraphPlan {
            attempt_id: None,
            members: vec![TypedMemberPlan::DirectSource {
                slot: v2::MemberSlot {
                    member_id: member,
                    owned_by: None,
                    depends_on: Vec::new(),
                    protected_paths: vec![path_string],
                },
                source,
            }],
        };
        let (preparation, role_index, domain) = if let Some(domain) = existing {
            let prepared = self.prepare_typed_families(
                &[FamilyDemand {
                    identity: source_family,
                    roles: &source_roles,
                }],
                vec![source_plan],
                deadline,
            );
            let preparation = match prepared {
                Ok(preparation) => preparation,
                Err(error) => {
                    let _ = self.remove_unused_family(source_family);
                    return Err(error.into());
                }
            };
            (preparation, 0, domain)
        } else {
            let (domain_member, domain_id) =
                new_domain_ids.ok_or_else(invalid_before_reservation)?;
            let domain_family = self.new_family().map_err(before_reservation_error)?;
            let domain_memory =
                DirectReadDomain::minimum_memory_bytes().ok_or_else(invalid_before_reservation)?;
            let domain_roles = [FamilyRoleDemand {
                role: FamilyRole::Companion,
                demand: WorkloadDemand {
                    resources: Metric::Known(ResourceVector {
                        cpu_ns_per_second: domain_cpu,
                        peak_memory_bytes: domain_memory,
                        ..ResourceVector::default()
                    }),
                    limit_epoch: demand.limit_epoch,
                    trial: false,
                },
                retained: None,
            }];
            let prepared = self.prepare_typed_families(
                &[
                    FamilyDemand {
                        identity: domain_family,
                        roles: &domain_roles,
                    },
                    FamilyDemand {
                        identity: source_family,
                        roles: &source_roles,
                    },
                ],
                vec![
                    TypedGraphPlan {
                        attempt_id: None,
                        members: vec![TypedMemberPlan::DirectReadDomain {
                            slot: v2::MemberSlot {
                                member_id: domain_member,
                                owned_by: None,
                                depends_on: Vec::new(),
                                protected_paths: Vec::new(),
                            },
                            domain_id,
                        }],
                    },
                    source_plan,
                ],
                deadline,
            );
            let preparation = match prepared {
                Ok(preparation) => preparation,
                Err(error) => {
                    let _ = self.remove_unused_family(domain_family);
                    let _ = self.remove_unused_family(source_family);
                    return Err(error.into());
                }
            };
            let obligation_id = self.prepared_typed_obligation_id(&preparation, 0)?;
            let domain = Arc::new(DirectReadDomain {
                preparation: preparation.clone(),
                obligation_id,
                member: domain_member.uuid(),
                domain_id: domain_id.uuid(),
                semaphore: Arc::new(Semaphore::new(1)),
                state: Mutex::new(DomainState {
                    members: [None; 64],
                    closing: false,
                    settled: false,
                    forgotten: false,
                }),
            });
            *domains = Some(Arc::clone(&domain));
            (preparation, 1, domain)
        };
        let mut domain_state = domain.state.lock().map_err(|_| invalid())?;
        if domain_state.closing
            || domain_state
                .members
                .iter()
                .any(|entry| *entry == Some(member.uuid()))
        {
            return Err(invalid());
        }
        let slot = domain_state
            .members
            .iter_mut()
            .find(|entry| entry.is_none())
            .ok_or_else(invalid)?;
        *slot = Some(member.uuid());
        drop(domain_state);
        self.install_finite_response_budget(&preparation, role_index, Arc::clone(&budget))?;
        let application = Arc::new(ApplicationLeaseState {
            state: Mutex::new(ApplicationLeaseStatus { held: true }),
            changed: Notify::new(),
        });
        let owner = DirectSourceOwner {
            runtime: Arc::clone(self),
            preparation,
            member: member.uuid(),
            role_index,
            domain,
            inner: Arc::new(DirectInner {
                state: Mutex::new(DirectState {
                    spec: Some(SourceOpenSpec {
                        path: retained_path,
                        scanner_size,
                        scanner_mtime,
                        expected_sha256: None,
                    }),
                    lease: None,
                    length: None,
                    object_version: None,
                    closing: false,
                    settled: false,
                    source_settled: false,
                    native_closed: false,
                    source_forgotten: false,
                    bodies: 0,
                    read_jobs: [const { None }; MAX_CHUNKS],
                }),
                changed: Notify::new(),
                open_gate: AsyncMutex::new(()),
                finish_gate: AsyncMutex::new(()),
                budget,
                max_bodies,
                chunk_bytes,
                application: Arc::clone(&application),
                #[cfg(any(test, feature = "fixtures"))]
                read_gate: Mutex::new(None),
            }),
        };
        Ok((owner, DirectSourceApplicationLease { state: application }))
    }
}

impl DirectSourceOwner {
    pub fn known_open_error_kind(&self) -> Option<std::io::ErrorKind> {
        self.runtime.direct_source_known_open_error_kind(
            &self.preparation,
            self.role_index,
            self.member,
        )
    }
    #[cfg(any(test, feature = "fixtures"))]
    pub fn install_read_gate(
        &self,
        gate: Arc<DirectSourceReadGate>,
    ) -> Result<(), DirectSourceError> {
        let state = self.inner.state.lock().map_err(|_| invalid())?;
        if state.closing || state.read_jobs.iter().any(Option::is_some) {
            return Err(invalid());
        }
        let mut selected = self.inner.read_gate.lock().map_err(|_| invalid())?;
        if selected.is_some() {
            return Err(invalid());
        }
        *selected = Some(gate);
        Ok(())
    }

    pub async fn open(&self, deadline: Instant) -> Result<(), DirectSourceError> {
        let _gate = self.inner.open_gate.lock().await;
        if self.inner.state.lock().map_err(|_| invalid())?.closing {
            return Err(invalid());
        }
        if self
            .inner
            .state
            .lock()
            .map_err(|_| invalid())?
            .lease
            .is_some()
        {
            return self.unchanged().map(|_| ());
        }
        self.runtime
            .resolve_preparation(&self.preparation, deadline)
            .await?;
        {
            let mut state = self.inner.state.lock().map_err(|_| invalid())?;
            if state.closing {
                return Err(invalid());
            }
            if state.spec.is_some() {
                self.runtime.queue_source_open(
                    &self.preparation,
                    self.role_index,
                    self.member,
                    &mut state.spec,
                )?;
            }
        }
        let (lease, length, object_version) = self
            .runtime
            .open_prepared_direct_source(&self.preparation, self.role_index, self.member, deadline)
            .await?;
        let mut state = self.inner.state.lock().map_err(|_| invalid())?;
        state.lease = Some(lease);
        state.length = Some(length);
        if object_version.len() > MAX_S1_BYTES || object_version.capacity() > MAX_S1_BYTES {
            return Err(invalid());
        }
        state.object_version = Some(object_version);
        if state.closing {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn length(&self) -> Result<u64, DirectSourceError> {
        let state = self.inner.state.lock().map_err(|_| invalid())?;
        if state.closing {
            return Err(invalid());
        }
        state.length.ok_or_else(invalid)
    }

    pub fn object_version(&self) -> Result<String, DirectSourceError> {
        let state = self.inner.state.lock().map_err(|_| invalid())?;
        if state.closing {
            return Err(invalid());
        }
        state.object_version.clone().ok_or_else(invalid)
    }

    pub fn unchanged(&self) -> Result<(), DirectSourceError> {
        let (file, length, identity) = {
            let state = self.inner.state.lock().map_err(|_| invalid())?;
            if state.closing {
                return Err(invalid());
            }
            (
                state.lease.as_ref().ok_or_else(invalid)?.shared_file(),
                state.length.ok_or_else(invalid)?,
                state.object_version.clone().ok_or_else(invalid)?,
            )
        };
        let metadata = file
            .metadata()
            .map_err(|error| DirectSourceError(OperationError::Physical(error)))?;
        if !metadata.is_file()
            || metadata.len() != length
            || identity != SourceIdentity::from_metadata(&metadata).render_s1()
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn begin_body(&self) -> Result<DirectSourceBody, DirectSourceError> {
        let mut state = self.inner.state.lock().map_err(|_| invalid())?;
        if state.closing || state.lease.is_none() || state.bodies >= self.inner.max_bodies {
            return Err(invalid());
        }
        state.bodies += 1;
        Ok(DirectSourceBody {
            inner: Arc::clone(&self.inner),
        })
    }

    pub async fn read_at(
        &self,
        body: &DirectSourceBody,
        offset: u64,
        len: usize,
        permit: OwnedSemaphorePermit,
        deadline: Instant,
    ) -> Result<FiniteResponseBuffer, DirectSourceError> {
        if !Arc::ptr_eq(&self.inner, &body.inner)
            || len == 0
            || len > self.inner.chunk_bytes
            || Instant::now() >= deadline
        {
            return Err(invalid());
        }
        if self.domain.state.lock().map_err(|_| invalid())?.closing {
            return Err(invalid());
        }
        let domain_permit = tokio::time::timeout_at(
            deadline.into(),
            Arc::clone(&self.domain.semaphore).acquire_owned(),
        )
        .await
        .map_err(|_| DirectSourceError(OperationError::Deadline))?
        .map_err(|_| invalid())?;
        // Charge every escaping chunk at the configured maximum, even when a
        // range tail asks for one byte. This bounds live chunk owners by the
        // admitted slot count rather than by spare payload bytes.
        let slot = self.inner.budget.reserve(self.inner.chunk_bytes)?;
        let buffer = FiniteResponseBuffer::allocate(slot, self.inner.chunk_bytes)?;
        let (file, expected_s1) = {
            let state = self.inner.state.lock().map_err(|_| invalid())?;
            let length = state.length.ok_or_else(invalid)?;
            if state.closing
                || state.lease.is_none()
                || offset >= length
                || offset
                    .checked_add(len as u64)
                    .is_none_or(|end| end > length)
            {
                return Err(invalid());
            }
            (
                state.lease.as_ref().ok_or_else(invalid)?.shared_file(),
                state.object_version.clone().ok_or_else(invalid)?,
            )
        };
        // The worker is blocked on this latch until its original JoinHandle
        // is retained in the exact owner. Cancellation of this waiter cannot
        // detach a native read or discard a charged result.
        let (enter, registered) = oneshot::channel::<()>();
        #[cfg(any(test, feature = "fixtures"))]
        let read_gate = self.inner.read_gate.lock().map_err(|_| invalid())?.clone();
        let task = tokio::task::spawn_blocking(move || {
            // The daemon's shared native-read limit stays held until this
            // ORIGINAL registered worker exits, even if its HTTP waiter is
            // cancelled or times out.
            let _permit = permit;
            let _domain_permit = domain_permit;
            if registered.blocking_recv().is_err() {
                return Err(invalid());
            }
            #[cfg(any(test, feature = "fixtures"))]
            if let Some(gate) = read_gate.as_ref() {
                gate.park_real_job();
            }
            let result = (|| {
                let mut buffer = buffer;
                let before = file
                    .metadata()
                    .map_err(|error| DirectSourceError(OperationError::Physical(error)))?;
                if SourceIdentity::from_metadata(&before).render_s1() != expected_s1 {
                    return Err(invalid());
                }
                let read = buffer
                    .read_at(&file, offset, len)
                    .map_err(|error| DirectSourceError(OperationError::Physical(error)))?;
                if read == 0 {
                    return Err(invalid());
                }
                let after = file
                    .metadata()
                    .map_err(|error| DirectSourceError(OperationError::Physical(error)))?;
                if SourceIdentity::from_metadata(&after).render_s1() != expected_s1 {
                    return Err(invalid());
                }
                Ok(buffer)
            })();
            #[cfg(any(test, feature = "fixtures"))]
            if let Some(gate) = read_gate.as_ref() {
                gate.complete.store(true, Ordering::Release);
            }
            result
        });
        let job_id = Uuid::new_v4();
        let job = Arc::new(ReadJob {
            state: AsyncMutex::new(ReadJobState {
                task: Some(task),
                completed: None,
                unknown: false,
            }),
        });
        {
            let mut state = self.inner.state.lock().map_err(|_| invalid())?;
            if state.closing
                || state
                    .read_jobs
                    .iter()
                    .any(|entry| entry.as_ref().is_some_and(|(id, _)| *id == job_id))
            {
                return Err(invalid());
            }
            let slot = state
                .read_jobs
                .iter_mut()
                .find(|entry| entry.is_none())
                .ok_or_else(invalid)?;
            *slot = Some((job_id, Arc::clone(&job)));
        }
        let _ = enter.send(());
        tokio::time::timeout_at(deadline.into(), job.join())
            .await
            .map_err(|_| DirectSourceError(OperationError::Deadline))??;
        let mut state = self.inner.state.lock().map_err(|_| invalid())?;
        let slot = state
            .read_jobs
            .iter_mut()
            .find(|entry| entry.as_ref().is_some_and(|(id, _)| *id == job_id))
            .ok_or_else(invalid)?;
        if slot.take().is_none() {
            return Err(invalid());
        }
        drop(state);
        job.take_completed()
    }

    pub async fn prepare_retirement(
        &self,
        deadline: Instant,
    ) -> Result<DirectSourceRetirementTicket, DirectSourceError> {
        {
            let mut state = self.inner.state.lock().map_err(|_| invalid())?;
            state.closing = true;
        }
        let _gate = self.inner.open_gate.lock().await;
        if !self
            .inner
            .state
            .lock()
            .map_err(|_| invalid())?
            .native_closed
        {
            if self
                .inner
                .state
                .lock()
                .map_err(|_| invalid())?
                .spec
                .is_some()
            {
                self.runtime
                    .resolve_preparation(&self.preparation, deadline)
                    .await?;
                let mut state = self.inner.state.lock().map_err(|_| invalid())?;
                if state.spec.is_some() {
                    self.runtime.queue_source_open(
                        &self.preparation,
                        self.role_index,
                        self.member,
                        &mut state.spec,
                    )?;
                }
            }
            loop {
                let notified = self.inner.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let bodies_drained = {
                    let state = self.inner.state.lock().map_err(|_| invalid())?;
                    state.bodies == 0
                };
                if bodies_drained {
                    break;
                }
                tokio::time::timeout_at(deadline.into(), notified)
                    .await
                    .map_err(|_| DirectSourceError(OperationError::Deadline))?;
            }
            // A timed-out or cancelled waiter left its original native task and
            // result here. Join each exact task, then discard unclaimed buffers
            // while the full source charge is still held.
            let jobs: Vec<_> = self
                .inner
                .state
                .lock()
                .map_err(|_| invalid())?
                .read_jobs
                .iter()
                .flatten()
                .map(|(id, job)| (*id, Arc::clone(job)))
                .collect();
            for (id, job) in jobs {
                tokio::time::timeout_at(deadline.into(), job.join())
                    .await
                    .map_err(|_| DirectSourceError(OperationError::Deadline))??;
                let result = job.take_completed();
                if let Ok(buffer) = result {
                    drop(buffer);
                }
                self.inner
                    .state
                    .lock()
                    .map_err(|_| invalid())?
                    .read_jobs
                    .iter_mut()
                    .find(|entry| entry.as_ref().is_some_and(|(job_id, _)| *job_id == id))
                    .ok_or_else(invalid)?
                    .take();
            }
            self.inner.budget.wait_drained(deadline).await?;
            self.inner.budget.begin_close_if_empty()?;
            let lease = self.inner.state.lock().map_err(|_| invalid())?.lease.take();
            drop(lease);
            self.runtime
                .close_direct_source_native(&self.preparation, self.role_index, self.member)
                .await?;
            let mut state = self.inner.state.lock().map_err(|_| invalid())?;
            state.spec = None;
            state.length = None;
            state.object_version = None;
            state.native_closed = true;
            drop(state);
            #[cfg(any(test, feature = "fixtures"))]
            if let Some(gate) = self.inner.read_gate.lock().map_err(|_| invalid())?.as_ref() {
                // This is later than actual original-FD close, not an authority
                // for the later typed settlement.
                gate.file_closed.store(true, Ordering::Release);
            }
        }
        Ok(DirectSourceRetirementTicket {
            runtime: Arc::clone(&self.runtime),
            preparation: self.preparation.clone(),
            member: self.member,
            role_index: self.role_index,
            domain: Arc::clone(&self.domain),
            inner: Arc::clone(&self.inner),
        })
    }
}

impl DirectSourceRetirementTicket {
    /// The daemon calls this only after the original worker is joined and its
    /// rich entry has been destroyed. On timeout or unknown commit the SAME
    /// ticket remains usable; it never creates a successor operation ID.
    pub async fn finish_retirement(&mut self, deadline: Instant) -> Result<(), DirectSourceError> {
        let _finish = self.inner.finish_gate.lock().await;
        if self.inner.state.lock().map_err(|_| invalid())?.settled {
            return Ok(());
        }
        if !self
            .inner
            .state
            .lock()
            .map_err(|_| invalid())?
            .native_closed
        {
            return Err(invalid());
        }
        loop {
            let notified = self.inner.application.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self
                .inner
                .application
                .state
                .lock()
                .map_err(|_| invalid())?
                .held
            {
                break;
            }
            tokio::time::timeout_at(deadline.into(), notified)
                .await
                .map_err(|_| DirectSourceError(OperationError::Deadline))?;
        }
        if !self
            .inner
            .state
            .lock()
            .map_err(|_| invalid())?
            .source_settled
        {
            self.runtime
                .settle_direct_source(&self.preparation, self.role_index, self.member, deadline)
                .await?;
            self.inner
                .state
                .lock()
                .map_err(|_| invalid())?
                .source_settled = true;
        }
        let last = {
            let registry = self
                .runtime
                .direct_read_domain
                .lock()
                .map_err(|_| invalid())?;
            let current = registry.as_ref().ok_or_else(invalid)?;
            if !Arc::ptr_eq(current, &self.domain) {
                return Err(invalid());
            }
            let mut state = self.domain.state.lock().map_err(|_| invalid())?;
            if let Some(slot) = state
                .members
                .iter_mut()
                .find(|entry| **entry == Some(self.member))
            {
                *slot = None;
            } else if !state.closing {
                return Err(invalid());
            }
            if state.members.iter().all(Option::is_none) {
                state.closing = true;
                self.domain.semaphore.close();
                true
            } else {
                false
            }
        };
        if last && !self.domain.state.lock().map_err(|_| invalid())?.settled {
            self.runtime
                .settle_direct_read_domain(
                    &self.domain.preparation,
                    self.domain.member,
                    self.domain.domain_id,
                    DirectReadDomainReady {
                        domain: Arc::clone(&self.domain),
                    },
                    deadline,
                )
                .await?;
            self.domain.state.lock().map_err(|_| invalid())?.settled = true;
        }
        if self.role_index == 0
            && !self
                .inner
                .state
                .lock()
                .map_err(|_| invalid())?
                .source_forgotten
        {
            self.runtime
                .forget_preparation(self.preparation.clone())
                .await?;
            self.inner
                .state
                .lock()
                .map_err(|_| invalid())?
                .source_forgotten = true;
        }
        if last && !self.domain.state.lock().map_err(|_| invalid())?.forgotten {
            self.runtime
                .forget_preparation(self.domain.preparation.clone())
                .await?;
            self.domain.state.lock().map_err(|_| invalid())?.forgotten = true;
            let mut registry = self
                .runtime
                .direct_read_domain
                .lock()
                .map_err(|_| invalid())?;
            if registry
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &self.domain))
            {
                *registry = None;
            }
        }
        self.inner.state.lock().map_err(|_| invalid())?.settled = true;
        Ok(())
    }
}
