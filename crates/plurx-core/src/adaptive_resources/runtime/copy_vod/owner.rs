//! Prepared one-input rolling copy role. Source, output and root entry all
//! resolve through the same already charged bundle and its original commands.
use super::*;
use crate::adaptive_resources::physical_graph::copy_vod::copy_task_structural_bytes;
use crate::adaptive_resources::physical_graph::copy_vod::CopyWork;
use crate::adaptive_resources::runtime::finite_vod::{finite_directory_path, FiniteVodError};
use crate::adaptive_resources::runtime::finite_vod::{
    FiniteClientStateLease, FiniteMetadataLease, FiniteReader, FiniteResponseBuffer,
};
use crate::adaptive_resources::runtime::operations::plans::{
    SourceCopyOwner, SourceCopyPlan, SourcePlan, TypedGraphPlan, TypedMemberPlan,
};
use crate::adaptive_resources::runtime::operations::source_files::{
    executable_recipe_binding, SourceOpenSpec,
};
use crate::adaptive_resources::runtime::response_memory::ResponseBudget;
use crate::adaptive_resources::{FamilyDemand, FamilyRole, FamilyRoleDemand, MemoryClaim};
use crate::copy_segmenter::PreparedCopyParser;
use crate::resource_custody::codec_v2 as v2;
use crate::resource_custody::{Adapter, MemberKind, PlannedMember, RecoveryMethod};
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::sync::Mutex;
use v2::scratch::bootstrap::*;
use v2::scratch::{
    AllocationAccountingV1, OutputKey, OutputNamesV1, OutputNamingV1, ScratchLimitsV1,
};

/// Hard per-segment ceiling of the selected typed copy graph. Legacy
/// copy segmenting may use a larger global default; selected planners must
/// derive parser, output storage and memory from this same admitted bound.
pub const COPY_VOD_MAX_SEGMENT_BYTES: usize = 48 * 1024 * 1024;

fn id() -> v2::Id {
    v2::Id::new(Uuid::new_v4()).expect("new UUID")
}
fn invalid() -> FiniteVodError {
    super::super::operations::OperationError::State.into()
}

/// Pure request/profile rejection before the original family is allocated.
/// Never use for constructor, native, journal or cleanup failures after R.
fn invalid_before_reservation() -> FiniteVodError {
    super::super::operations::OperationError::BeforeReservation(
        super::super::operations::PreReservationRefusal::Admission(
            crate::adaptive_resources::AdmissionRefusal::InvalidBundle,
        ),
    )
    .into()
}

fn known_copy_memory(
    runtime: &NodeResourceRuntime,
    request: &CopyVodRequest,
    selected_directory_capacity: usize,
    binary_path_capacity: usize,
) -> Result<(usize, u64), FiniteVodError> {
    let bounds = &request.output;
    let max_pending_bytes = request
        .parser_limits
        .max_bytes
        .checked_add(request.reader_bounds.unit_bytes)
        .ok_or_else(invalid)?;
    let task = CopySink::requested_memory_bytes(request.max_segments, request.parser_limits)
        .and_then(|n| n.checked_add(request.command.resident_memory_bytes().ok()?))
        .and_then(|n| n.checked_add(request.command.source().requested_heap_bytes()?))
        .and_then(|n| n.checked_add(request.reader_bounds.buffer_bytes))
        .and_then(|n| n.checked_add(256 * 1024 + 8192))
        .and_then(|n| n.checked_add(std::mem::size_of::<PreparedCopyParser>()))
        .and_then(|n| {
            n.checked_add(
                request
                    .max_pending_fragments
                    .checked_mul(std::mem::size_of::<crate::fmp4::Fragment>())?,
            )
        })
        .and_then(|n| {
            n.checked_add(
                usize::try_from(request.max_segments)
                    .ok()?
                    .checked_mul(std::mem::size_of::<(u32, u64)>())?,
            )
        })
        .and_then(|n| n.checked_add(max_pending_bytes))
        .and_then(|n| {
            n.checked_add(
                usize::try_from(bounds.one_io_payload_bytes)
                    .ok()?
                    .checked_mul(2)?,
            )
        })
        .and_then(|n| n.checked_add(std::mem::size_of::<CopyWork>()))
        .and_then(|n| n.checked_add(NodeResourceRuntime::copy_launch_structural_bytes()))
        .and_then(|n| n.checked_add(request.rendition_id.capacity()))
        .and_then(|n| {
            n.checked_add(super::cleanup_memory::requested_bytes(
                runtime,
                request,
                selected_directory_capacity,
            )?)
        })
        .and_then(|n| n.checked_add(copy_task_structural_bytes()?))
        .ok_or_else(invalid)?;
    // One retained role pointer survives CopyLaunch removal. The returned
    // ticket and exclusive claim are compact selectors; account their inline
    // storage in addition to the actual nested async-frame projection.
    let owner = std::mem::size_of::<CopyVodOwner>()
        .checked_add(std::mem::size_of::<Option<Arc<CopyCleanupCustody>>>())
        .and_then(|n| n.checked_add(std::mem::size_of::<CopyCloseTicket>()))
        .and_then(|n| {
            n.checked_add(std::mem::size_of::<PathBuf>() + 2 * std::mem::size_of::<usize>())
        })
        .and_then(|n| n.checked_add(selected_directory_capacity))
        .and_then(|n| n.checked_add(request.source.path.capacity()))
        .and_then(|n| n.checked_add(binary_path_capacity))
        .and_then(|n| {
            n.checked_add(
                std::mem::size_of::<super::super::finite_lifetime::FiniteAppState>()
                    + 2 * std::mem::size_of::<usize>(),
            )
        })
        .and_then(|n| {
            n.checked_add(
                std::mem::size_of::<super::super::finite_lifetime::FiniteAppGuard>()
                    + 2 * std::mem::size_of::<usize>(),
            )
        })
        .and_then(|n| {
            n.checked_add(std::mem::size_of::<ResponseBudget>() + 2 * std::mem::size_of::<usize>())
        })
        .and_then(|n| {
            n.checked_add(
                std::mem::size_of::<tokio::sync::Mutex<()>>() + 2 * std::mem::size_of::<usize>(),
            )
        })
        .and_then(|n| n.checked_add(CopyCleanupCustody::requested_allocation_bytes()?))
        .and_then(|n| {
            n.checked_add(
                std::mem::size_of::<tokio::sync::Notify>() + 2 * std::mem::size_of::<usize>(),
            )
        })
        .ok_or_else(invalid)?;
    let total = bounds
        .resident_memory_bytes
        .checked_add(bounds.response_memory_bytes)
        .and_then(|n| n.checked_add(bounds.retained_metadata_bytes))
        .and_then(|n| n.checked_add(bounds.client_state_bytes))
        .and_then(|n| n.checked_add(u64::try_from(task).ok()?))
        .and_then(|n| n.checked_add(u64::try_from(owner).ok()?))
        .and_then(|n| n.checked_add(request.application_owner_bytes))
        .ok_or_else(invalid)?;
    Ok((task, total))
}

