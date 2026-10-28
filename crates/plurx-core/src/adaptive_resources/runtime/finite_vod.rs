//! Concrete finite software-VOD preparation interface for the daemon.
//! Request data selects plans, never native facts, callbacks or refund proofs.
mod command;
mod executable;
mod initial_unit;
pub(in crate::adaptive_resources) mod pump;
pub use super::operations::bootstrap::readers::{FiniteReadChunk, FiniteReader};
pub use command::{FiniteEngineSnapshot, SoftwareAvcCommand};
pub use executable::SoftwareAvcExecutable;
pub use pump::{
    FiniteWriterCompletion, FiniteWriterFailureKind, FiniteWriterOutcome, SoftwareAvcPumpPlan,
    SoftwareVodEvent,
};

/// Joined producer metadata only. Original native owners and the reservation
/// remain in the runtime; this value cannot authorize transfer or refund.
#[derive(Clone, Copy, Debug)]
pub struct FiniteProducerCompletion {
    pub writer: Option<FiniteWriterCompletion>,
    pub root_status: Option<std::process::ExitStatus>,
    pub parent_sources_closed: usize,
}

use super::operations::plans::{SourceCopyOwner, SourceCopyPlan, SourcePlan};
use super::operations::source_files::SourceOpenSpec;
use super::operations::{OperationError, OwnedPreparation, TypedGraphPlan, TypedMemberPlan};
use super::*;
use crate::adaptive_resources::{FamilyDemand, FamilyRole, FamilyRoleDemand, WorkloadDemand};
use crate::resource_custody::codec_v2 as v2;
use crate::resource_custody::{Adapter, MemberKind, PlannedMember, RecoveryMethod};
#[cfg(target_os = "linux")]
use crate::resource_history::{CostPhase, CostSummary, HistoryKey};
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use v2::scratch::bootstrap::*;
use v2::scratch::{
    AllocationAccountingV1, OutputKey, OutputNamesV1, OutputNamingV1, ScratchLimitsV1,
};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct FiniteVodError(OperationError);
impl FiniteVodError {
    pub fn admission_refusal(&self) -> Option<crate::adaptive_resources::AdmissionRefusal> {
        match &self.0 {
            OperationError::Admission(refusal)
            | OperationError::BeforeReservation(
                super::operations::PreReservationRefusal::Admission(refusal),
            ) => Some(*refusal),
            _ => None,
        }
    }
    /// Exact preparation provenance only. This is not a refund, native Close,
    /// or permission to replay the request. Generic Admission and Draining
    /// remain uncertain because they may arise after a reservation exists.
    pub fn definitely_before_reservation(&self) -> bool {
        matches!(self.0, OperationError::BeforeReservation(_))
    }

    /// Preserve a native failure when adapting counted reads to an I/O stream.
    pub fn into_io_error(self) -> std::io::Error {
        match self.0 {
            OperationError::Physical(error) => error,
            OperationError::Deadline => {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "finite VOD deadline elapsed")
            }
            error => std::io::Error::other(error),
        }
    }
}
impl From<OperationError> for FiniteVodError {
    fn from(error: OperationError) -> Self {
        Self(error)
    }
}
impl From<std::io::Error> for FiniteVodError {
    fn from(error: std::io::Error) -> Self {
        Self(OperationError::Physical(error))
    }
}
impl From<CustodyError> for FiniteVodError {
    fn from(error: CustodyError) -> Self {
        Self(OperationError::Custody(error))
    }
}

/// Immutable scanner/recipe association available before native acquisition.
pub struct FiniteSourceRequest {
    pub path: PathBuf,
    pub media_file_id: String,
    pub object_version: String,
    pub scanner_size: i64,
    pub scanner_mtime: i64,
}

pub use super::finite_lifetime::FiniteApplicationLease;
use super::response_memory::ResponseBudget;
pub use super::response_memory::{
    FiniteClientSlot, FiniteClientStateLease, FiniteMetadataLease, FiniteResponseBuffer,
    FiniteResponseWorkspace,
};

/// Explicit logical/reported-allocation profile. These are admission bounds,
/// not a claim that a filesystem or host enforces a physical-byte quota.
pub struct FiniteOutputLimits {
    /// Fixed same-role partition for buffered response payloads and owners.
    /// Separate from the V3 scratch/native reader resident-memory bound.
    pub response_memory_bytes: u64,
    /// Selected daemon rendition metadata retained for the entire R/S lifetime.
    /// Counts requested nested heap capacities; excludes allocator overhead/RSS.
    pub retained_metadata_bytes: u64,
    /// Enforced in-role mutable reader, marker and demand-state partition.
    pub client_state_bytes: u64,
    pub resident_file_objects: u32,
    pub resident_logical_bytes: u64,
    pub reserved_storage_bytes: u64,
    pub one_file_logical_bytes: u64,
    pub one_io_payload_bytes: u64,
    pub live_handles: u32,
    pub reader_leases: u32,
    pub resident_memory_bytes: u64,
    pub allocation_unit_bytes: u64,
    pub per_object_reserve_bytes: u64,
    pub child_directory_reserve_bytes: u64,
    pub parent_entry_reserve_bytes: u64,
    pub bootstrap_memory_bytes: u64,
}

pub struct SoftwareVodRequest {
    pub rendition_id: String,
    /// Nonzero typed asset generation, fixed before admission.
    pub generation_epoch: u64,
    pub recipe_source_binding: String,
    pub command: SoftwareAvcCommand,
    pub producer: SoftwareAvcPumpPlan,
    pub sources: Vec<FiniteSourceRequest>,
    pub segment_count: u32,
    pub encoded_process_marker: bool,
    pub demand: WorkloadDemand,
    /// One rich daemon worker/attempt entry and its original future. This is
    /// a separate lifetime slot from Rendition metadata and response bodies.
    pub application_owner_bytes: u64,
    pub output: FiniteOutputLimits,
}

impl SoftwareVodRequest {
    /// First typed asset generation of a newly admitted immutable owner.
    /// The legacy scheduler's zero-based epoch is a separate counter; this
    /// value never resets or rewrites an existing admitted asset.
    pub const INITIAL_GENERATION_EPOCH: u64 = 1;

    /// Requested capacity of the request's present planning graph. This is a
    /// heap projection, not native custody, admission, or a bound on future
    /// parser/output allocations.
    pub fn requested_heap_bytes(&self) -> Option<usize> {
        let mut bytes = std::mem::size_of::<Self>()
            .checked_add(self.rendition_id.capacity())?
            .checked_add(self.recipe_source_binding.capacity())?
            .checked_add(self.command.requested_heap_bytes()?)?
            .checked_add(self.producer.requested_heap_bytes()?)?
            .checked_add(
                self.sources
                    .capacity()
                    .checked_mul(std::mem::size_of::<FiniteSourceRequest>())?,
            )?;
        for source in &self.sources {
            bytes = bytes
                .checked_add(source.path.capacity())?
                .checked_add(source.media_file_id.capacity())?
                .checked_add(source.object_version.capacity())?;
        }
        Some(bytes)
    }

