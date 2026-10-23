//! Concrete finite software-VOD preparation interface for the daemon.
//! Request data selects plans, never native facts, callbacks or refund proofs.
mod command;
mod executable;
mod initial_unit;
pub(in crate::adaptive_resources) mod pump;
pub use super::operations::bootstrap::readers::{FiniteReadChunk, FiniteReader};
pub use command::SoftwareAvcCommand;
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
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use v2::scratch::bootstrap::*;
use v2::scratch::{
    AllocationAccountingV1, OutputKey, OutputNamesV1, OutputNamingV1, ScratchLimitsV1,
};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct FiniteVodError(OperationError);
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

/// Explicit logical/reported-allocation profile. These are admission bounds,
/// not a claim that a filesystem or host enforces a physical-byte quota.
pub struct FiniteOutputLimits {
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
    pub generation_epoch: u64,
    pub recipe_source_binding: String,
    pub command: SoftwareAvcCommand,
    pub producer: SoftwareAvcPumpPlan,
    pub sources: Vec<FiniteSourceRequest>,
    pub segment_count: u32,
    pub encoded_process_marker: bool,
    pub demand: WorkloadDemand,
    pub output: FiniteOutputLimits,
}

/// Nonserde ownership selection. The runtime book retains all actual resources
/// if a request drops this value or cancels any borrowed setup wait.
pub(in crate::adaptive_resources::runtime) struct FiniteControl {
    requested: std::sync::atomic::AtomicBool,
    root: std::sync::Mutex<Option<crate::process::priority::RegisteredChildControl>>,
    task: std::sync::Mutex<Option<tokio::task::AbortHandle>>,
}
impl FiniteControl {
    fn new() -> Self {
        Self {
            requested: std::sync::atomic::AtomicBool::new(false),
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

pub struct SoftwareVodOwner {
    runtime: Arc<NodeResourceRuntime>,
    preparation: OwnedPreparation,
    source_members: Vec<Uuid>,
    source_specs: Vec<Option<SourceOpenSpec>>,
    media_source_count: usize,
    scratch_member: Uuid,
    directory_path: PathBuf,
    encoded_process_marker: bool,
    events: Option<tokio::sync::mpsc::Receiver<SoftwareVodEvent>>,
    control: Arc<FiniteControl>,
}

/// A bounded selection of the same custodian, not an exported File/Arc<File>.
/// Cloning this selection does not duplicate a directory or admission charge.
#[derive(Clone)]
pub struct FiniteOutput {
    runtime: Arc<NodeResourceRuntime>,
    preparation: OwnedPreparation,
    member: Uuid,
    directory_path: PathBuf,
    encoded_process_marker: bool,
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
}

impl FiniteFile {
    fn key(self) -> OutputKey {
        match self {
            Self::Init => OutputKey::Init,
            Self::Segment(index) => OutputKey::Segment { index },
            Self::IdentityMetadata => OutputKey::IdentityMetadata,
            Self::EncodedProcessMetadata => OutputKey::EncodedProcessMetadata,
        }
    }
}

impl FiniteOutput {
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

impl SoftwareVodOwner {
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
        self.prepare(deadline).await?;
        for member in &self.source_members {
            self.runtime
                .acquire_prepared_source(&self.preparation, 0, *member, deadline)
                .await?;
        }
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
        })
    }

    pub async fn stop_producer(&self) -> Result<FiniteProducerCompletion, FiniteVodError> {
        self.request_stop()?;
        self.join_producer().await
    }

    pub async fn cancel(&self, deadline: Instant) -> Result<(), FiniteVodError> {
        self.runtime
            .cancel_preparation(&self.preparation, deadline)
            .await
            .map_err(Into::into)
    }
}

fn id() -> v2::Id {
    v2::Id::new(Uuid::new_v4()).expect("new nonnil UUID")
}
fn invalid() -> FiniteVodError {
    OperationError::State.into()
}

impl NodeResourceRuntime {
    /// Synchronous whole-role preparation before selected-path native IO. The
    /// daemon supplies real current demand/settings; unknown capacity refuses
    /// through the SAME ledger, with no legacy-admission fallback in this API.
    pub fn prepare_software_vod(
        self: &Arc<Self>,
        request: SoftwareVodRequest,
        deadline: Instant,
    ) -> Result<SoftwareVodOwner, FiniteVodError> {
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
        let protected_child = format!("{root}/{}", request.rendition_id);
        let directory_path = PathBuf::from(&protected_child);
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
        let task_memory = request
            .producer
            .minimum_memory_bytes()?
            .checked_add(
                u64::try_from(request.command.resident_memory_bytes()?).map_err(|_| invalid())?,
            )
            .and_then(|bytes| bytes.checked_add(64 * 1024))
            .ok_or_else(invalid)?;
        if resources
            .peak_memory_bytes
            .checked_sub(request.output.resident_memory_bytes)
            .is_none_or(|remaining| remaining < task_memory)
        {
            return Err(invalid());
        }
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
        let sources = request
            .sources
            .into_iter()
            .map(|source| (source, None, request.recipe_source_binding.clone()))
            .chain(std::iter::once((
                binary_source,
                Some(binary_digest),
                binary_binding,
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
                    naming: OutputNamingV1::FiniteVodMetadataV2 {
                        encoded_process: request.encoded_process_marker,
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
            recovery: RecoveryMethod::LiveOnly {
                reason: "fixed software VOD graph; no old-domain release proof".into(),
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
        let family = self.new_family()?;
        let preparation = match self.prepare_typed_families(
            &[FamilyDemand {
                identity: family,
                roles: &roles,
            }],
            vec![TypedGraphPlan {
                attempt_id: None,
                members: plans,
            }],
            deadline,
        ) {
            Ok(preparation) => preparation,
            Err(error) => {
                // No accepted bundle means no source/native work was entered.
                // Remove only this unused family selector; never its charges.
                let _ = self.remove_unused_family(family);
                return Err(error.into());
            }
        };
        let output = FiniteOutput {
            runtime: Arc::clone(self),
            preparation: preparation.clone(),
            member: scratch_member.uuid(),
            directory_path: directory_path.clone(),
            encoded_process_marker: true,
        };
        let (pump, events) = pump::SoftwarePump::new(output, request.producer)
            .map_err(|error| FiniteVodError(OperationError::Physical(error)))?;
        let control = Arc::new(FiniteControl::new());
        self.queue_finite_vod(
            &preparation,
            0,
            root_member.uuid(),
            task_member.uuid(),
            root_acquisition_id.uuid(),
            source_members.clone(),
            request.command,
            pump,
            Arc::clone(&control),
        )?;
        Ok(SoftwareVodOwner {
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
        })
    }
}

#[cfg(all(test, target_os = "linux"))]
mod control_tests;

#[cfg(test)]
mod tests;