/// The original producer selector. Dropping it cannot refund the journaled
/// role; the runtime retains native cells and transition IDs across waits.
pub struct CopyVodOwner {
    runtime: Arc<NodeResourceRuntime>,
    preparation: OwnedPreparation,
    source_members: [Uuid; 2],
    source_specs: [Option<SourceOpenSpec>; 2],
    scratch_member: Uuid,
    directory_path: Arc<PathBuf>,
    produced_playlist: Arc<Mutex<String>>,
    served_playlist: Arc<Mutex<String>>,
    playlist_commit: Arc<tokio::sync::Mutex<()>>,
    playlist_capacity: usize,
    target_seconds: u32,
    app_state: Arc<super::super::finite_lifetime::FiniteAppState>,
    cleanup_custody: Arc<CopyCleanupCustody>,
    progress: Arc<tokio::sync::Notify>,
    // Last: this guard observes destruction of the rich owner fields above.
    _app_guard: Arc<super::super::finite_lifetime::FiniteAppGuard>,
}

/// Reason why the same copy R remains held for reconciliation.
/// These diagnostics grant no close or restart-settlement authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyCleanupHoldReason {
    UncertainLaunch,
    PoisonedTerminal,
    LostTaskTerminal,
    AbandonedTask,
    FailedPipeClose,
    FailedSourceClose,
    ValidationRejected,
    FailedParserKill,
    UnboundRootIdentity,
    TerminalIdentityMismatch,
}

/// Snapshot of an actual original held reservation. Missing/busy diagnostics
/// are represented by None from the accessor, never an invented zero cost.
#[derive(Clone, Debug, serde::Serialize)]
pub struct CopyCleanupHoldDiagnostic {
    pub preparation_id: Uuid,
    pub graph_id: Option<Uuid>,
    pub reason: CopyCleanupHoldReason,
    pub held_resources: crate::adaptive_resources::ResourceVector,
}
impl NodeResourceRuntime {
    pub fn copy_cleanup_holds(&self) -> Option<Vec<CopyCleanupHoldDiagnostic>> {
        self.copy_cleanup_holds_inner()
    }
}

/// Original registered parser result for rolling actor classification only.
/// It does not imply root exit, pipe closure, scratch cleanup or a refund.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CopyProductionResult {
    pub completed: bool,
    pub segments: u64,
}
impl CopyProductionResult {
    fn from_terminal(
        terminal: crate::adaptive_resources::physical_graph::copy_vod::CopyTaskTerminal,
    ) -> Self {
        use crate::adaptive_resources::physical_graph::copy_vod::CopyTaskTerminal;
        match terminal {
            CopyTaskTerminal::NeverEntered => Self {
                completed: false,
                segments: 0,
            },
            CopyTaskTerminal::Entered(summary) => Self {
                completed: summary.completed,
                segments: summary.segments,
            },
        }
    }
}

/// A compact original-operation selector. Native and typed settlement facts
/// stay in the runtime book across waiter cancellation or an unknown commit.
pub struct CopyCloseTicket {
    runtime: Arc<NodeResourceRuntime>,
    preparation: Option<OwnedPreparation>,
    app_state: Option<Arc<super::super::finite_lifetime::FiniteAppState>>,
    cleanup_claim: Option<CopyCloseClaim>,
}
impl CopyCloseTicket {
    /// Observes this ticket's same application fence; never native/settlement proof.
    #[cfg(test)]
    pub(in crate::adaptive_resources::runtime) fn fixture_application_released(
        &self,
    ) -> Option<bool> {
        self.app_state.as_ref().map(|state| state.released())
    }

    pub async fn finish_retirement(&mut self, deadline: Instant) -> Result<(), FiniteVodError> {
        if self.preparation.is_none() {
            return Ok(());
        }
        if let Some(app_state) = self.app_state.as_ref() {
            if !app_state.wait_released(deadline).await {
                return Err(FiniteVodError::from(
                    super::super::operations::OperationError::Deadline,
                ));
            }
            self.app_state = None;
        }
        tokio::time::timeout_at(
            deadline.into(),
            self.runtime
                .finish_finite_close(&mut self.preparation, 0, deadline),
        )
        .await
        .map_err(|_| super::super::operations::OperationError::Deadline)?
        .map_err(FiniteVodError::from)?;
        if let Some(claim) = self.cleanup_claim.take() {
            claim.complete();
        }
        Ok(())
    }
}