    /// Add the caller's concrete rich worker/future slot before reservation.
    /// A later caller-supplied byte count never authorizes native work or
    /// weakens the original hard vector; actual allocation must take its one
    /// matching runtime lease after prepare and fit within this amount.
    pub fn charge_application_owner_bytes(&mut self, bytes: usize) -> Result<(), FiniteVodError> {
        let bytes = u64::try_from(bytes).map_err(|_| invalid())?;
        if bytes == 0 || self.application_owner_bytes != 0 {
            return Err(invalid());
        }
        let Metric::Known(resources) = &mut self.demand.resources else {
            return Err(invalid());
        };
        let increased = resources
            .peak_memory_bytes
            .checked_add(bytes)
            .ok_or_else(invalid)?;
        resources.peak_memory_bytes = increased;
        self.application_owner_bytes = bytes;
        Ok(())
    }
    /// Known requested allocations only. The additional producer peak is
    /// explicitly unknown and serialized by the runtime's memory claim.
    pub fn known_memory_component(
        command: &SoftwareAvcCommand,
        producer: &SoftwareAvcPumpPlan,
        output: &FiniteOutputLimits,
        application_owner_bytes: u64,
    ) -> std::io::Result<u64> {
        let retained_structure =
            super::operations::finite_retirement::finite_retained_structural_bytes(
                output.reader_leases,
            )
            .ok_or_else(|| std::io::Error::other("finite retained layout overflow"))?;
        producer
            .minimum_memory_bytes()?
            .checked_add(
                u64::try_from(command.resident_memory_bytes()?)
                    .map_err(|_| std::io::Error::other("finite memory overflow"))?,
            )
            .and_then(|bytes| bytes.checked_add(64 * 1024))
            .and_then(|bytes| bytes.checked_add(output.resident_memory_bytes))
            .and_then(|bytes| bytes.checked_add(output.response_memory_bytes))
            .and_then(|bytes| bytes.checked_add(output.bootstrap_memory_bytes))
            .and_then(|bytes| bytes.checked_add(output.retained_metadata_bytes))
            .and_then(|bytes| bytes.checked_add(application_owner_bytes))
            .and_then(|bytes| bytes.checked_add(output.client_state_bytes))
            .and_then(|bytes| bytes.checked_add(retained_structure))
            .and_then(|bytes| bytes.checked_add(64 * 1024))
            .ok_or_else(|| std::io::Error::other("finite memory overflow"))
    }
}

/// Nonserde ownership selection. The runtime book retains all actual resources
/// if a request drops this value or cancels any borrowed setup wait.
pub(in crate::adaptive_resources::runtime) struct FiniteControl {
    requested: std::sync::atomic::AtomicBool,
    suspended: std::sync::atomic::AtomicBool,
    root: std::sync::Mutex<Option<crate::process::priority::RegisteredChildControl>>,
    task: std::sync::Mutex<Option<tokio::task::AbortHandle>>,
}
impl FiniteControl {
    fn new() -> Self {
        Self {
            requested: std::sync::atomic::AtomicBool::new(false),
            suspended: std::sync::atomic::AtomicBool::new(false),
            root: std::sync::Mutex::new(None),
            task: std::sync::Mutex::new(None),
        }
    }
    pub(in crate::adaptive_resources::runtime) fn requested(&self) -> bool {
        self.requested.load(std::sync::atomic::Ordering::Acquire)
    }
    pub(in crate::adaptive_resources::runtime) fn install(
        &self,
        control: crate::process::priority::RegisteredChildControl,
    ) -> std::io::Result<()> {
        let mut root = self
            .root
            .lock()
            .map_err(|_| std::io::Error::other("finite root control unavailable"))?;
        if root.is_some_and(|previous| previous != control) {
            return Err(std::io::Error::other(
                "finite root control cannot change identity",
            ));
        }
        *root = Some(control);
        if self.requested() {
            control.start_kill()?;
        } else if self.suspended.load(std::sync::atomic::Ordering::Acquire) {
            control.signal(crate::process::ProcessSignal::Suspend)?;
        }
        Ok(())
    }
    pub(in crate::adaptive_resources::runtime) fn install_task(
        &self,
        abort: tokio::task::AbortHandle,
    ) -> std::io::Result<()> {
        let mut task = self
            .task
            .lock()
            .map_err(|_| std::io::Error::other("finite task control unavailable"))?;
        if task.is_some() {
            return Err(std::io::Error::other(
                "finite task control already installed",
            ));
        }
        if self.requested() {
            abort.abort();
        }
        *task = Some(abort);
        Ok(())
    }
    /// The original producer task has positively joined. Its abort selection
    /// is no longer needed to signal native work and must not persist in S.
    pub(in crate::adaptive_resources::runtime) fn release_joined_task(
        &self,
    ) -> std::io::Result<()> {
        self.task
            .lock()
            .map_err(|_| std::io::Error::other("finite task control unavailable"))?
            .take();
        Ok(())
    }
    fn suspend(&self, suspended: bool) -> std::io::Result<()> {
        // Serialize state and the original registry control, including late
        // installation. This never addresses a recycled numeric PID directly.
        let root = self
            .root
            .lock()
            .map_err(|_| std::io::Error::other("finite root control unavailable"))?;
        self.suspended
            .store(suspended, std::sync::atomic::Ordering::Release);
        if let Some(root) = *root {
            root.signal(if suspended {
                crate::process::ProcessSignal::Suspend
            } else {
                crate::process::ProcessSignal::Resume
            })?;
        }
        Ok(())
    }
    fn request(&self) -> std::io::Result<()> {
        self.requested
            .store(true, std::sync::atomic::Ordering::Release);
        // Attempt both original controls even when signalling one fails. This
        // path never waits on the graph mutex held by a concurrent join.
        let task_result = self
            .task
            .lock()
            .map(|task| {
                if let Some(task) = task.as_ref() {
                    task.abort();
                }
            })
            .map_err(|_| std::io::Error::other("finite task control unavailable"));
        let root_result = self
            .root
            .lock()
            .map_err(|_| std::io::Error::other("finite root control unavailable"))
            .and_then(|root| root.as_ref().map_or(Ok(()), |root| root.start_kill()));
        task_result.and(root_result)
    }
}

/// Control for the same registered producer while its owner is joining or
/// waiting for durable I/O. Signalling conveys no close or refund proof.
#[derive(Clone)]
pub struct SoftwareVodControl {
    control: Arc<FiniteControl>,
    // Last: the shared final-drop guard observes the control Arc gone.
    _app_guard: Arc<super::finite_lifetime::FiniteAppGuard>,
}
impl SoftwareVodControl {
    pub fn request_stop(&self) -> std::io::Result<()> {
        self.control.request()
    }
    pub fn suspend(&self) -> std::io::Result<()> {
        self.control.suspend(true)
    }
    pub fn resume(&self) -> std::io::Result<()> {
        self.control.suspend(false)
    }
}

pub struct SoftwareVodOwner {
    runtime: Arc<NodeResourceRuntime>,
    preparation: OwnedPreparation,
    source_members: Vec<Uuid>,
    source_specs: Vec<Option<SourceOpenSpec>>,
    media_source_count: usize,
    scratch_member: Uuid,
    directory_path: Arc<PathBuf>,
    encoded_process_marker: bool,
    events: Option<tokio::sync::mpsc::Receiver<SoftwareVodEvent>>,
    control: Arc<FiniteControl>,
    #[cfg(target_os = "linux")]
    history_basis: Option<pump::HistoryBasis>,
    #[cfg(target_os = "linux")]
    history_family: crate::adaptive_resources::FamilyIdentity,
    #[cfg(target_os = "linux")]
    history_units: u32,
    // Last: drop every rich field before announcing application release.
    app_guard: Arc<super::finite_lifetime::FiniteAppGuard>,
}

/// Exact recipe/mix prior for operator and placement diagnostics only. Neither
/// phase supplies a hard resource, startup, cleanup or trial bound.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug)]
pub struct FiniteHistoryHint {
    pub startup: Option<CostSummary>,
    pub steady: Option<CostSummary>,
    pub startup_recent: bool,
    pub steady_recent: bool,
    pub durable_units: u32,
}

