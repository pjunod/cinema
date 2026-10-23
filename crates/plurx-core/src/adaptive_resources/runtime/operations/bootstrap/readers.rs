//! Counted response readers. A response owns only an opaque selection/chunk;
//! the retained runtime keeps the original File, buffer and journal operation.
use super::*;
use native::readers::{OwnedReadChunk, ReaderCell};
use std::sync::atomic::{AtomicBool, Ordering};
use v2::scratch::{
    AssetIoV1, AssetObservationV1, ByteRangeV1, NativeOutcomeV1, OutputKey, ReaderLeaseV1,
};

pub struct FiniteReadChunk(OwnedReadChunk);
impl AsRef<[u8]> for FiniteReadChunk {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

struct ReaderControl {
    closing: AtomicBool,
    terminal: AtomicBool,
    ready: Arc<tokio::sync::Notify>,
}

/// Drop requests closure; it never reports a closed File or returns credit.
/// Normal body completion calls close. Later serving/eviction also drains
/// requested closes through the same retained entries, without a watchdog.
pub struct FiniteReader {
    runtime: Arc<NodeResourceRuntime>,
    preparation: OwnedPreparation,
    index: usize,
    lease: Uuid,
    control: Arc<ReaderControl>,
}
impl Drop for FiniteReader {
    fn drop(&mut self) {
        self.control.closing.store(true, Ordering::Release);
        self.control.ready.notify_one();
    }
}
impl FiniteReader {
    pub async fn next_chunk(
        &self,
        deadline: Instant,
    ) -> Result<Option<FiniteReadChunk>, super::super::super::finite_vod::FiniteVodError> {
        if self.control.terminal.load(Ordering::Acquire) {
            return Ok(None);
        }
        self.runtime
            .finite_reader_step(&self.preparation, self.index, self.lease, false, deadline)
            .await
            .map_err(Into::into)
    }
    pub async fn close(
        &self,
        deadline: Instant,
    ) -> Result<(), super::super::super::finite_vod::FiniteVodError> {
        if self.control.terminal.load(Ordering::Acquire) {
            return Ok(());
        }
        self.control.closing.store(true, Ordering::Release);
        self.runtime
            .finite_reader_step(&self.preparation, self.index, self.lease, true, deadline)
            .await?;
        Ok(())
    }
}

#[derive(Clone)]
struct ReaderIo {
    operation: AssetIoV1,
    io: v2::Id,
    observe: v2::Id,
    join: v2::Id,
    armed: bool,
    launched: bool,
    skipped: bool,
    joined: bool,
}
impl ReaderIo {
    fn new(operation: AssetIoV1, io: v2::Id) -> Result<Self, OperationError> {
        Ok(Self {
            operation,
            io,
            observe: id()?,
            join: id()?,
            armed: false,
            launched: false,
            skipped: false,
            joined: false,
        })
    }
}
pub(super) struct ReaderEntry {
    cell: Option<ReaderCell>,
    current: Option<ReaderIo>,
    control: Arc<ReaderControl>,
    opened: bool,
    terminal: bool,
}
/// Retained retirement may discard only fully acknowledged terminal rows.
/// A live response chunk keeps its original ReaderCell/buffer and refuses this.
pub(super) fn clear_terminal_readers(
    bootstrap: &mut BootstrapMember,
) -> Result<(), OperationError> {
    if bootstrap.readers.values().any(|entry| {
        !entry.terminal
            || entry.cell.is_some()
            || entry.current.is_some()
            || !entry.control.terminal.load(Ordering::Acquire)
    }) {
        return Err(OperationError::State);
    }
    bootstrap.readers.clear();
    Ok(())
}

fn id() -> Result<v2::Id, OperationError> {
    Ok(v2::Id::new(Uuid::new_v4())?)
}

enum ReaderProgress {
    Continue,
    Wait(Arc<tokio::sync::Notify>),
    Chunk(FiniteReadChunk),
    Closed,
}

pub(in crate::adaptive_resources::runtime::operations) fn reader_progress(
    bootstrap: &BootstrapMember,
    lease: Uuid,
) -> Option<Arc<tokio::sync::Notify>> {
    bootstrap
        .readers
        .get(&lease)
        .map(|entry| Arc::clone(&entry.control.ready))
}

pub(in crate::adaptive_resources::runtime::operations) fn notify_progress(
    bootstrap: &BootstrapMember,
) {
    for entry in bootstrap.readers.values() {
        entry.control.ready.notify_one();
    }
}

pub(super) fn transfer_readers_match(
    bootstrap: &BootstrapMember,
    expected: &[ReaderLeaseV1],
) -> bool {
    let active: Vec<_> = bootstrap
        .readers
        .values()
        .filter(|entry| !entry.terminal)
        .collect();
    active.len() == expected.len()
        && active.iter().all(|entry| {
            entry.opened
                && entry.current.is_none()
                && entry.cell.as_ref().is_some_and(|cell| {
                    let mut lease = cell.lease().clone();
                    lease.cursor = cell.cursor();
                    !cell.has_pending_native() && expected.contains(&lease)
                })
        })
}

impl NodeResourceRuntime {
    /// Metadata lookup only; it does not open a directory or file.
    pub(in crate::adaptive_resources::runtime) async fn finite_file_len(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        requested_member: Uuid,
        output: &OutputKey,
    ) -> Result<Option<u64>, OperationError> {
        self.validate_preparation(owner)?;
        let bundle = owner.state.lock().await;
        let role = bundle.roles.get(index).ok_or(OperationError::State)?;
        let member = resolve_member(role, requested_member)?;
        let bootstrap = role.bootstrap.get(&member).ok_or(OperationError::State)?;
        Ok(bootstrap
            .fold
            .as_ref()
            .ok_or(OperationError::State)?
            .files()
            .iter()
            .find(|file| !file.temporary && file.published.as_ref() == Some(output))
            .map(|file| file.logical_len))
    }