/// Fixed canonical copy outputs. The selector contains no raw descriptor and
/// every read goes through the already charged ScratchAssetsV3 reader cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyFile {
    Init,
    Segment(u32),
    Playlist,
}
impl CopyFile {
    pub fn from_canonical_name(name: &str) -> Option<Self> {
        match name {
            "init.mp4" => Some(Self::Init),
            "index.m3u8" => Some(Self::Playlist),
            _ => {
                let digits = name.strip_prefix("seg")?.strip_suffix(".m4s")?;
                if digits.len() != 5 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                    return None;
                }
                let index: u32 = digits.parse().ok()?;
                (format!("seg{index:05}.m4s") == name).then_some(Self::Segment(index))
            }
        }
    }
    fn key(self) -> OutputKey {
        match self {
            Self::Init => OutputKey::Init,
            Self::Segment(index) => OutputKey::Segment { index },
            Self::Playlist => OutputKey::Playlist,
        }
    }
}

#[derive(Clone)]
pub struct CopyOutput {
    runtime: Arc<NodeResourceRuntime>,
    preparation: OwnedPreparation,
    member: Uuid,
    directory_path: Arc<PathBuf>,
    produced_playlist: Arc<Mutex<String>>,
    served_playlist: Arc<Mutex<String>>,
    playlist_commit: Arc<tokio::sync::Mutex<()>>,
    playlist_capacity: usize,
    target_seconds: u32,
    _app_guard: Arc<super::super::finite_lifetime::FiniteAppGuard>,
}
/// Journal-known but not yet visible through HTTP. The rolling actor must
/// accept this exact revision before the original output cache activates it.
pub struct CopyServedRevision {
    bytes: FiniteResponseBuffer,
    served_playlist: Arc<Mutex<String>>,
}
impl AsRef<[u8]> for CopyServedRevision {
    fn as_ref(&self) -> &[u8] {
        self.bytes.as_ref()
    }
}
impl CopyOutput {
    pub fn directory_path(&self) -> &Path {
        &self.directory_path
    }
    /// Observe only the original registered parser result. The owner retains
    /// actual root, pipe, native jobs and typed retirement separately.
    pub async fn wait_producer_result(
        &self,
        deadline: Instant,
    ) -> Result<CopyProductionResult, FiniteVodError> {
        let summary = self
            .runtime
            .wait_copy_parser_result(&self.preparation, 0, deadline)
            .await
            .map_err(FiniteVodError::from)?;
        Ok(CopyProductionResult::from_terminal(summary))
    }
    /// A scratch object can exist before the bounded copy sink commits the
    /// playlist that names it. Only a journal-known playlist revision makes
    /// that object eligible for a selected rolling response.
    pub fn playlist_names(&self, file: CopyFile) -> Result<bool, FiniteVodError> {
        let playlist = self.served_playlist.lock().map_err(|_| invalid())?;
        if playlist.is_empty() {
            return Ok(false);
        }
        Ok(match file {
            CopyFile::Playlist | CopyFile::Init => true,
            CopyFile::Segment(index) => playlist.lines().any(|line| {
                matches!(CopyFile::from_canonical_name(line), Some(CopyFile::Segment(found)) if found == index)
            }),
        })
    }
    pub async fn file_len(&self, file: CopyFile) -> Result<Option<u64>, FiniteVodError> {
        if file == CopyFile::Playlist {
            let latest = self.served_playlist.lock().map_err(|_| invalid())?;
            return if latest.is_empty() {
                Ok(None)
            } else {
                Ok(Some(u64::try_from(latest.len()).map_err(|_| invalid())?))
            };
        }
        self.runtime
            .finite_file_len(&self.preparation, 0, self.member, &file.key())
            .await
            .map_err(Into::into)
    }
    /// One charged compact projection of the already journaled segment
    /// inventory. It avoids a per-name full fold scan while the daemon
    /// parses the producer's bounded EXTINF timeline.
    pub async fn produced_segment_lengths(&self) -> Result<Vec<(u32, u64)>, FiniteVodError> {
        self.runtime
            .copy_produced_segment_lengths(&self.preparation, 0, self.member)
            .await
            .map_err(Into::into)
    }
    pub async fn prepare_reader(
        &self,
        file: CopyFile,
        start: u64,
        end_exclusive: u64,
        memory_cap: u64,
    ) -> Result<FiniteReader, FiniteVodError> {
        if file == CopyFile::Playlist {
            return Err(invalid());
        }
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
    /// Read an entire already-published copy object into the charged response
    /// partition. The original reader remains registered until its positive
    /// close, including when the caller's wait is cancelled.
    pub async fn read_all_bounded(
        &self,
        file: CopyFile,
        maximum_bytes: usize,
        deadline: Instant,
    ) -> Result<Option<FiniteResponseBuffer>, FiniteVodError> {
        if file == CopyFile::Playlist {
            return self.playlist_buffer(deadline).await;
        }
        let Some(length) = self.file_len(file).await? else {
            return Ok(None);
        };
        let length = usize::try_from(length).map_err(|_| invalid())?;
        if length > maximum_bytes {
            return Err(invalid());
        }
        let mut response = self.allocate_response_buffer(length, deadline).await?;
        let reader = self
            .prepare_reader(
                file,
                0,
                length as u64,
                u64::try_from(length.min(64 * 1024)).map_err(|_| invalid())?,
            )
            .await?;
        while response.len() < length {
            let chunk = reader.next_chunk(deadline).await?.ok_or_else(invalid)?;
            if chunk.as_ref().is_empty() || chunk.as_ref().len() > length - response.len() {
                return Err(invalid());
            }
            response
                .try_extend_from_slice(chunk.as_ref())
                .map_err(|_| invalid())?;
        }
        reader.close(deadline).await?;
        Ok(Some(response))
    }
    /// Serve the journal-known latest playlist without opening an old native
    /// inode while the next exact ReplacePlaylist revision is in flight. The
    /// returned buffer retains the response charge through its final Bytes alias.
    pub async fn playlist_buffer(
        &self,
        deadline: Instant,
    ) -> Result<Option<FiniteResponseBuffer>, FiniteVodError> {
        let mut buffer = self
            .allocate_response_buffer(self.playlist_capacity, deadline)
            .await?;
        let latest = self.served_playlist.lock().map_err(|_| invalid())?;
        if latest.is_empty() {
            return Ok(None);
        }
        buffer.try_extend_from_slice(latest.as_bytes())?;
        Ok(Some(buffer))
    }
    /// Producer progress is a separate charged snapshot. It has no HTTP
    /// visibility until the actor commits a served revision below.
    pub async fn produced_playlist_buffer(
        &self,
        deadline: Instant,
    ) -> Result<Option<FiniteResponseBuffer>, FiniteVodError> {
        let mut buffer = self
            .allocate_response_buffer(self.playlist_capacity, deadline)
            .await?;
        let latest = self.produced_playlist.lock().map_err(|_| invalid())?;
        if latest.is_empty() {
            return Ok(None);
        }
        buffer.try_extend_from_slice(latest.as_bytes())?;
        Ok(Some(buffer))
    }

    /// Publish exactly the actor's bounded contiguous window through the
    /// original ScratchAssetsV3 ReplacePlaylist cell. The actor supplies only
    /// indices: every byte is derived from the core's produced inventory.
    pub async fn commit_served_playlist(
        &self,
        first: u32,
        last: u32,
        deadline: Instant,
    ) -> Result<CopyServedRevision, FiniteVodError> {
        if first > last {
            return Err(invalid());
        }
        let _serial = self.playlist_commit.lock().await;
        // The std inventory guard is scoped to selection: it must not be
        // held across any later await of this Send future.
        let selected = {
            let produced = self.produced_playlist.lock().map_err(|_| invalid())?;
            if produced.is_empty() {
                return Err(invalid());
            }
            let mut selected = String::new();
            selected
                .try_reserve_exact(self.playlist_capacity)
                .map_err(|_| invalid())?;
            if selected.capacity() > self.playlist_capacity {
                return Err(invalid());
            }
            use std::fmt::Write as _;
            write!(&mut selected,
            "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:{first}\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXT-X-START:TIME-OFFSET=0\n",
            self.target_seconds).map_err(|_| invalid())?;
            let mut lines = produced.lines();
            let mut next = first;
            let mut produced_last = None;
            let mut end_list = false;
            while let Some(line) = lines.next() {
                if line == "#EXT-X-ENDLIST" {
                    end_list = true;
                    continue;
                }
                if !line.starts_with("#EXTINF:") {
                    continue;
                }
                let name = lines.next().ok_or_else(invalid)?;
                let Some(CopyFile::Segment(index)) = CopyFile::from_canonical_name(name) else {
                    return Err(invalid());
                };
                if produced_last.is_some_and(|prior: u32| index != prior.saturating_add(1)) {
                    return Err(invalid());
                }
                produced_last = Some(index);
                if index >= first && index <= last {
                    if index != next {
                        return Err(invalid());
                    }
                    let addition = line
                        .len()
                        .checked_add(name.len())
                        .and_then(|n| n.checked_add(2))
                        .ok_or_else(invalid)?;
                    if selected
                        .len()
                        .checked_add(addition)
                        .is_none_or(|n| n > self.playlist_capacity)
                    {
                        return Err(invalid());
                    }
                    selected.push_str(line);
                    selected.push('\n');
                    selected.push_str(name);
                    selected.push('\n');
                    next = next.checked_add(1).ok_or_else(invalid)?;
                }
            }
            if next != last.checked_add(1).ok_or_else(invalid)?
                || produced_last.is_none_or(|n| n < last)
            {
                return Err(invalid());
            }
            if end_list && produced_last == Some(last) {
                if selected
                    .len()
                    .checked_add("#EXT-X-ENDLIST\n".len())
                    .is_none_or(|n| n > self.playlist_capacity)
                {
                    return Err(invalid());
                }
                selected.push_str("#EXT-X-ENDLIST\n");
            }
            selected
        };
        if selected.len() > self.playlist_capacity {
            return Err(invalid());
        }
        self.runtime
            .validate_copy_sources(&self.preparation, 0)
            .await?;
        let mut bytes = selected.as_bytes().to_vec();
        let publication = self
            .runtime
            .queue_scratch_publication_wait(
                &self.preparation,
                0,
                self.member,
                OutputKey::Playlist,
                &mut bytes,
            )
            .await?;
        self.runtime
            .publish_scratch_output(&self.preparation, 0, self.member, publication, deadline)
            .await?;
        self.runtime
            .validate_copy_sources(&self.preparation, 0)
            .await?;
        let mut response = self
            .allocate_response_buffer(self.playlist_capacity, deadline)
            .await?;
        response.try_extend_from_slice(selected.as_bytes())?;
        Ok(CopyServedRevision {
            bytes: response,
            served_playlist: Arc::clone(&self.served_playlist),
        })
    }

    pub fn activate_served_playlist(
        &self,
        revision: CopyServedRevision,
    ) -> Result<FiniteResponseBuffer, FiniteVodError> {
        if !Arc::ptr_eq(&self.served_playlist, &revision.served_playlist) {
            return Err(invalid());
        }
        let text = std::str::from_utf8(revision.bytes.as_ref()).map_err(|_| invalid())?;
        let mut served = self.served_playlist.lock().map_err(|_| invalid())?;
        if text.len() > served.capacity() {
            return Err(invalid());
        }
        served.clear();
        served.push_str(text);
        drop(served);
        Ok(revision.bytes)
    }
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
    pub async fn wait_allocations_release(&self, deadline: Instant) -> Result<(), FiniteVodError> {
        self.runtime
            .wait_finite_allocations_release(&self.preparation, 0, deadline)
            .await
            .map_err(Into::into)
    }

    /// Release only an actor-expired segment through its original typed
    /// RemoveOwned operation. A live reader or uncertain native unlink keeps
    /// the file and the full rolling role charged for the same-operation retry.
    pub async fn retire_segment(
        &self,
        segment: u32,
        deadline: Instant,
    ) -> Result<(), FiniteVodError> {
        let _publication = self.playlist_commit.lock().await;
        self.runtime
            .remove_copy_segment(&self.preparation, 0, self.member, segment, deadline)
            .await
            .map_err(Into::into)
    }
}

impl CopyVodOwner {
    pub fn directory_path(&self) -> &Path {
        &self.directory_path
    }
    /// Stop and join the original parser/root while retaining the journaled
    /// read-only scratch inventory for the actor's promised grace window.
    /// This proves producer termination only; output closure/refund still
    /// requires `prepare_retirement` and its same-operation ticket.
    pub async fn stop_producer(
        &mut self,
        abort: bool,
    ) -> Result<CopyProductionResult, FiniteVodError> {
        let summary = self
            .runtime
            .join_copy_producer(&self.preparation, 0, abort)
            .await
            .map_err(FiniteVodError::from)?;
        Ok(CopyProductionResult::from_terminal(summary))
    }
    pub fn output(&self) -> CopyOutput {
        CopyOutput {
            runtime: Arc::clone(&self.runtime),
            preparation: self.preparation.clone(),
            member: self.scratch_member,
            directory_path: Arc::clone(&self.directory_path),
            produced_playlist: Arc::clone(&self.produced_playlist),
            served_playlist: Arc::clone(&self.served_playlist),
            playlist_commit: Arc::clone(&self.playlist_commit),
            playlist_capacity: self.playlist_capacity,
            target_seconds: self.target_seconds,
            _app_guard: Arc::clone(&self._app_guard),
        }
    }