/// A bounded selection of the same custodian, not an exported File/Arc<File>.
/// Cloning this selection does not duplicate a directory or admission charge.
#[derive(Clone)]
pub struct FiniteOutput {
    runtime: Arc<NodeResourceRuntime>,
    preparation: OwnedPreparation,
    member: Uuid,
    directory_path: Arc<PathBuf>,
    encoded_process_marker: bool,
    // Last: response selectors hold the same application lifetime as owner
    // and control aliases, including when a ReadyFile outlives its rendition.
    _app_guard: Arc<super::finite_lifetime::FiniteAppGuard>,
}

/// Original finite close selection. The runtime book keeps the native proof,
/// durable operation and complete charge if this selector or waiter drops.
/// It is deliberately separate from an output or producer application alias.
pub struct FiniteCloseTicket {
    runtime: Arc<NodeResourceRuntime>,
    preparation: Option<OwnedPreparation>,
    app_state: Option<Arc<super::finite_lifetime::FiniteAppState>>,
}

impl FiniteCloseTicket {
    /// Drain actual application aliases outside any daemon key gate. This is
    /// only a heap fence; native and durable close still belong to the ticket.
    pub async fn wait_application_release(
        &mut self,
        deadline: Instant,
    ) -> Result<(), FiniteVodError> {
        if self.preparation.is_none() {
            return Ok(());
        }
        if let Some(state) = self.app_state.as_ref() {
            if !state.wait_released(deadline).await {
                return Err(FiniteVodError(OperationError::Deadline));
            }
            // The core role retains this state until its rich bundle is
            // destroyed; the compact selector retains no alias payload.
            self.app_state = None;
        }
        Ok(())
    }

    /// Retry the original proof-bearing operation only after every application
    /// owner really dropped. The exact same ticket survives unknown results.
    pub async fn finish_retirement(&mut self, deadline: Instant) -> Result<(), FiniteVodError> {
        self.wait_application_release(deadline).await?;
        self.runtime
            .finish_finite_close(&mut self.preparation, 0, deadline)
            .await
            .map_err(Into::into)
    }
}

/// Opaque identity of a payload already retained by the runtime. Dropping the
/// ticket cannot cancel native work, lose the original result or refund credit.
pub struct PublicationTicket {
    id: Uuid,
    kind: TicketKind,
}
enum TicketKind {
    Publication,
    Sync,
}

/// Fixed names selected from the core's actual observed inventory.
#[derive(Clone, Copy)]
pub enum FiniteFile {
    Init,
    Segment(u32),
    IdentityMetadata,
    EncodedProcessMetadata,
    CompleteArtifactManifest,
}

impl FiniteFile {
    fn key(self) -> OutputKey {
        match self {
            Self::Init => OutputKey::Init,
            Self::Segment(index) => OutputKey::Segment { index },
            Self::IdentityMetadata => OutputKey::IdentityMetadata,
            Self::EncodedProcessMetadata => OutputKey::EncodedProcessMetadata,
            Self::CompleteArtifactManifest => OutputKey::CompleteArtifactManifest,
        }
    }
}

fn software_owner_heap_bytes(
    source_member_capacity: usize,
    source_spec_capacity: usize,
    source_path_capacities: impl IntoIterator<Item = usize>,
) -> Option<usize> {
    let mut bytes = source_member_capacity
        .checked_mul(std::mem::size_of::<Uuid>())?
        .checked_add(
            source_spec_capacity.checked_mul(std::mem::size_of::<Option<SourceOpenSpec>>())?,
        )?
        .checked_add(std::mem::size_of::<FiniteControl>())?
        .checked_add(2 * std::mem::size_of::<usize>())?;
    for capacity in source_path_capacities {
        bytes = bytes.checked_add(capacity)?;
    }
    Some(bytes)
}

pub(super) fn finite_directory_path(
    root: &str,
    rendition_id: &str,
) -> Result<(String, PathBuf), FiniteVodError> {
    if root.len() > 4096
        || !root.starts_with('/')
        || root == "/"
        || rendition_id.is_empty()
        || rendition_id.len() > 255
        || rendition_id.contains(['/', '\\', '\0'])
        || matches!(rendition_id, "." | "..")
    {
        return Err(invalid());
    }
    let protected_child = format!("{root}/{rendition_id}");
    let path = PathBuf::from(&protected_child);
    Ok((protected_child, path))
}

impl FiniteOutput {
    /// Equality check for the original planned output in a prepared finite
    /// graph. It does not assert native acquisition or publication.
    pub fn output_digest(&self) -> Result<[u8; 32], FiniteVodError> {
        self.runtime
            .finite_worker_binding_digests(&self.preparation, 0, self.member)
            .map(|(_, output)| output)
            .map_err(Into::into)
    }
    /// Requested heap unique to the shared directory selector. Additional
    /// FiniteOutput clones share this allocation; their inline frames are
    /// counted by their callers. The path is immutable for its whole lifetime.
    pub fn requested_heap_bytes(&self) -> Option<usize> {
        std::mem::size_of::<PathBuf>()
            .checked_add(2 * std::mem::size_of::<usize>())?
            .checked_add(self.directory_path.capacity())
    }
    /// Recheck the same originally held source files around publication. A
    /// latched drift prevents later freezing/retirement of mixed-source bytes.
    async fn validate_sources(&self) -> Result<(), FiniteVodError> {
        self.runtime
            .validate_finite_sources(&self.preparation, 0)
            .await
            .map_err(Into::into)
    }
    /// Reserve the complete buffered response under the same retained role
    /// before allocating or opening the native reader.
    pub async fn allocate_response_buffer(
        &self,
        capacity: usize,
        deadline: Instant,
    ) -> Result<FiniteResponseBuffer, FiniteVodError> {
        self.runtime
            .allocate_finite_response_buffer(&self.preparation, 0, self.member, capacity, deadline)
            .await
            .map_err(Into::into)
    }

    /// Reserve the selected daemon rendition's separate metadata partition.
    /// Final native settlement stays charged until this sole lease is dropped.
    pub async fn reserve_metadata_partitions(
        &self,
        immutable_required: usize,
        client_limit: usize,
    ) -> Result<(FiniteMetadataLease, FiniteClientStateLease), FiniteVodError> {
        self.runtime
            .reserve_finite_metadata_partitions(
                &self.preparation,
                0,
                immutable_required,
                client_limit,
            )
            .await
            .map_err(Into::into)
    }

    /// Wait for actual daemon metadata and buffered bodies to leave their
    /// in-role partitions. This event grants no native-close/refund authority.
    pub async fn wait_allocations_release(&self, deadline: Instant) -> Result<(), FiniteVodError> {
        self.runtime
            .wait_finite_allocations_release(&self.preparation, 0, deadline)
            .await
            .map_err(Into::into)
    }

    /// Read immutable published metadata without opening a native file.
    pub async fn file_len(&self, file: FiniteFile) -> Result<Option<u64>, FiniteVodError> {
        self.runtime
            .finite_file_len(&self.preparation, 0, self.member, &file.key())
            .await
            .map_err(Into::into)
    }
    /// Select the exact response range before opening its counted File. Drop
    /// leaves a pending close request in the same retained runtime entry.
    pub async fn prepare_reader(
        &self,
        file: FiniteFile,
        start: u64,
        end_exclusive: u64,
        memory_cap: u64,
        deadline: Instant,
    ) -> Result<FiniteReader, FiniteVodError> {
        self.runtime
            .close_dropped_finite_readers(&self.preparation, 0, deadline)
            .await?;
        self.runtime
            .prepare_finite_reader(
                &self.preparation,
                0,
                self.member,
                file.key(),
                start,
                end_exclusive,
                memory_cap,
            )
            .await
            .map_err(Into::into)
    }

