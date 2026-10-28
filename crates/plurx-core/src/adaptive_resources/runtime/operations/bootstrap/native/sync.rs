//! Bounded, prearmed sync over the SAME held child and exact file inventory.
use super::publication::queue_output_change;
use super::*;
use crate::fs_secure::{
    sync_files::{SyncFile, SyncIo, SyncRequest},
    FileIdentity,
};
use v2::scratch::{AssetEventV1, AssetIoV1, AssetObservationV1, NativeOutcomeV1, OutputKey};

pub(in crate::adaptive_resources::runtime::operations) struct Sync {
    io: v2::Id,
    observe: v2::Id,
    join: v2::Id,
    files: Vec<v2::Id>,
    request: Option<SyncRequest>,
    pending: Option<JoinHandle<SyncResult>>,
    result: Option<SyncResult>,
    failed_join: bool,
    pub(super) acknowledged: bool,
}
struct SyncResult {
    native: SyncIo,
    result: std::io::Result<()>,
}
impl Sync {
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn retained_heap_bytes(
        &self,
    ) -> Result<usize, OperationError> {
        if !self.ready_for_transfer() || self.request.is_some() {
            return Err(OperationError::State);
        }
        self.files
            .capacity()
            .checked_mul(std::mem::size_of::<v2::Id>())
            .and_then(|bytes| {
                bytes.checked_add(self.result.as_ref()?.native.retained_heap_bytes()?)
            })
            .ok_or(OperationError::State)
    }
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn ready_for_transfer(
        &self,
    ) -> bool {
        self.acknowledged
            && self.pending.is_none()
            && !self.failed_join
            && self.result.as_ref().is_some_and(|result| {
                result.result.is_ok()
                    && result.native.entered()
                    && result.native.completed()
                    && result.native.current_file().is_none()
                    && !result.native.has_held_directory()
            })
    }

    fn launch(
        &mut self,
        root: &mut RootCell,
        directory: &v2::scratch::DirectoryObservedV1,
    ) -> Result<tokio::sync::oneshot::Sender<()>, OperationError> {
        if self.request.is_none()
            || self.pending.is_some()
            || self.result.is_some()
            || self.failed_join
            || root.pending.is_some()
            || root.failed_join
            || directory.held_copy_ids.len() != 1
        {
            return Err(OperationError::State);
        }
        let copy = directory.held_copy_ids[0];
        let held = root.result.as_mut().ok_or(OperationError::State)?;
        if held.audit.acquired.as_ref() != Some(directory)
            || held.audit.outcome != NativeOutcomeV1::Completed
            || !held.files.contains_key(&copy)
        {
            return Err(OperationError::State);
        }
        let runtime = tokio::runtime::Handle::try_current().map_err(std::io::Error::other)?;
        let request = self
            .request
            .take()
            .expect("validated retained sync request");
        let directory = held.files.remove(&copy).expect("validated same child");
        let mut native = SyncIo::from_held_directory(request, directory);
        let (enter, registered) = tokio::sync::oneshot::channel();
        self.pending = Some(runtime.spawn_blocking(move || {
            let result = match registered.blocking_recv() {
                Ok(()) => native.run(),
                Err(_) => Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "sync native entry was never released",
                )),
            };
            SyncResult { native, result }
        }));
        Ok(enter)
    }
    /// Await only this cell's original registered worker. Its native object
    /// and I/O result stay cached for the ordinary Observe path.
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) async fn join_registered(
        &mut self,
    ) -> Result<(), OperationError> {
        if self.failed_join {
            return Err(OperationError::State);
        }
        if self.pending.is_some() {
            self.join().await?;
        }
        Ok(())
    }

    async fn join(&mut self) -> Result<(), OperationError> {
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
        self.result.as_ref().ok_or(OperationError::State)?;
        Ok(())
    }
}
fn new_id() -> Result<v2::Id, OperationError> {
    Ok(v2::Id::new(Uuid::new_v4())?)
}
fn name(key: &OutputKey) -> String {
    match key {
        OutputKey::Init => "init.mp4".into(),
        OutputKey::Segment { index } => format!("seg{index:05}.m4s"),
        OutputKey::Playlist => "index.m3u8".into(),
        OutputKey::IdentityMetadata => "identity.json".into(),
        OutputKey::EncodedProcessMetadata => "encoded-process".into(),
        OutputKey::CompleteArtifactManifest => "complete-v1.json".into(),
    }
}
impl NodeResourceRuntime {
    /// Select only journal-observed actual outputs. No caller identity/proof is accepted.
    pub(crate) fn queue_scratch_sync(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        outputs: &[OutputKey],
    ) -> Result<Uuid, OperationError> {
        self.validate_preparation(owner)?;
        if outputs.is_empty()
            || outputs.len() > 16
            || outputs.iter().collect::<BTreeSet<_>>().len() != outputs.len()
        {
            return Err(OperationError::State);
        }
        let mut bundle = owner.state.try_lock().map_err(|_| OperationError::State)?;
        self.queue_scratch_sync_locked(&mut bundle, index, member, outputs)
    }