    /// Return the response selection before any native open. Cancellation after
    /// registration can only leave this same arm/cell in runtime custody.
    pub(in crate::adaptive_resources::runtime) async fn prepare_finite_reader(
        self: &Arc<Self>,
        owner: &OwnedPreparation,
        index: usize,
        requested_member: Uuid,
        output: OutputKey,
        start: u64,
        end_exclusive: u64,
        memory_cap: u64,
    ) -> Result<FiniteReader, OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        self.eligible(&bundle)?;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        let member = resolve_member(role, requested_member)?;
        if role.refunded || role.transition.is_some() {
            return Err(OperationError::State);
        }
        let bootstrap = role
            .bootstrap
            .get_mut(&member)
            .ok_or(OperationError::State)?;
        bootstrap.readers.retain(|_, reader| !reader.terminal);
        let fold = bootstrap.fold.as_ref().ok_or(OperationError::State)?;
        let state = fold.state();
        if bootstrap.change.is_some()
            || state.asset.totals.pending_jobs != 0
            || !matches!(
                state.asset.lifecycle,
                v2::scratch::ScratchLifecycleV1::Open
                    | v2::scratch::ScratchLifecycleV1::RetainedReadOnly
            )
            || memory_cap == 0
            || start > end_exclusive
            || bootstrap.readers.len() >= bootstrap.plan.limits.reader_leases as usize
            || state
                .asset
                .totals
                .memory_reserved_bytes
                .checked_add(memory_cap)
                .is_none_or(|total| total > bootstrap.plan.limits.resident_memory_bytes)
            || state
                .asset
                .totals
                .handles_reserved
                .checked_add(1)
                .is_none_or(|total| total > bootstrap.plan.limits.live_handles)
        {
            return Err(OperationError::State);
        }
        let file = fold
            .files()
            .iter()
            .find(|file| !file.temporary && file.published.as_ref() == Some(&output))
            .ok_or(OperationError::State)?;
        if end_exclusive > file.logical_len {
            return Err(OperationError::State);
        }
        let io = id()?;
        let lease = ReaderLeaseV1 {
            lease_id: io,
            asset_id: bootstrap.plan.asset_id,
            file_id: file.file_id,
            copy_id: id()?,
            owner_id: id()?,
            memory_cap,
            read_range: ByteRangeV1 {
                start,
                end_exclusive,
            },
            cursor: start,
            acquisition_io_id: io,
        };
        // This allocation consumes the already reserved output allowance. The
        // native worker remains unopened until its exact durable arm commits.
        let cell = ReaderCell::new(
            lease.clone(),
            file,
            state
                .asset
                .directory
                .as_ref()
                .ok_or(OperationError::State)?,
        )?;
        let current = ReaderIo::new(AssetIoV1::OpenReader { lease }, io)?;
        let control = Arc::new(ReaderControl {
            closing: AtomicBool::new(false),
            terminal: AtomicBool::new(false),
            ready: Arc::new(tokio::sync::Notify::new()),
        });
        bootstrap.readers.insert(
            io.uuid(),
            ReaderEntry {
                cell: Some(cell),
                current: Some(current),
                control: Arc::clone(&control),
                opened: false,
                terminal: false,
            },
        );
        Ok(FiniteReader {
            runtime: Arc::clone(self),
            preparation: owner.clone(),
            index,
            lease: io.uuid(),
            control,
        })
    }