    /// Evict the exact retained output through original native cleanup and a
    /// known durable settlement. Active bodies must finish before this closes.
    /// Cancellation retains the same native jobs and settlement operation.
    pub async fn prepare_eviction(
        &self,
        deadline: Instant,
    ) -> Result<FiniteCloseTicket, FiniteVodError> {
        self.runtime
            .prepare_retained_finite_output_close(&self.preparation, 0, self.member, deadline)
            .await?;
        let app_state = self.runtime.finite_app_state(&self.preparation, 0)?;
        Ok(FiniteCloseTicket {
            runtime: Arc::clone(&self.runtime),
            preparation: Some(self.preparation.clone()),
            app_state: Some(app_state),
        })
    }

    pub fn has_encoded_process_metadata(&self) -> bool {
        self.encoded_process_marker
    }
    /// Configured location for application metadata; never native authority.
    pub fn path(&self) -> &Path {
        &self.directory_path
    }
    pub fn queue_init(&self, bytes: &mut Vec<u8>) -> Result<PublicationTicket, FiniteVodError> {
        self.queue(OutputKey::Init, bytes)
    }
    pub fn queue_segment(
        &self,
        index: u32,
        bytes: &mut Vec<u8>,
    ) -> Result<PublicationTicket, FiniteVodError> {
        self.queue(OutputKey::Segment { index }, bytes)
    }
    pub fn queue_identity_metadata(
        &self,
        bytes: &mut Vec<u8>,
    ) -> Result<PublicationTicket, FiniteVodError> {
        self.queue(OutputKey::IdentityMetadata, bytes)
    }
    pub fn queue_encoded_process_metadata(
        &self,
        bytes: &mut Vec<u8>,
    ) -> Result<PublicationTicket, FiniteVodError> {
        self.queue(OutputKey::EncodedProcessMetadata, bytes)
    }
    /// Publish the bounded complete-artifact manifest into the same charged
    /// scratch directory. Only a StaticArtifactV1 plan admits this key.
    pub fn queue_complete_artifact_manifest(
        &self,
        bytes: &mut Vec<u8>,
    ) -> Result<PublicationTicket, FiniteVodError> {
        if bytes.is_empty() || bytes.len() > 4 * 1024 * 1024 || bytes.capacity() > 4 * 1024 * 1024 {
            return Err(invalid());
        }
        self.queue(OutputKey::CompleteArtifactManifest, bytes)
    }
    /// Commit and sync the exact bounded manifest through the original
    /// publication cell while the finite producer and source parents remain
    /// live. The caller must finish this before joining/freezing the graph.
    pub async fn publish_complete_artifact_manifest(
        &self,
        bytes: &mut Vec<u8>,
        deadline: Instant,
    ) -> Result<(), FiniteVodError> {
        if bytes.is_empty() || bytes.len() > 4 * 1024 * 1024 || bytes.capacity() > 4 * 1024 * 1024 {
            return Err(invalid());
        }
        self.validate_sources().await?;
        let publication = self
            .queue_file_wait(FiniteFile::CompleteArtifactManifest, bytes)
            .await?;
        self.finish(&publication, deadline).await?;
        let sync = self
            .queue_sync_one_wait(FiniteFile::CompleteArtifactManifest)
            .await?;
        self.finish(&sync, deadline).await?;
        self.validate_sources().await
    }
    fn queue(
        &self,
        output: OutputKey,
        bytes: &mut Vec<u8>,
    ) -> Result<PublicationTicket, FiniteVodError> {
        let id = self.runtime.queue_scratch_publication(
            &self.preparation,
            0,
            self.member,
            output,
            bytes,
        )?;
        Ok(PublicationTicket {
            id,
            kind: TicketKind::Publication,
        })
    }
    pub fn queue_sync(&self, files: &[FiniteFile]) -> Result<PublicationTicket, FiniteVodError> {
        if files.is_empty() || files.len() > 16 {
            return Err(invalid());
        }
        let mut outputs = [OutputKey::Init; 16];
        for (target, file) in outputs.iter_mut().zip(files) {
            *target = match file {
                FiniteFile::Init => OutputKey::Init,
                FiniteFile::Segment(index) => OutputKey::Segment { index: *index },
                FiniteFile::IdentityMetadata => OutputKey::IdentityMetadata,
                FiniteFile::EncodedProcessMetadata => OutputKey::EncodedProcessMetadata,
                FiniteFile::CompleteArtifactManifest => OutputKey::CompleteArtifactManifest,
            };
        }
        let id = self.runtime.queue_scratch_sync(
            &self.preparation,
            0,
            self.member,
            &outputs[..files.len()],
        )?;
        Ok(PublicationTicket {
            id,
            kind: TicketKind::Sync,
        })
    }
    async fn queue_file_wait(
        &self,
        file: FiniteFile,
        bytes: &mut Vec<u8>,
    ) -> Result<PublicationTicket, FiniteVodError> {
        let id = self
            .runtime
            .queue_scratch_publication_wait(&self.preparation, 0, self.member, file.key(), bytes)
            .await?;
        Ok(PublicationTicket {
            id,
            kind: TicketKind::Publication,
        })
    }
    async fn queue_sync_one_wait(
        &self,
        file: FiniteFile,
    ) -> Result<PublicationTicket, FiniteVodError> {
        let id = self
            .runtime
            .queue_scratch_sync_wait(&self.preparation, 0, self.member, &[file.key()])
            .await?;
        Ok(PublicationTicket {
            id,
            kind: TicketKind::Sync,
        })
    }

    pub async fn finish(
        &self,
        ticket: &PublicationTicket,
        deadline: Instant,
    ) -> Result<(), FiniteVodError> {
        match ticket.kind {
            TicketKind::Publication => {
                self.runtime
                    .publish_scratch_output(&self.preparation, 0, self.member, ticket.id, deadline)
                    .await
            }
            TicketKind::Sync => {
                self.runtime
                    .sync_scratch_outputs(&self.preparation, 0, self.member, ticket.id, deadline)
                    .await
            }
        }
        .map_err(Into::into)
    }
}

#[cfg(target_os = "linux")]
impl NodeResourceRuntime {
    /// Reuse a complete current-holder cache without opening a source or
    /// creating a replacement producer. A matching unresolved old obligation
    /// refuses; it never falls through to uncounted pathname cache adoption.
    pub async fn recovered_software_output(
        self: &Arc<Self>,
        rendition_id: &str,
        media_file_id: &str,
        object_version: &str,
        segment_count: u32,
    ) -> Result<Option<FiniteOutput>, FiniteVodError> {
        let selection = self
            .recovered_finite_selection(rendition_id, media_file_id, object_version, segment_count)
            .await
            .map_err(|_| OperationError::State)?;
        Ok(selection.map(
            |(preparation, member, directory_path, encoded_process_marker, app_guard)| {
                FiniteOutput {
                    runtime: self.clone(),
                    preparation,
                    member,
                    directory_path: Arc::new(directory_path),
                    encoded_process_marker,
                    _app_guard: app_guard,
                }
            },
        ))
    }
}