    /// Wait for the SAME bundle lock before registering sync against observed
    /// outputs. The caller retains its output selection across the wait.
    pub(in crate::adaptive_resources::runtime) async fn queue_scratch_sync_wait(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        outputs: &[OutputKey],
    ) -> Result<Uuid, OperationError> {
        self.validate_preparation(owner)?;
        if outputs.is_empty()
            || outputs.len() > 16
            || outputs.iter().collect::<BTreeSet<_>>().len() != outputs.len()
        {
            return Err(OperationError::State);
        }
        let mut bundle = owner.state.lock().await;
        self.queue_scratch_sync_locked(&mut bundle, index, member, outputs)
    }

    fn queue_scratch_sync_locked(
        &self,
        bundle: &mut Bundle,
        index: usize,
        member: Uuid,
        outputs: &[OutputKey],
    ) -> Result<Uuid, OperationError> {
        if !bundle.installed {
            return Err(OperationError::State);
        }
        self.eligible(bundle)?;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        if role.refunded
            || role.transition.is_some()
            || !matches!(role.phase, Phase::AcquisitionPossible | Phase::Sealed)
        {
            return Err(OperationError::State);
        }
        let bootstrap = role
            .bootstrap
            .get_mut(&member)
            .ok_or(OperationError::State)?;
        let fold = bootstrap.fold.as_ref().ok_or(OperationError::State)?;
        let state = fold.state();
        if state.bootstrap.phase != BootstrapPhase::SinkReady
            || state.asset.lifecycle != v2::scratch::ScratchLifecycleV1::Open
            || state.asset.totals.pending_jobs != 0
            || bootstrap.change.is_some()
            || bootstrap.next_arm.is_some()
            || !bootstrap.native_io_ready()
        {
            return Err(OperationError::State);
        }
        let io = new_id()?;
        let mut files = Vec::with_capacity(outputs.len());
        let mut ids = Vec::with_capacity(outputs.len());
        for output in outputs {
            let file = fold
                .files()
                .iter()
                .find(|f| {
                    f.published.as_ref() == Some(output)
                        && !f.temporary
                        && f.held_copy_ids.is_empty()
                })
                .ok_or(OperationError::State)?;
            let (
                v2::NativeValue::Known { value: device },
                v2::NativeValue::Known { value: inode },
                v2::NativeValue::Known { value: mount },
            ) = (
                &file.native_object.device,
                &file.native_object.inode,
                &file.native_object.mount_id,
            )
            else {
                return Err(OperationError::State);
            };
            files.push(SyncFile::new(
                &name(output),
                FileIdentity {
                    device: *device,
                    inode: *inode,
                    size: file.logical_len,
                    changed_seconds: 0,
                    changed_nanoseconds: 0,
                },
                Some(*mount),
            )?);
            ids.push(file.file_id);
        }
        let request = SyncRequest::new(io.uuid(), files)?;
        let memory = request
            .resident_memory_bytes()?
            .checked_add(std::mem::size_of::<Sync>())
            .and_then(|n| n.checked_add(std::mem::size_of::<SyncResult>()))
            .and_then(|n| n.checked_add(ids.capacity().checked_mul(std::mem::size_of::<v2::Id>())?))
            .and_then(|n| u64::try_from(n).ok())
            .ok_or(OperationError::State)?;
        if state
            .asset
            .totals
            .memory_reserved_bytes
            .checked_add(memory)
            .is_none_or(|n| n > bootstrap.plan.limits.resident_memory_bytes)
            || state
                .asset
                .totals
                .handles_reserved
                .checked_add(2)
                .is_none_or(|n| n > bootstrap.plan.limits.live_handles)
        {
            return Err(OperationError::State);
        }
        let observe = new_id()?;
        let join = new_id()?;
        bootstrap.sync_registered = true;
        bootstrap.sync = Some(Sync {
            io,
            observe,
            join,
            files: ids,
            request: Some(request),
            pending: None,
            result: None,
            failed_join: false,
            acknowledged: false,
        });
        Ok(io.uuid())
    }
    pub(crate) async fn sync_scratch_outputs(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        sync_id: Uuid,
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
            .and_then(|r| r.bootstrap.get(&member))
            .and_then(|b| b.sync.as_ref())
            .is_some_and(|s| s.request.is_some());
        if needs_entry {
            self.eligible(&bundle)?;
        }
        let bundle_id = bundle.id;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        let sync = role
            .bootstrap
            .get(&member)
            .and_then(|b| b.sync.as_ref())
            .ok_or(OperationError::State)?;
        if sync.io.uuid() != sync_id || role.refunded {
            return Err(OperationError::State);
        }
        if sync.acknowledged {
            return Ok(());
        }
        resume_change(&self.journal, role, member, deadline)?;
        if role.transition.is_some() {
            self.finish_transition(bundle_id, role).await?;
        }
        let bootstrap = role.bootstrap.get(&member).ok_or(OperationError::State)?;
        let state = bootstrap
            .fold
            .as_ref()
            .ok_or(OperationError::State)?
            .state();
        let sync = bootstrap.sync.as_ref().ok_or(OperationError::State)?;
        if sync.request.is_some() && state.asset.totals.pending_jobs == 0 {
            if Instant::now() >= deadline {
                return Err(OperationError::Deadline);
            }
            let event = AssetEventV1::ArmIo {
                io_id: sync.io,
                operation: AssetIoV1::SyncFiles {
                    file_ids: sync.files.clone(),
                },
            };
            queue_output_change(&self.journal, role, member, sync_id, event, deadline)?;
            self.finish_transition(bundle_id, role).await?;
        }
        let bootstrap = role
            .bootstrap
            .get_mut(&member)
            .ok_or(OperationError::State)?;
        let state = bootstrap
            .fold
            .as_ref()
            .ok_or(OperationError::State)?
            .state();
        let sync = bootstrap.sync.as_mut().ok_or(OperationError::State)?;
        if sync.request.is_some() {
            if Instant::now() >= deadline
                || bootstrap.change.is_some()
                || state
                    .asset
                    .head
                    .as_ref()
                    .is_none_or(|h| h.operation_id != sync.io)
                || state.asset.totals.pending_jobs != 1
            {
                return Err(OperationError::State);
            }
            let enter = sync.launch(
                bootstrap
                    .native_root
                    .as_mut()
                    .ok_or(OperationError::State)?,
                state
                    .asset
                    .directory
                    .as_ref()
                    .ok_or(OperationError::State)?,
            )?;
            self.release_native_entry(role.binding, enter)?;
        }
        sync.join().await?;
        let result = sync.result.as_ref().ok_or(OperationError::State)?;
        let success = result.result.is_ok() && result.native.completed();
        let io = sync.io;
        let observe = sync.observe;
        let join = sync.join;
        if state
            .asset
            .head
            .as_ref()
            .is_none_or(|h| h.operation_id != observe)
        {
            let event = AssetEventV1::ObserveIo {
                io_id: io,
                native_join_operation_id: join,
                outcome: if success {
                    NativeOutcomeV1::Completed
                } else {
                    NativeOutcomeV1::Failed {
                        error_class: "held file sync failed".into(),
                    }
                },
                observation: AssetObservationV1::Synced,
            };
            queue_output_change(&self.journal, role, member, observe.uuid(), event, deadline)?;
            self.finish_transition(bundle_id, role).await?;
        }
        if !success {
            return Err(OperationError::State);
        }
        let bootstrap = role
            .bootstrap
            .get_mut(&member)
            .ok_or(OperationError::State)?;
        let directory = bootstrap
            .fold
            .as_ref()
            .ok_or(OperationError::State)?
            .state()
            .asset
            .directory
            .ok_or(OperationError::State)?;
        let copy = *directory
            .held_copy_ids
            .first()
            .ok_or(OperationError::State)?;
        let held = bootstrap
            .native_root
            .as_mut()
            .and_then(|r| r.result.as_mut())
            .ok_or(OperationError::State)?;
        if held.files.contains_key(&copy) {
            return Err(OperationError::State);
        }
        let sync = bootstrap.sync.as_mut().ok_or(OperationError::State)?;
        let directory = sync
            .result
            .as_mut()
            .ok_or(OperationError::State)?
            .native
            .take_held_directory()
            .ok_or(OperationError::State)?;
        held.files.insert(copy, directory);
        sync.acknowledged = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