    /// Existing eviction/serving work resumes dropped bodies in place. A body
    /// drop alone never removes a lease or retries an uncertain numeric close.
    pub(in crate::adaptive_resources::runtime) async fn close_dropped_finite_readers(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let leases = {
            let bundle = owner.state.lock().await;
            let role = bundle.roles.get(index).ok_or(OperationError::State)?;
            role.bootstrap
                .values()
                .flat_map(|bootstrap| bootstrap.readers.iter())
                .filter(|(_, entry)| {
                    entry.control.closing.load(Ordering::Acquire) && !entry.terminal
                })
                .map(|(lease, _)| *lease)
                .collect::<Vec<_>>()
        };
        for lease in leases {
            self.finite_reader_step(owner, index, lease, true, deadline)
                .await?;
        }
        Ok(())
    }

    async fn finite_reader_step(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        lease: Uuid,
        closing: bool,
        deadline: Instant,
    ) -> Result<Option<FiniteReadChunk>, OperationError> {
        self.validate_preparation(owner)?;
        loop {
            if Instant::now() >= deadline {
                return Err(OperationError::Deadline);
            }
            let progress = {
                let mut bundle = owner.state.lock().await;
                let bundle_id = bundle.id;
                let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
                let member = role.bootstrap.iter().find_map(|(member, bootstrap)| {
                    bootstrap.readers.contains_key(&lease).then_some(*member)
                });
                if let Some(member) = member {
                    self.drive_reader(role, bundle_id, member, lease, closing, deadline)
                        .await?
                } else if let Some(signal) = role
                    .retirement
                    .as_ref()
                    .and_then(|retirement| retirement.reader_progress(lease))
                {
                    // The same reader is in provisional S under full R while
                    // the original retirement command awaits known COMMIT.
                    ReaderProgress::Wait(signal)
                } else {
                    return Err(OperationError::State);
                }
            };
            match progress {
                ReaderProgress::Continue => {}
                ReaderProgress::Closed => return Ok(None),
                ReaderProgress::Chunk(chunk) if !closing => return Ok(Some(chunk)),
                ReaderProgress::Chunk(chunk) => drop(chunk),
                ReaderProgress::Wait(notification) => {
                    tokio::time::timeout_at(
                        tokio::time::Instant::from_std(deadline),
                        notification.notified(),
                    )
                    .await
                    .map_err(|_| OperationError::Deadline)?;
                }
            }
        }
    }