impl SoftwareVodOwner {
    /// Read the exact current ledger mixture and matching bounded history.
    /// A missing/stale mix or clock never becomes zero-cost evidence.
    #[cfg(target_os = "linux")]
    pub fn history_hint(&self) -> Option<FiniteHistoryHint> {
        let basis = self.history_basis?;
        let mix = self
            .runtime
            .ledger
            .actual_history_mix_for(self.history_family)
            .ok()?;
        let startup_key = basis.with_mix(mix);
        let steady_key = HistoryKey {
            phase: CostPhase::Steady,
            ..startup_key
        };
        let now = Instant::now();
        let unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok());
        let mut history = self.runtime.history.lock().ok()?;
        let recent_startup = history.recent(startup_key, now);
        let recent_steady = history.recent(steady_key, now);
        let startup =
            recent_startup.or_else(|| unix_ms.and_then(|at| history.prior(startup_key, at)));
        let steady = recent_steady.or_else(|| unix_ms.and_then(|at| history.prior(steady_key, at)));
        Some(FiniteHistoryHint {
            startup,
            steady,
            startup_recent: recent_startup.is_some(),
            steady_recent: recent_steady.is_some(),
            durable_units: self.history_units,
        })
    }
    /// Take the one precharged application entry/future slot. This must happen
    /// before constructing the rich daemon payload; a second take is refused.
    /// The lease's final Drop follows destruction of that payload and blocks
    /// terminal refund independently of native and journal closure.
    pub fn take_application_lease(
        &self,
        requested_bytes: usize,
    ) -> Result<FiniteApplicationLease, FiniteVodError> {
        self.runtime
            .finite_app_state(&self.preparation, 0)?
            .take_application_lease(requested_bytes)
            .ok_or_else(invalid)
    }
    /// Stable same-preparation identity for the selected Store worker record.
    /// This is a negative equality check, never physical or admission proof.
    pub fn prepared_handle_digest(&self) -> Result<[u8; 32], FiniteVodError> {
        self.runtime
            .finite_worker_binding_digests(&self.preparation, 0, self.scratch_member)
            .map(|(prepared, _)| prepared)
            .map_err(Into::into)
    }
    /// Requested dynamic heap retained by this owner through R. Source specs
    /// are only taken from their slots, so this value cannot grow after
    /// construction. The directory Arc payload is counted once by the output.
    pub fn requested_heap_bytes(&self) -> Option<usize> {
        software_owner_heap_bytes(
            self.source_members.capacity(),
            self.source_specs.capacity(),
            self.source_specs
                .iter()
                .flatten()
                .map(|spec| spec.path.capacity()),
        )
    }
    pub fn control(&self) -> SoftwareVodControl {
        SoftwareVodControl {
            control: Arc::clone(&self.control),
            _app_guard: Arc::clone(&self.app_guard),
        }
    }
    /// Selector only: construction does not open the source or output root.
    pub fn output(&self) -> FiniteOutput {
        FiniteOutput {
            runtime: Arc::clone(&self.runtime),
            preparation: self.preparation.clone(),
            member: self.scratch_member,
            directory_path: self.directory_path.clone(),
            encoded_process_marker: self.encoded_process_marker,
            _app_guard: Arc::clone(&self.app_guard),
        }
    }
    /// Resolve prepare BEFORE queueing either source native open or output
    /// traversal. Each source request is registered synchronously in core.
    pub async fn prepare(&mut self, deadline: Instant) -> Result<(), FiniteVodError> {
        self.runtime
            .resolve_preparation(&self.preparation, deadline)
            .await?;
        for (member, spec) in self.source_members.iter().zip(&mut self.source_specs) {
            if spec.is_some() {
                self.runtime
                    .queue_source_open(&self.preparation, 0, *member, spec)?;
            }
        }
        Ok(())
    }
    /// Resolve and acquire every original input, including the verified
    /// executable member, while leaving output and root entry dormant. A
    /// repeated call reuses the same registered source cells and operation IDs.
    pub async fn prepare_inputs(&mut self, deadline: Instant) -> Result<(), FiniteVodError> {
        self.prepare(deadline).await?;
        for member in &self.source_members {
            self.runtime
                .acquire_prepared_source(&self.preparation, 0, *member, deadline)
                .await?;
        }
        Ok(())
    }
    pub async fn open_source(
        &self,
        index: usize,
        deadline: Instant,
    ) -> Result<RetainedSourceLease, FiniteVodError> {
        if index >= self.media_source_count {
            return Err(invalid());
        }
        let member = *self
            .source_members
            .get(index)
            .ok_or(OperationError::State)?;
        self.runtime
            .open_prepared_source(&self.preparation, 0, member, deadline)
            .await
            .map_err(Into::into)
    }
    pub async fn open_output(&self, deadline: Instant) -> Result<FiniteOutput, FiniteVodError> {
        self.runtime
            .arm_scratch_bootstrap(&self.preparation, 0, self.scratch_member, deadline)
            .await?;
        self.runtime
            .open_scratch_bootstrap_root(&self.preparation, 0, self.scratch_member, deadline)
            .await?;
        self.runtime
            .open_scratch_bootstrap_sink(&self.preparation, 0, self.scratch_member, deadline)
            .await?;
        Ok(FiniteOutput {
            runtime: Arc::clone(&self.runtime),
            preparation: self.preparation.clone(),
            member: self.scratch_member,
            directory_path: self.directory_path.clone(),
            encoded_process_marker: self.encoded_process_marker,
            _app_guard: Arc::clone(&self.app_guard),
        })
    }
    /// Request stop through the original registration without waiting for a
    /// child-wait or journal lock. This is neither wait nor extinction proof.
    pub fn request_stop(&self) -> Result<(), FiniteVodError> {
        self.control.request().map_err(Into::into)
    }

    /// Metadata notifications do not confer root/task completion or refunds.
    pub fn take_events(&mut self) -> Option<tokio::sync::mpsc::Receiver<SoftwareVodEvent>> {
        self.events.take()
    }

    /// Core opens its own prearmed source/output cells and launches only the
    /// immutable compiled software recipe. No child, raw pipe or callback leaves.
    pub async fn start(&mut self, deadline: Instant) -> Result<(), FiniteVodError> {
        self.prepare_inputs(deadline).await?;
        self.open_output(deadline).await?;
        self.runtime
            .start_finite_vod(&self.preparation, 0, deadline)
            .await?;
        Ok(())
    }

    /// Freeze new producer entry, join the original writer/root and registered
    /// native jobs, then close observed parent source descriptors. Output assets,
    /// inherited copies, task results and all charges remain owned in core.
    /// Drain `take_events()` concurrently during normal production.
    pub async fn join_producer(&self) -> Result<FiniteProducerCompletion, FiniteVodError> {
        self.runtime
            .join_finite_producer(&self.preparation, 0)
            .await
            .map_err(Into::into)
    }

    /// Freeze the exact admitted output only after original producer/native
    /// joins and parent closure. This preserves R and every output owner; it
    /// does not establish or perform the conserving retained-output transfer.
    pub async fn freeze_output(
        &self,
        deadline: Instant,
    ) -> Result<FiniteProducerCompletion, FiniteVodError> {
        let completion = self.join_producer().await?;
        self.runtime
            .freeze_finite_output(&self.preparation, 0, self.scratch_member, deadline)
            .await?;
        Ok(completion)
    }

    /// Retain the same frozen output in S and return producer capacity only
    /// after one known journal COMMIT and one conserving ledger replacement.
    /// Cancellation/unknown results retain the original operation and full R.
    pub async fn retire_output(&self, deadline: Instant) -> Result<FiniteOutput, FiniteVodError> {
        let member = self
            .runtime
            .retire_finite_output(&self.preparation, 0, self.scratch_member, deadline)
            .await?;
        Ok(FiniteOutput {
            runtime: Arc::clone(&self.runtime),
            preparation: self.preparation.clone(),
            member,
            directory_path: self.directory_path.clone(),
            encoded_process_marker: self.encoded_process_marker,
            _app_guard: Arc::clone(&self.app_guard),
        })
    }

    pub async fn stop_producer(&self) -> Result<FiniteProducerCompletion, FiniteVodError> {
        self.request_stop()?;
        self.join_producer().await
    }

    /// Positive partial-start cancellation through original retained native
    /// owners. Unsupported earlier acquisition states keep their complete hold.
    pub async fn prepare_cancel(
        &self,
        deadline: Instant,
    ) -> Result<FiniteCloseTicket, FiniteVodError> {
        self.request_stop()?;
        let accepted = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            self.runtime
                .acknowledge_finite_preparation_for_cleanup(&self.preparation),
        )
        .await
        .map_err(|_| OperationError::Deadline)??;
        if accepted {
            self.runtime
                .prepare_failed_finite_vod_close(&self.preparation, 0, deadline)
                .await?;
        } else {
            self.runtime
                .prepare_unaccepted_finite_close(&self.preparation, 0)
                .await?;
        }
        let app_state = self.runtime.finite_app_state(&self.preparation, 0)?;
        Ok(FiniteCloseTicket {
            runtime: Arc::clone(&self.runtime),
            preparation: Some(self.preparation.clone()),
            app_state: Some(app_state),
        })
    }
}