    pub fn take_application_lease(
        &self,
        requested_bytes: usize,
    ) -> Result<super::super::finite_lifetime::FiniteApplicationLease, FiniteVodError> {
        self.app_state
            .take_application_lease(requested_bytes)
            .ok_or_else(invalid)
    }

    pub async fn prepare_inputs(&mut self, deadline: Instant) -> Result<(), FiniteVodError> {
        self.runtime
            .resolve_preparation(&self.preparation, deadline)
            .await?;
        for (member, spec) in self.source_members.iter().zip(&mut self.source_specs) {
            if spec.is_some() {
                self.runtime
                    .queue_source_open(&self.preparation, 0, *member, spec)?;
            }
        }
        self.runtime
            .prepare_copy_inputs(&self.preparation, 0, deadline)
            .await?;
        Ok(())
    }

    pub async fn start(&mut self, deadline: Instant) -> Result<(), FiniteVodError> {
        self.prepare_inputs(deadline).await?;
        self.runtime
            .arm_scratch_bootstrap(&self.preparation, 0, self.scratch_member, deadline)
            .await?;
        self.runtime
            .open_scratch_bootstrap_root(&self.preparation, 0, self.scratch_member, deadline)
            .await?;
        self.runtime
            .open_scratch_bootstrap_sink(&self.preparation, 0, self.scratch_member, deadline)
            .await?;
        self.runtime
            .start_copy_vod(&self.preparation, 0, deadline)
            .await?;
        Ok(())
    }