    async fn drive_reader(
        &self,
        role: &mut OwnedRole,
        bundle_id: Uuid,
        member: Uuid,
        lease: Uuid,
        closing: bool,
        deadline: Instant,
    ) -> Result<ReaderProgress, OperationError> {
        let bootstrap = role
            .bootstrap
            .get_mut(&member)
            .ok_or(OperationError::State)?;
        let entry = bootstrap
            .readers
            .get_mut(&lease)
            .ok_or(OperationError::State)?;
        if entry.terminal {
            return Ok(ReaderProgress::Closed);
        }
        if closing {
            entry.control.closing.store(true, Ordering::Release);
        }
        let stop = entry.control.closing.load(Ordering::Acquire);
        if entry.current.is_none() {
            let cell = entry.cell.as_mut().ok_or(OperationError::State)?;
            if !cell.buffer_returned() {
                return Ok(ReaderProgress::Wait(cell.recycle_notification()));
            }
            if stop || cell.cursor() == cell.lease().read_range.end_exclusive {
                entry.control.closing.store(true, Ordering::Release);
                entry.current = Some(ReaderIo::new(
                    AssetIoV1::CloseReader {
                        lease_id: cell.lease_id(),
                    },
                    id()?,
                )?);
            } else {
                let range = ByteRangeV1 {
                    start: cell.cursor(),
                    end_exclusive: cell
                        .cursor()
                        .saturating_add(cell.fixed_capacity() as u64)
                        .min(cell.lease().read_range.end_exclusive),
                };
                entry.current = Some(ReaderIo::new(
                    AssetIoV1::ReadRange {
                        lease_id: cell.lease_id(),
                        range,
                    },
                    id()?,
                )?);
            }
        }
        let io = entry.current.as_ref().ok_or(OperationError::State)?.clone();
        let ready = Arc::clone(&entry.control.ready);
        if role.transition.as_ref().is_some_and(|transition| {
            transition.operation.id != io.io.uuid() && transition.operation.id != io.observe.uuid()
        }) {
            return Ok(ReaderProgress::Wait(ready));
        }
        if !io.armed {
            if role.transition.is_none() {
                native::publication::queue_output_change(
                    &self.journal,
                    role,
                    member,
                    io.io.uuid(),
                    AssetEventV1::ArmIo {
                        io_id: io.io,
                        operation: io.operation.clone(),
                    },
                    deadline,
                )?;
            }
            retry_retained_transition(role, &self.journal, deadline)?;
            self.finish_transition(bundle_id, role).await?;
            role.bootstrap
                .get_mut(&member)
                .expect("retained member")
                .readers
                .get_mut(&lease)
                .expect("retained reader")
                .current
                .as_mut()
                .expect("original IO")
                .armed = true;
            return Ok(ReaderProgress::Continue);
        }
        if !io.launched {
            let bootstrap = role
                .bootstrap
                .get_mut(&member)
                .ok_or(OperationError::State)?;
            let entry = bootstrap
                .readers
                .get_mut(&lease)
                .ok_or(OperationError::State)?;
            if stop && matches!(io.operation, AssetIoV1::OpenReader { .. }) {
                let pending = entry.current.as_mut().expect("original IO");
                pending.joined = true;
                pending.launched = true;
                pending.skipped = true;
            } else {
                let cell = entry.cell.as_mut().ok_or(OperationError::State)?;
                let enter = match &io.operation {
                    AssetIoV1::OpenReader { .. } => cell.launch_open(
                        bootstrap
                            .native_root
                            .as_mut()
                            .ok_or(OperationError::State)?,
                    )?,
                    AssetIoV1::ReadRange { range, .. } => {
                        cell.launch_read(io.io, io.join, range.clone())?
                    }
                    AssetIoV1::CloseReader { .. } => cell.launch_close(io.io, io.join)?,
                    _ => return Err(OperationError::State),
                };
                entry.current.as_mut().expect("original IO").launched = true;
                if matches!(io.operation, AssetIoV1::OpenReader { .. }) {
                    // Recheck expansion under the same ledger after known Arm.
                    // Refusal drops only the unopened latch; join records it.
                    let _ = self.release_reader_open_entry(role.binding, enter);
                } else {
                    enter.send(()).map_err(|_| OperationError::State)?;
                }
            }
            return Ok(ReaderProgress::Continue);
        }
        if !io.joined {
            let bootstrap = role
                .bootstrap
                .get_mut(&member)
                .ok_or(OperationError::State)?;
            let cell = bootstrap
                .readers
                .get_mut(&lease)
                .and_then(|entry| entry.cell.as_mut())
                .ok_or(OperationError::State)?;
            match io.operation {
                AssetIoV1::OpenReader { .. } => {
                    cell.join_open(
                        bootstrap
                            .native_root
                            .as_mut()
                            .ok_or(OperationError::State)?,
                    )
                    .await?
                }
                AssetIoV1::ReadRange { .. } => cell.join_read().await?,
                AssetIoV1::CloseReader { .. } => cell.join_close().await?,
                _ => return Err(OperationError::State),
            }
            bootstrap
                .readers
                .get_mut(&lease)
                .expect("retained reader")
                .current
                .as_mut()
                .expect("original IO")
                .joined = true;
            return Ok(ReaderProgress::Continue);
        }
        let bootstrap = role
            .bootstrap
            .get_mut(&member)
            .ok_or(OperationError::State)?;
        let entry = bootstrap
            .readers
            .get_mut(&lease)
            .ok_or(OperationError::State)?;
        let cell = entry.cell.as_ref().ok_or(OperationError::State)?;
        let (outcome, observation, rejected) = match &io.operation {
            AssetIoV1::OpenReader { lease: planned } => {
                if let Some(native_object) = cell.open_fact() {
                    (
                        NativeOutcomeV1::Completed,
                        AssetObservationV1::ReaderOpened {
                            lease: planned.clone(),
                            native_object,
                        },
                        false,
                    )
                } else if let Some(errno) = cell.open_rejection_errno() {
                    (
                        NativeOutcomeV1::Failed {
                            error_class: "retained file open rejected".into(),
                        },
                        AssetObservationV1::ReaderOpenRejected {
                            lease_id: planned.lease_id,
                            errno,
                        },
                        true,
                    )
                } else if io.skipped || cell.open_never_entered() {
                    (
                        NativeOutcomeV1::NeverEntered,
                        AssetObservationV1::NoAcquisition,
                        true,
                    )
                } else {
                    return Err(OperationError::State);
                }
            }
            AssetIoV1::ReadRange { lease_id, .. } => {
                let fact = cell.read_fact().ok_or(OperationError::State)?;
                if fact.io_id != io.io || fact.join_id != io.join {
                    return Err(OperationError::State);
                }
                (
                    fact.outcome,
                    AssetObservationV1::ReaderRead {
                        lease_id: *lease_id,
                        bytes_read: fact.bytes_read,
                    },
                    false,
                )
            }
            AssetIoV1::CloseReader { lease_id } => {
                let (actual, join, outcome) = cell.close_fact().ok_or(OperationError::State)?;
                if actual != io.io || join != io.join {
                    return Err(OperationError::State);
                }
                (
                    outcome,
                    AssetObservationV1::ReaderClosed {
                        lease_id: *lease_id,
                    },
                    false,
                )
            }
            _ => return Err(OperationError::State),
        };
        let failed_read = matches!(&io.operation, AssetIoV1::ReadRange { range, .. } if
            !matches!(outcome, NativeOutcomeV1::Completed) || matches!(&observation, AssetObservationV1::ReaderRead { bytes_read: 0, .. } if range.start < range.end_exclusive));
        let rejected_open_errno = rejected.then(|| cell.open_rejection_errno()).flatten();
        if role.transition.is_none() {
            native::publication::queue_output_change(
                &self.journal,
                role,
                member,
                io.observe.uuid(),
                AssetEventV1::ObserveIo {
                    io_id: io.io,
                    native_join_operation_id: io.join,
                    outcome,
                    observation,
                },
                deadline,
            )?;
        }
        retry_retained_transition(role, &self.journal, deadline)?;
        self.finish_transition(bundle_id, role).await?;
        let entry = role
            .bootstrap
            .get_mut(&member)
            .expect("retained member")
            .readers
            .get_mut(&lease)
            .expect("retained reader");
        let cell = entry.cell.as_mut().expect("original reader cell");
        let mut failed_read_error = None;
        let result = match io.operation {
            AssetIoV1::OpenReader { .. } if rejected => {
                entry.cell = None;
                entry.terminal = true;
                ReaderProgress::Closed
            }
            AssetIoV1::OpenReader { .. } => {
                cell.acknowledge_open()?;
                entry.opened = true;
                ReaderProgress::Continue
            }
            AssetIoV1::ReadRange { .. } => {
                cell.acknowledge_read(io.io)?;
                let failure = cell.read_fact().and_then(|fact| fact.failure);
                let original_error = cell.take_acknowledged_read_error();
                let chunk = cell.take_acknowledged_chunk()?;
                if failed_read {
                    drop(chunk);
                    entry.control.closing.store(true, Ordering::Release);
                    failed_read_error = Some(original_error.unwrap_or_else(|| {
                        failure.map_or_else(
                            || {
                                std::io::Error::new(
                                    std::io::ErrorKind::UnexpectedEof,
                                    "retained file reached early EOF",
                                )
                            },
                            |native| {
                                native.errno.map_or_else(
                                    || {
                                        std::io::Error::new(
                                            native.kind,
                                            "retained file read failed",
                                        )
                                    },
                                    std::io::Error::from_raw_os_error,
                                )
                            },
                        )
                    }));
                    ReaderProgress::Continue
                } else {
                    ReaderProgress::Chunk(FiniteReadChunk(chunk))
                }
            }
            AssetIoV1::CloseReader { .. } => {
                cell.acknowledge_close(io.io)?;
                entry.cell = None;
                entry.terminal = true;
                ReaderProgress::Closed
            }
            _ => return Err(OperationError::State),
        };
        entry.current = None;
        entry
            .control
            .terminal
            .store(entry.terminal, Ordering::Release);
        entry.control.ready.notify_one();
        if let Some(errno) = rejected_open_errno {
            return Err(OperationError::Physical(std::io::Error::from_raw_os_error(
                errno,
            )));
        }
        if let Some(error) = failed_read_error {
            return Err(OperationError::Physical(error));
        }
        Ok(result)
    }