fn id() -> v2::Id {
    v2::Id::new(Uuid::new_v4()).expect("new nonnil UUID")
}
fn invalid() -> FiniteVodError {
    OperationError::State.into()
}

impl NodeResourceRuntime {
    /// Project only the application allocations this finite owner retains.
    /// The caller adds these two values to its rendition/closing metadata
    /// before the original role is reserved. No native file, ID, or proof is
    /// acquired here; the actual arrays and path are checked before reserve.
    pub fn software_application_metadata_projection(
        &self,
        request: &SoftwareVodRequest,
    ) -> Result<(usize, usize), FiniteVodError> {
        if request.sources.is_empty() || request.sources.len() > 2 {
            return Err(invalid());
        }
        let root = self
            .rendition_root
            .as_ref()
            .and_then(|path| path.to_str())
            .ok_or_else(invalid)?;
        let (_, directory_path) = finite_directory_path(root, &request.rendition_id)?;
        let source_slots = request.sources.len().checked_add(1).ok_or_else(invalid)?;
        // The original executable path is cloned into the final source spec.
        // Measure that same owned clone, rather than its borrowed path length.
        let executable_path = request.command.executable().path().to_owned();
        let owner = software_owner_heap_bytes(
            source_slots,
            source_slots,
            request
                .sources
                .iter()
                .map(|source| source.path.capacity())
                .chain(std::iter::once(executable_path.capacity())),
        )
        .ok_or_else(invalid)?;
        let output = std::mem::size_of::<PathBuf>()
            .checked_add(2 * std::mem::size_of::<usize>())
            .and_then(|bytes| bytes.checked_add(directory_path.capacity()))
            .ok_or_else(invalid)?;
        Ok((owner, output))
    }

    /// Synchronous whole-role preparation before selected-path native IO. The
    /// daemon supplies real current demand/settings; unknown capacity refuses
    /// through the SAME ledger, with no legacy-admission fallback in this API.
    pub fn prepare_software_vod(
        self: &Arc<Self>,
        request: SoftwareVodRequest,
        deadline: Instant,
    ) -> Result<SoftwareVodOwner, FiniteVodError> {
        self.prepare_software_vod_with_naming(request, deadline, false, |_| Ok(()))
            .map(|(owner, ())| owner)
    }

    /// Admit the same fixed software graph with one additional, counted
    /// immutable manifest file in its retained S inventory. This is the
    /// selected static-background profile; it never enters a second output
    /// directory or grants a Store job physical-close authority.
    pub fn prepare_static_software_artifact(
        self: &Arc<Self>,
        mut request: SoftwareVodRequest,
        manifest_workspace_bytes: u64,
        application_owner_bytes: usize,
        deadline: Instant,
    ) -> Result<(SoftwareVodOwner, FiniteApplicationLease), FiniteVodError> {
        const MANIFEST_LIMIT: u64 = 4 * 1024 * 1024;
        let named_files = request.segment_count.checked_add(4).ok_or_else(invalid)?;
        if request.output.resident_file_objects < named_files
            || request.output.one_file_logical_bytes < MANIFEST_LIMIT
            || request.output.one_io_payload_bytes < MANIFEST_LIMIT
            || manifest_workspace_bytes < MANIFEST_LIMIT
        {
            return Err(invalid());
        }
        // The daemon projects the historical 4 MiB serialized buffer plus
        // its simultaneously live manifest/observation copies. Charge that
        // complete requested workspace before any R/native entry. The native
        // publication payload itself remains capped at MANIFEST_LIMIT.
        request.output.retained_metadata_bytes = request
            .output
            .retained_metadata_bytes
            .checked_add(manifest_workspace_bytes)
            .ok_or_else(invalid)?;
        let Metric::Known(resources) = &mut request.demand.resources else {
            return Err(invalid());
        };
        resources.peak_memory_bytes = resources
            .peak_memory_bytes
            .checked_add(manifest_workspace_bytes)
            .ok_or_else(invalid)?;
        self.prepare_software_vod_with_naming(request, deadline, true, |state| {
            state
                .take_application_lease(application_owner_bytes)
                .ok_or_else(invalid)
        })
    }