    /// Wait on the original registered parser completion slot. A cancelled
    /// waiter leaves the same parser JoinHandle and full role in the core
    /// book; only `prepare_retirement` can establish native closure.
    pub async fn wait_producer_result(
        &self,
        deadline: Instant,
    ) -> Result<CopyProductionResult, FiniteVodError> {
        let summary = self
            .runtime
            .wait_copy_parser_result(&self.preparation, 0, deadline)
            .await
            .map_err(FiniteVodError::from)?;
        Ok(CopyProductionResult::from_terminal(summary))
    }

    /// Stop the original root if requested, join parser/root/source jobs, and
    /// close native scratch under the full charge. The returned ticket still
    /// requires actual application and response alias destruction before one
    /// known journal settlement and refund.
    pub async fn prepare_retirement(
        &mut self,
        abort: bool,
        deadline: Instant,
    ) -> Result<CopyCloseTicket, FiniteVodError> {
        // Claim before the first await; cancellation queues the same original
        // close for repair. An already live ticket keeps its exclusive claim.
        let cleanup_claim = self
            .cleanup_custody
            .try_claim_owner()
            .ok_or(super::super::operations::OperationError::State)?;
        let accepted = tokio::time::timeout_at(
            deadline.into(),
            self.runtime
                .acknowledge_copy_preparation_for_cleanup(&self.preparation),
        )
        .await
        .map_err(|_| super::super::operations::OperationError::Deadline)??;
        if accepted {
            tokio::time::timeout_at(
                deadline.into(),
                self.runtime.prepare_copy_close(
                    &self.preparation,
                    &cleanup_claim,
                    0,
                    self.scratch_member,
                    abort,
                    deadline,
                ),
            )
            .await
            .map_err(|_| super::super::operations::OperationError::Deadline)??;
        } else {
            tokio::time::timeout_at(
                deadline.into(),
                self.runtime
                    .prepare_unaccepted_copy_close(&self.preparation, &cleanup_claim),
            )
            .await
            .map_err(|_| super::super::operations::OperationError::Deadline)??;
        }
        Ok(CopyCloseTicket {
            runtime: Arc::clone(&self.runtime),
            preparation: Some(self.preparation.clone()),
            app_state: Some(Arc::clone(&self.app_state)),
            cleanup_claim: Some(cleanup_claim),
        })
    }
}
impl Drop for CopyVodOwner {
    fn drop(&mut self) {
        // A cancelled manager start or lost session selector is never a
        // native close fact. The original charged role remains in the
        // runtime book for its ordinary same-operation repair pass.
        self.cleanup_custody.request_repair();
        self.progress.notify_waiters();
    }
}

impl NodeResourceRuntime {
    /// Pure configured output metadata for operator quota lookup before R.
    /// This opens nothing and grants no path-based native close authority.
    pub fn copy_vod_output_path(&self, rendition_id: &str) -> Result<PathBuf, FiniteVodError> {
        let root = self
            .rendition_root
            .as_ref()
            .and_then(|path| path.to_str())
            .ok_or_else(invalid)?;
        let (_, path) = finite_directory_path(root, rendition_id)?;
        Ok(path)
    }