    fn release_reader_open_entry(
        &self,
        binding: ReservationBinding,
        enter: tokio::sync::oneshot::Sender<()>,
    ) -> Result<(), OperationError> {
        let state = self
            .ledger
            .inner
            .lock()
            .map_err(|_| OperationError::State)?;
        let entry = state
            .entries
            .get(&binding.reservation)
            .ok_or(OperationError::State)?;
        if entry.binding != binding
            || entry.durable.is_none()
            || state.invalid
            || state.draining
            || state.native_overage_fenced
            || state.epoch != binding.allocation_epoch
            || entry.state == ReservationState::UnknownHeld
        {
            return Err(OperationError::State);
        }
        enter.send(()).map_err(|_| OperationError::State)
    }
}
fn retry_retained_transition(
    role: &mut OwnedRole,
    journal: &CustodyJournal,
    deadline: Instant,
) -> Result<(), OperationError> {
    let transition = role.transition.as_mut().ok_or(OperationError::State)?;
    if transition.operation.pending.is_none() && matches!(transition.operation.result, Some(Err(_)))
    {
        transition.operation.retry(journal, deadline)?;
    }
    Ok(())
}
fn resolve_member(role: &OwnedRole, requested: Uuid) -> Result<Uuid, OperationError> {
    if role.bootstrap.contains_key(&requested) {
        return Ok(requested);
    }
    role.retirement
        .as_ref()
        .and_then(|retirement| retirement.resolve_source_member(requested))
        .ok_or(OperationError::State)
}