    fn prepare_software_vod_with_naming<T>(
        self: &Arc<Self>,
        request: SoftwareVodRequest,
        deadline: Instant,
        static_artifact: bool,
        application: impl FnOnce(
            &Arc<super::finite_lifetime::FiniteAppState>,
        ) -> Result<T, FiniteVodError>,
    ) -> Result<(SoftwareVodOwner, T), FiniteVodError> {
        if request.generation_epoch == 0 {
            return Err(OperationError::BeforeReservation(
                super::operations::PreReservationRefusal::Admission(
                    crate::adaptive_resources::AdmissionRefusal::InvalidBundle,
                ),
            )
            .into());
        }
        if !cfg!(target_os = "linux")
            || request.sources.is_empty()
            || request.sources.len() > 2
            || request.segment_count == 0
            || request.rendition_id.is_empty()
            || request.rendition_id.len() > 255
            || request.rendition_id.contains(['/', '\\', '\0'])
            || matches!(request.rendition_id.as_str(), "." | "..")
            || request.recipe_source_binding.is_empty()
            || request.recipe_source_binding.len() > 4096
            || !request.encoded_process_marker
            || request.sources.len() != request.command.source_count()
            || !request.command.matches_producer(&request.producer)
            || request.producer.segment_count() != request.segment_count as usize
            || request
                .sources
                .iter()
                .any(|source| !request.command.matches_source(source))
            || request.sources.iter().any(|s| {
                !s.path.is_absolute()
                    || s.path.as_os_str().len() > 4096
                    || s.scanner_size < 0
                    || s.object_version.is_empty()
                    || s.object_version.len() > 4096
                    || s.media_file_id.is_empty()
                    || s.media_file_id.len() > 4096
            })
        {
            return Err(invalid());
        }
        let (projected_owner_heap, projected_output_heap) =
            self.software_application_metadata_projection(&request)?;
        let root = self
            .rendition_root
            .as_ref()
            .and_then(|p| p.to_str())
            .ok_or_else(invalid)?;
        if root.len() > 4096 || !root.starts_with('/') || root == "/" {
            return Err(invalid());
        }
        let components: Vec<String> = root
            .strip_prefix('/')
            .ok_or_else(invalid)?
            .split('/')
            .map(str::to_owned)
            .collect();
        if components.len() > 64
            || components.iter().any(|c| {
                c.is_empty()
                    || c.len() > 255
                    || matches!(c.as_str(), "." | "..")
                    || c.contains(['\\', '\0'])
            })
        {
            return Err(invalid());
        }
        let Metric::Known(resources) = request.demand.resources else {
            return Err(invalid());
        };
        let minimum_handles = u64::try_from(request.sources.len() + 1)
            .map_err(|_| invalid())?
            .checked_mul(3)
            .and_then(|n| n.checked_add(u64::from(request.output.live_handles)))
            .and_then(|n| n.checked_add(3))
            .ok_or_else(invalid)?;
        if resources.handles < minimum_handles
            || resources.processes != 1
            || resources.cpu_ns_per_second == 0
            || resources.peak_memory_bytes < request.output.resident_memory_bytes
            || resources.scratch_bytes < request.output.reserved_storage_bytes
            || resources.hardware_slots != 0
            || resources.tuner_slots != 0
            || request.demand.trial
        {
            return Err(invalid());
        }
        let task_memory = request
            .producer
            .minimum_memory_bytes()?
            .checked_add(
                u64::try_from(request.command.resident_memory_bytes()?).map_err(|_| invalid())?,
            )
            .and_then(|bytes| bytes.checked_add(64 * 1024))
            .ok_or_else(invalid)?;
        let response_memory = request.output.response_memory_bytes;
        let response_limit = usize::try_from(response_memory).map_err(|_| invalid())?;
        let application_owner_limit =
            usize::try_from(request.application_owner_bytes).map_err(|_| invalid())?;
        let total_memory = SoftwareVodRequest::known_memory_component(
            &request.command,
            &request.producer,
            &request.output,
            request.application_owner_bytes,
        )?;
        if resources.peak_memory_bytes < total_memory
            || response_memory == 0
            || request.output.retained_metadata_bytes == 0
            || request.output.client_state_bytes == 0
        {
            return Err(invalid());
        }
        // This owner is allocated before the reservation and installed before
        // any selected source/output native entry. Its partition remains in S.
        let response_budget = ResponseBudget::with_metadata(
            response_limit,
            usize::try_from(
                request
                    .output
                    .retained_metadata_bytes
                    .checked_add(request.output.client_state_bytes)
                    .ok_or_else(invalid)?,
            )
            .map_err(|_| invalid())?,
        )
        .map_err(FiniteVodError)?;
        let namespace = std::fs::metadata("/proc/self/ns/mnt")
            .map_err(|e| FiniteVodError(OperationError::Physical(e)))?
            .ino();
        let domain = v2::FilesystemDomain {
            platform: v2::FilesystemPlatform::Linux,
            domain_id: id(),
            daemon_owner_id: v2::Id::new(self.owner.owner_id)?,
            os_boot: self.owner.os_boot.clone(),
            mount_namespace: v2::NativeValue::Known { value: namespace },
        };
        let root_member = id();
        let root_acquisition_id = id();
        let scratch_member = id();
        let asset = id();
        let (protected_child, selected_directory_path) =
            finite_directory_path(root, &request.rendition_id)?;
        let actual_output_heap = std::mem::size_of::<PathBuf>()
            .checked_add(2 * std::mem::size_of::<usize>())
            .and_then(|bytes| bytes.checked_add(selected_directory_path.capacity()))
            .ok_or_else(invalid)?;
        if actual_output_heap > projected_output_heap {
            return Err(invalid());
        }
        let directory_path = Arc::new(selected_directory_path);
        let slot = |member_id, protected_paths| v2::MemberSlot {
            member_id,
            owned_by: None,
            depends_on: vec![],
            protected_paths,
        };
        let task_member = id();
        let stdout_owner = id();
        let diagnostic_owner = id();
        let sink_owner = id();
        let task_buffer = id();
        let task_source = &request.sources[0];
        let task_association = v2::SourceAssociation {
            media_file_id: task_source.media_file_id.clone(),
            object_version: task_source.object_version.clone(),
            recipe_source_binding: request.recipe_source_binding.clone(),
        };
        let media_source_count = request.sources.len();
        let executable = request.command.executable();
        let binary_identity = executable.source_identity();
        let binary_digest = executable.sha256();
        let binary_binding =
            super::operations::source_files::executable_recipe_binding(&binary_digest);
        let binary_source = FiniteSourceRequest {
            path: executable.path().to_owned(),
            media_file_id: binary_binding.clone(),
            object_version: binary_identity.render_s1(),
            scanner_size: i64::try_from(binary_identity.size).map_err(|_| invalid())?,
            scanner_mtime: binary_identity.mtime,
        };
        let mut plans = Vec::with_capacity(media_source_count + 4);
        let mut source_members = Vec::with_capacity(media_source_count + 1);
        let mut source_specs = Vec::with_capacity(media_source_count + 1);
        #[cfg(target_os = "linux")]
        let history_basis = pump::history_basis(&request);
        let sources = request
            .sources
            .into_iter()
            .map(|source| (source, None, request.recipe_source_binding.clone()))
            .chain(std::iter::once((
                binary_source,
                Some(binary_digest),
                binary_binding.clone(),
            )));
        for (source, expected_sha256, recipe_source_binding) in sources {
            let member = id();
            let source_copy = id();
            plans.push(TypedMemberPlan::Source {
                slot: slot(
                    member,
                    vec![source.path.to_str().ok_or_else(invalid)?.to_owned()],
                ),
                source: SourcePlan {
                    holder_id: id(),
                    source_association: v2::SourceAssociation {
                        media_file_id: source.media_file_id,
                        object_version: source.object_version,
                        recipe_source_binding,
                    },
                    filesystem_domain: domain.clone(),
                    copies: vec![
                        SourceCopyPlan {
                            copy_id: source_copy,
                            owner: SourceCopyOwner::LocalSourceCell,
                            acquisition_operation_id: id(),
                            parent_copy_id: None,
                        },
                        SourceCopyPlan {
                            copy_id: id(),
                            owner: SourceCopyOwner::LocalRoot {
                                root_member_id: root_member,
                            },
                            acquisition_operation_id: root_acquisition_id,
                            parent_copy_id: Some(source_copy),
                        },
                    ],
                },
            });
            source_members.push(member.uuid());
            source_specs.push(Some(SourceOpenSpec {
                path: source.path,
                scanner_size: source.scanner_size,
                scanner_mtime: source.scanner_mtime,
                expected_sha256,
            }));
        }
        let actual_owner_heap = software_owner_heap_bytes(
            source_members.capacity(),
            source_specs.capacity(),
            source_specs
                .iter()
                .flatten()
                .map(|spec| spec.path.capacity()),
        )
        .ok_or_else(invalid)?;
        if actual_owner_heap > projected_owner_heap {
            return Err(invalid());
        }
        let bounds = request.output;
        let limits = ScratchLimitsV1 {
            resident_file_objects: bounds.resident_file_objects,
            resident_logical_bytes: bounds.resident_logical_bytes,
            reserved_storage_bytes: bounds.reserved_storage_bytes,
            one_file_logical_bytes: bounds.one_file_logical_bytes,
            one_io_payload_bytes: bounds.one_io_payload_bytes,
            pending_native_jobs: 1,
            live_handles: bounds.live_handles,
            reader_leases: bounds.reader_leases,
            resident_memory_bytes: bounds.resident_memory_bytes,
            allocation: AllocationAccountingV1::LogicalAndReportedAllocationV1 {
                allocation_unit_bytes: bounds.allocation_unit_bytes,
                per_object_reserve_bytes: bounds.per_object_reserve_bytes,
                directory_reserve_bytes: bounds.child_directory_reserve_bytes,
            },
        };
        plans.push(TypedMemberPlan::ScratchV3 {
            slot: slot(scratch_member, vec![protected_child.clone()]),
            plan: ScratchPlanV3 {
                version: 3,
                asset_id: asset,
                rendition_id: request.rendition_id,
                generation_epoch: request.generation_epoch,
                filesystem_domain: domain,
                directory: DirectoryOriginV1::ConfiguredRootBootstrapV1 {
                    bootstrap: RootBootstrapPlanV1 {
                        version: 1,
                        authority: ConfiguredRootAuthorityV1 {
                            installation_id: v2::Id::new(self.recovered.installation_id)?,
                            purpose: RootPurpose::RenditionCacheV1,
                            resolved_absolute_root: root.to_owned(),
                            traversal: RootTraversal::NamespaceRootToConfiguredRootV1,
                            descendant_mounts: DescendantMounts::RefuseV1,
                        },
                        components,
                        child_component: protected_child
                            .rsplit('/')
                            .next()
                            .ok_or_else(invalid)?
                            .to_owned(),
                        protected_child_path: protected_child,
                        bounds: BootstrapBoundsV1 {
                            max_components: 64,
                            max_path_bytes: 4096,
                            max_component_bytes: 255,
                            peak_directory_handles: 2,
                            pending_native_jobs: 1,
                            resident_memory_bytes: bounds.bootstrap_memory_bytes,
                            child_directory_reserve_bytes: bounds.child_directory_reserve_bytes,
                            parent_entry_reserve_bytes: bounds.parent_entry_reserve_bytes,
                        },
                    },
                },
                outputs: OutputNamesV1 {
                    init: true,
                    first_segment: 0,
                    segment_end_exclusive: u64::from(request.segment_count),
                    naming: if static_artifact {
                        OutputNamingV1::StaticArtifactV1 {
                            encoded_process: request.encoded_process_marker,
                        }
                    } else {
                        OutputNamingV1::FiniteVodMetadataV2 {
                            encoded_process: request.encoded_process_marker,
                        }
                    },
                },
                limits,
            },
        });
        plans.push(TypedMemberPlan::Legacy(PlannedMember {
            member_id: root_member.uuid(),
            kind: MemberKind::Root,
            adapter: Adapter::ProcessRoot,
            adapter_version: 1,
            domain: self.owner.daemon_process.domain.clone(),
            locator: request
                .command
                .program()
                .to_str()
                .ok_or_else(invalid)?
                .to_owned(),
            recovery: if cfg!(target_os = "linux")
                && self.owner.platform_domain.continuity_provider.as_deref()
                    == Some("linux_kernel_pid_namespace_v1")
                && matches!(&self.owner.os_boot, BootIdentity::Known { provider, .. }
                    if provider == "linux_kernel_boot_id")
            {
                match &self.owner.platform_domain.namespace {
                    Some(pid_namespace) => RecoveryMethod::TrustedFixedSoftwareSameBootV1 {
                        pid_namespace: pid_namespace.clone(),
                        executable_binding: binary_binding.clone(),
                    },
                    None => RecoveryMethod::LiveOnly {
                        reason: "fixed software VOD root namespace unavailable".into(),
                    },
                }
            } else {
                RecoveryMethod::LiveOnly {
                    reason: "fixed software VOD root continuity unavailable".into(),
                }
            },
            owned_by: None,
            depends_on: vec![],
            maximum_cardinality: 1,
            protected_paths: vec![],
        }));
        plans.push(TypedMemberPlan::VodOutputTask {
            slot: v2::MemberSlot {
                member_id: task_member,
                owned_by: Some(root_member),
                depends_on: vec![root_member, scratch_member],
                protected_paths: vec![],
            },
            task: super::operations::plans::VodTaskPlan {
                root_member_id: root_member,
                generation_epoch: request.generation_epoch,
                source_association: task_association,
                stdout_owner_id: stdout_owner,
                diagnostic_owner_id: diagnostic_owner,
                asset_id: asset,
                sink_owner_id: sink_owner,
                inventory: v2::IoInventory {
                    pending_io: vec![],
                    buffers: vec![v2::OwnerAllocation {
                        owner_id: task_buffer,
                        memory_bytes: task_memory,
                        handles: 0,
                    }],
                    readers: vec![
                        v2::OwnerAllocation {
                            owner_id: stdout_owner,
                            memory_bytes: 0,
                            handles: 1,
                        },
                        v2::OwnerAllocation {
                            owner_id: diagnostic_owner,
                            memory_bytes: 0,
                            handles: 1,
                        },
                    ],
                },
            },
        });
        let roles = [FamilyRoleDemand {
            role: FamilyRole::Video,
            demand: request.demand,
            retained: None,
        }];
        // Construct every fallible application owner before accepted R. The
        // initializer below only binds these same objects to the private role.
        let app_state =
            super::finite_lifetime::FiniteAppState::with_application_limit(application_owner_limit);
        let app_guard = app_state.new_guard().ok_or_else(invalid)?;
        let application = application(&app_state)?;
        let family = self.new_family()?;
        let control = Arc::new(FiniteControl::new());
        let source_members_for_launch = source_members.clone();
        let control_for_launch = Arc::clone(&control);
        let directory_for_output = Arc::clone(&directory_path);
        let guard_for_output = Arc::clone(&app_guard);
        let producer_memory_claim = request.producer.memory_claim();
        let (preparation, events) = match self.prepare_typed_families_claiming_initialized(
            &[FamilyDemand {
                identity: family,
                roles: &roles,
            }],
            vec![TypedGraphPlan {
                attempt_id: None,
                members: plans,
            }],
            Some(&[producer_memory_claim]),
            deadline,
            |preparation| {
                let output = FiniteOutput {
                    runtime: Arc::clone(self),
                    preparation: preparation.clone(),
                    member: scratch_member.uuid(),
                    directory_path: directory_for_output,
                    encoded_process_marker: request.encoded_process_marker,
                    _app_guard: guard_for_output,
                };
                let (pump, events) =
                    pump::SoftwarePump::from_encoded_output(output, request.producer);
                #[cfg(target_os = "linux")]
                let pump = {
                    let mut pump = pump;
                    if let Some(basis) = history_basis {
                        pump.bind_history_context(basis, self.ledger.clone(), family);
                    }
                    pump
                };
                self.initialize_finite_vod(
                    preparation,
                    0,
                    root_member.uuid(),
                    task_member.uuid(),
                    root_acquisition_id.uuid(),
                    source_members_for_launch,
                    request.command,
                    pump,
                    control_for_launch,
                    app_state,
                    response_budget,
                );
                events
            },
        ) {
            Ok(preparation) => preparation,
            Err(error) => {
                // No accepted bundle means no source/native work was entered.
                // Remove only this unused family selector; never its charges.
                let _ = self.remove_unused_family(family);
                return Err(error.into());
            }
        };
        Ok((
            SoftwareVodOwner {
                runtime: Arc::clone(self),
                preparation,
                source_members,
                source_specs,
                media_source_count,
                scratch_member: scratch_member.uuid(),
                directory_path,
                encoded_process_marker: request.encoded_process_marker,
                events: Some(events),
                control,
                #[cfg(target_os = "linux")]
                history_basis,
                #[cfg(target_os = "linux")]
                history_family: family,
                #[cfg(target_os = "linux")]
                history_units: request.segment_count,
                app_guard,
            },
            application,
        ))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod control_tests;

#[cfg(test)]
mod tests;

#[cfg(all(test, target_os = "linux", feature = "fixtures"))]
#[path = "finite_vod/tests/cleanup.rs"]
pub(super) mod cleanup_tests;