    /// Requested capacity of the exact session path this copy role will
    /// construct. This is pure metadata sizing before native admission.
    pub fn copy_vod_application_path_capacity(
        &self,
        rendition_id: &str,
    ) -> Result<usize, FiniteVodError> {
        let root = self
            .rendition_root
            .as_ref()
            .and_then(|path| path.to_str())
            .ok_or_else(invalid)?;
        let (_, directory) = finite_directory_path(root, rendition_id)?;
        Ok(directory.capacity())
    }

    /// Project the exact named copy partitions before constructing a demand.
    /// The returned amount is the known/requested-capacity component; the
    /// producer's unmeasured native peak remains a separate memory claim.
    pub fn copy_vod_known_memory_bytes(
        &self,
        request: &CopyVodRequest,
    ) -> Result<u64, FiniteVodError> {
        let root = self
            .rendition_root
            .as_ref()
            .and_then(|path| path.to_str())
            .ok_or_else(invalid)?;
        let (_, directory) = finite_directory_path(root, &request.rendition_id)?;
        let binary_path = request.command.executable().path().to_owned();
        known_copy_memory(self, request, directory.capacity(), binary_path.capacity())
            .map(|(_, total)| total)
    }

    /// Admit the complete fixed copy inventory before acquiring any source,
    /// directory, child, pipe or parser task. An incompatible host/limit fails
    /// without issuing a new operation or opening a native resource.
    pub async fn prepare_copy_vod(
        self: &Arc<Self>,
        request: CopyVodRequest,
        deadline: Instant,
    ) -> Result<CopyVodOwner, FiniteVodError> {
        if !cfg!(target_os = "linux")
            || request.generation_epoch == 0
            || request.max_segments == 0
            || request.max_segments > 65_536
            || request.source.path != request.command.source().path
            || request.source.media_file_id != request.command.source().id.to_string()
            || request.source.scanner_size != request.command.source().size
            || request.source.scanner_mtime != request.command.source().mtime
            || request.source.object_version.is_empty()
            || request.source.object_version.len() > 4096
            || request.recipe_source_binding.is_empty()
            || request.recipe_source_binding.len() > 4096
            || request.operation_timeout.is_zero()
            || request.parser_limits.max_bytes == 0
            || request.parser_limits.max_bytes > COPY_VOD_MAX_SEGMENT_BYTES
            || request.reader_bounds.validate().is_err()
            || request.max_pending_fragments == 0
            || request.max_pending_fragments > 65_536
        {
            return Err(invalid_before_reservation());
        }
        let root = self
            .rendition_root
            .as_ref()
            .and_then(|p| p.to_str())
            .ok_or_else(invalid_before_reservation)?;
        let (protected_child, selected_directory_path) =
            finite_directory_path(root, &request.rendition_id)
                .map_err(|_| invalid_before_reservation())?;
        let components: Vec<String> = root
            .strip_prefix('/')
            .ok_or_else(invalid_before_reservation)?
            .split('/')
            .map(str::to_owned)
            .collect();
        if components.is_empty()
            || components.len() > 64
            || components.iter().any(|c| {
                c.is_empty()
                    || c.len() > 255
                    || matches!(c.as_str(), "." | "..")
                    || c.contains(['\\', '\0'])
            })
        {
            return Err(invalid_before_reservation());
        }
        let Metric::Known(resources) = request.demand.resources else {
            return Err(invalid_before_reservation());
        };
        let bounds = &request.output;
        let max_pending_bytes = request
            .parser_limits
            .max_bytes
            .checked_add(request.reader_bounds.unit_bytes)
            .ok_or_else(invalid_before_reservation)?;
        let media_path_capacity = request.source.path.capacity();
        let binary_path_for_source = request.command.executable().path().to_owned();
        let binary_path_projected = binary_path_for_source.capacity();
        let (minimum_task_memory, minimum_total_memory) = known_copy_memory(
            self,
            &request,
            selected_directory_path.capacity(),
            binary_path_projected,
        )
        .map_err(|_| invalid_before_reservation())?;
        if resources.processes != 1 || resources.cpu_ns_per_second == 0
            || resources.hardware_slots != 0 || resources.tuner_slots != 0
            || resources.handles < u64::from(bounds.live_handles).saturating_add(9)
            || resources.peak_memory_bytes < minimum_total_memory
            || resources.scratch_bytes < bounds.reserved_storage_bytes
            || request.demand.trial
            // The naming range bounds lifetime indices. The resident limit
            // bounds the charged rolling window, including an init, one old
            // and one new playlist, a live segment and its publication overlap.
            || bounds.resident_file_objects < 5
            || bounds.one_io_payload_bytes < u64::try_from(request.parser_limits.max_bytes).map_err(|_| invalid_before_reservation())?
        {
            return Err(invalid_before_reservation());
        }
        let playlist_capacity =
            CopySink::playlist_capacity_for(request.max_segments, request.parser_limits)
                .ok_or_else(invalid_before_reservation)?;
        if request.application_owner_bytes
            < u64::try_from(playlist_capacity)
                .map_err(|_| invalid_before_reservation())?
                .checked_mul(2)
                .ok_or_else(invalid_before_reservation)?
        {
            return Err(invalid_before_reservation());
        }
        if bounds.response_memory_bytes
            < FiniteResponseBuffer::required_partition_bytes(playlist_capacity, 2)
                .ok_or_else(invalid_before_reservation)?
        {
            return Err(invalid_before_reservation());
        }
        let namespace = std::fs::metadata("/proc/self/ns/mnt")
            .map_err(|e| {
                FiniteVodError::from(super::super::operations::OperationError::Physical(e))
            })?
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
        let task_member = id();
        let stdout_owner = id();
        let diagnostic_owner = id();
        let sink_owner = id();
        let task_buffer = id();
        let binary = request.command.executable();
        let binary_digest = binary.sha256();
        let binary_binding = executable_recipe_binding(&binary_digest);
        let binary_identity = binary.source_identity();
        let binary_source = super::FiniteSourceRequest {
            path: binary_path_for_source,
            media_file_id: binary_binding.clone(),
            object_version: binary_identity.render_s1(),
            scanner_size: i64::try_from(binary_identity.size)
                .map_err(|_| invalid_before_reservation())?,
            scanner_mtime: binary_identity.mtime,
        };
        let media_association = v2::SourceAssociation {
            media_file_id: request.source.media_file_id.clone(),
            object_version: request.source.object_version.clone(),
            recipe_source_binding: request.recipe_source_binding.clone(),
        };
        let source_requests = [
            (request.source, None, request.recipe_source_binding.clone()),
            (binary_source, Some(binary_digest), binary_binding.clone()),
        ];
        let mut plans = Vec::with_capacity(5);
        let mut source_members = [Uuid::nil(); 2];
        let mut source_specs: [Option<SourceOpenSpec>; 2] = [None, None];
        let slot = |member_id, protected_paths| v2::MemberSlot {
            member_id,
            owned_by: None,
            depends_on: vec![],
            protected_paths,
        };
        for (index, (source, expected_sha256, binding)) in source_requests.into_iter().enumerate() {
            let member = id();
            let source_copy = id();
            let protected_path = source
                .path
                .to_str()
                .ok_or_else(invalid_before_reservation)?
                .to_owned();
            plans.push(TypedMemberPlan::Source {
                slot: slot(member, vec![protected_path]),
                source: SourcePlan {
                    holder_id: id(),
                    source_association: v2::SourceAssociation {
                        media_file_id: source.media_file_id,
                        object_version: source.object_version,
                        recipe_source_binding: binding,
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
            source_members[index] = member.uuid();
            source_specs[index] = Some(SourceOpenSpec {
                path: source.path,
                scanner_size: source.scanner_size,
                scanner_mtime: source.scanner_mtime,
                expected_sha256,
            });
        }
        if source_specs[0]
            .as_ref()
            .is_none_or(|spec| spec.path.capacity() > media_path_capacity)
            || source_specs[1]
                .as_ref()
                .is_none_or(|spec| spec.path.capacity() > binary_path_projected)
        {
            return Err(invalid_before_reservation());
        }
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
                rendition_id: request.rendition_id.clone(),
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
                            .ok_or_else(invalid_before_reservation)?
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
                    segment_end_exclusive: u64::from(request.max_segments),
                    naming: OutputNamingV1::CopyRollingV1,
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
                .ok_or_else(invalid_before_reservation)?
                .to_owned(),
            recovery: if self.owner.platform_domain.continuity_provider.as_deref()
                == Some("linux_kernel_pid_namespace_v1")
                && matches!(&self.owner.os_boot, BootIdentity::Known { provider, .. }
                    if provider == "linux_kernel_boot_id")
            {
                self.owner.platform_domain.namespace.as_ref().map_or_else(
                    || RecoveryMethod::LiveOnly {
                        reason: "copy root namespace unavailable".into(),
                    },
                    |pid_namespace| RecoveryMethod::TrustedFixedSoftwareSameBootV1 {
                        pid_namespace: pid_namespace.clone(),
                        executable_binding: binary_binding.clone(),
                    },
                )
            } else {
                RecoveryMethod::LiveOnly {
                    reason: "copy root continuity unavailable".into(),
                }
            },
            owned_by: None,
            depends_on: vec![],
            maximum_cardinality: 1,
            protected_paths: vec![],
        }));
        plans.push(TypedMemberPlan::CopyOutputTask {
            slot: v2::MemberSlot {
                member_id: task_member,
                owned_by: Some(root_member),
                depends_on: vec![root_member, scratch_member],
                protected_paths: vec![],
            },
            task: super::super::operations::plans::VodTaskPlan {
                root_member_id: root_member,
                generation_epoch: request.generation_epoch,
                source_association: media_association,
                stdout_owner_id: stdout_owner,
                diagnostic_owner_id: diagnostic_owner,
                asset_id: asset,
                sink_owner_id: sink_owner,
                inventory: v2::IoInventory {
                    pending_io: vec![],
                    buffers: vec![v2::OwnerAllocation {
                        owner_id: task_buffer,
                        memory_bytes: u64::try_from(minimum_task_memory)
                            .map_err(|_| invalid_before_reservation())?,
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
        let preparation = match self.prepare_typed_families_claiming(
            &[FamilyDemand {
                identity: family,
                roles: &roles,
            }],
            vec![TypedGraphPlan {
                attempt_id: None,
                members: plans,
            }],
            Some(&[MemoryClaim::UnknownProducerPeak]),
            deadline,
        ) {
            Ok(preparation) => preparation,
            Err(error) => {
                let _ = self.remove_unused_family(family);
                return Err(error.into());
            }
        };
        // The typed R is charged before any copy parser, playlist, response
        // budget or application owner allocation. Register the original role
        // with no native entry so a failed allocation has a positive same-role
        // no-root cancellation path rather than an unowned reservation.
        let app_state = super::super::finite_lifetime::FiniteAppState::with_application_limit(
            usize::try_from(request.application_owner_bytes).map_err(|_| invalid())?,
        );
        let cleanup_custody = Arc::new(CopyCleanupCustody::new());
        let progress = Arc::new(tokio::sync::Notify::new());
        let parser_source = request.command.source().clone();
        let video = request.command.video();
        self.queue_copy_vod(
            &preparation,
            0,
            root_member.uuid(),
            task_member.uuid(),
            root_acquisition_id.uuid(),
            source_members,
            request.command,
            None,
            Arc::clone(&app_state),
            Arc::clone(&cleanup_custody),
            Arc::clone(&progress),
        )?;
        let pending_preparation = preparation.clone();
        let prepared = (move || -> Result<CopyVodOwner, FiniteVodError> {
            let prepared_sink = CopySink::prepare(
                request.max_segments,
                usize::try_from(bounds.one_io_payload_bytes).map_err(|_| invalid())?,
                request.parser_limits,
                request.operation_timeout,
                Arc::clone(&progress),
            )?;
            let response_budget = ResponseBudget::with_metadata(
                usize::try_from(bounds.response_memory_bytes).map_err(|_| invalid())?,
                usize::try_from(
                    bounds
                        .retained_metadata_bytes
                        .checked_add(bounds.client_state_bytes)
                        .ok_or_else(invalid)?,
                )
                .map_err(|_| invalid())?,
            )
            .map_err(FiniteVodError::from)?;
            let parser = PreparedCopyParser::new(
                request.reader_bounds,
                request.max_pending_fragments,
                max_pending_bytes,
            )?;
            let app_guard = app_state.new_guard().ok_or_else(invalid)?;
            let sink = prepared_sink.bind(
                Arc::clone(self),
                pending_preparation.clone(),
                scratch_member.uuid(),
            );
            let produced_playlist = sink.produced_playlist_handle();
            let served_playlist = sink.served_playlist_handle();
            let playlist_commit = sink.publication_gate_handle();
            let work = CopyWork {
                sink,
                source: parser_source,
                video,
                limits: request.parser_limits,
                diagnostic_id: request.rendition_id,
                parser,
            };
            let actual_task = work
                .known_memory_bytes()?
                .checked_add(
                    u64::try_from(copy_task_structural_bytes().ok_or_else(invalid)?)
                        .map_err(|_| invalid())?,
                )
                .ok_or_else(invalid)?;
            if actual_task > u64::try_from(minimum_task_memory).map_err(|_| invalid())? {
                return Err(invalid());
            }
            self.install_copy_work_and_response(
                &pending_preparation,
                0,
                work,
                response_budget,
                (&app_state, &cleanup_custody),
            )?;
            Ok(CopyVodOwner {
                runtime: Arc::clone(self),
                preparation: pending_preparation,
                source_members,
                source_specs,
                scratch_member: scratch_member.uuid(),
                directory_path: Arc::new(selected_directory_path),
                produced_playlist,
                served_playlist,
                playlist_commit,
                playlist_capacity,
                target_seconds: request.parser_limits.target_seconds.max(1),
                app_state: Arc::clone(&app_state),
                cleanup_custody,
                progress,
                _app_guard: app_guard,
            })
        })();
        match prepared {
            Ok(owner) => Ok(owner),
            Err(error) => {
                // The returned error is never itself evidence of non-entry.
                // Register maintenance selection before the first await;
                // cancellation or an unknown journal result keeps this SAME
                // original R available to the existing supervisor cadence.
                let cleanup_claim = self.mark_copy_admission_failed(&preparation, 0)?;
                let accepted = tokio::time::timeout_at(
                    deadline.into(),
                    self.acknowledge_copy_preparation_for_cleanup(&preparation),
                )
                .await
                .map_err(|_| super::super::operations::OperationError::Deadline)??;
                if accepted {
                    tokio::time::timeout_at(
                        deadline.into(),
                        self.prepare_copy_close(
                            &preparation,
                            &cleanup_claim,
                            0,
                            scratch_member.uuid(),
                            true,
                            deadline,
                        ),
                    )
                    .await
                    .map_err(|_| super::super::operations::OperationError::Deadline)??;
                } else {
                    tokio::time::timeout_at(
                        deadline.into(),
                        self.prepare_unaccepted_copy_close(&preparation, &cleanup_claim),
                    )
                    .await
                    .map_err(|_| super::super::operations::OperationError::Deadline)??;
                }
                let mut selection = Some(preparation);
                tokio::time::timeout_at(
                    deadline.into(),
                    self.finish_finite_close(&mut selection, 0, deadline),
                )
                .await
                .map_err(|_| super::super::operations::OperationError::Deadline)??;
                cleanup_claim.complete();
                Err(error)
            }
        }
    }
}

#[cfg(all(test, feature = "fixtures"))]
impl NodeResourceRuntime {
    pub(in crate::adaptive_resources::runtime) async fn fixture_actual_copy_request(
        root: &Path,
        rejected: bool,
    ) -> (Arc<NodeResourceRuntime>, CopyVodRequest) {
        cleanup_tests::actual_request(root, rejected).await
    }
    pub(in crate::adaptive_resources::runtime) fn fixture_copy_original_preparation(
        owner: &CopyVodOwner,
    ) -> OwnedPreparation {
        owner.preparation.clone()
    }
}

#[cfg(all(test, feature = "fixtures"))]
#[path = "tests/cleanup.rs"]
mod cleanup_tests;
