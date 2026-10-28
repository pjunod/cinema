//! The original native publisher under the SAME bootstrap/journal custodian.
//! Nothing exported here is a receipt constructor or a pathname fallback.
use super::*;
use crate::fs_secure::publication::{ExpectedPublishedObject, PublicationIo, PublicationRequest};
use sha2::{Digest, Sha256};
use v2::scratch::{
    AssetEventV1, AssetIoV1, AssetObservationV1, FileObservedV1, FileSlotId, NativeOutcomeV1,
    OutputKey,
};

pub(in crate::adaptive_resources::runtime::operations) struct Publication {
    io_id: v2::Id,
    observe_id: v2::Id,
    join_id: v2::Id,
    copy_id: v2::Id,
    output: OutputKey,
    prior_playlist: Option<v2::Id>,
    logical_end: u64,
    payload_capacity: u64,
    content_binding: String,
    request: Option<PublicationRequest>,
    pending: Option<JoinHandle<PublicationResult>>,
    result: Option<PublicationResult>,
    failed_join: bool,
    acknowledged: bool,
}
struct PublicationResult {
    native: PublicationIo,
    result: std::io::Result<()>,
}
impl Publication {
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn retained_heap_bytes(
        &self,
    ) -> Result<usize, OperationError> {
        if !self.ready_for_transfer() || self.request.is_some() {
            return Err(OperationError::State);
        }
        self.result
            .as_ref()
            .ok_or(OperationError::State)?
            .native
            .retained_heap_bytes()
            .and_then(|bytes| bytes.checked_add(self.content_binding.capacity()))
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
                    && result.native.published()
                    && !result.native.has_unresolved_object()
                    && !result.native.has_held_directory()
            })
    }
    /// Await only this cell's original registered worker. Preserve both the
    /// native object and its I/O result for the ordinary Observe path.
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

    async fn join(&mut self) -> Result<&PublicationResult, OperationError> {
        if self.failed_join {
            return Err(OperationError::State);
        }
        if let Some(pending) = self.pending.as_mut() {
            // Cancellation leaves the ORIGINAL job in this runtime-owned cell.
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
    fn launch(
        &mut self,
        root: &mut RootCell,
        directory: &v2::scratch::DirectoryObservedV1,
    ) -> Result<tokio::sync::oneshot::Sender<()>, OperationError> {
        // All checks precede moving the sole File or request. A failed check
        // retains both objects in their already-counted custodians.
        if self.pending.is_some()
            || self.result.is_some()
            || self.failed_join
            || self.request.is_none()
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
        let request = self.request.take().expect("validated retained request");
        let directory = held.files.remove(&copy).expect("validated SAME child");
        let mut native = PublicationIo::from_held_directory(request, directory);
        let (enter, registered) = tokio::sync::oneshot::channel();
        let pending = runtime.spawn_blocking(move || {
            let result = match registered.blocking_recv() {
                Ok(()) => native.run(),
                Err(_) => Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "publication native entry was never released",
                )),
            };
            PublicationResult { native, result }
        });
        self.pending = Some(pending);
        // The original handle is registered BEFORE its native-entry permission.
        Ok(enter)
    }
}

fn expected_playlist_object(
    object: &v2::NativeObject,
) -> Result<ExpectedPublishedObject, OperationError> {
    let (
        v2::NativeValue::Known { value: device },
        v2::NativeValue::Known { value: inode },
        v2::NativeValue::Known { value: mount_id },
    ) = (&object.device, &object.inode, &object.mount_id)
    else {
        return Err(OperationError::State);
    };
    if object.file_type != v2::FileType::Regular {
        return Err(OperationError::State);
    }
    Ok(ExpectedPublishedObject {
        device: *device,
        inode: *inode,
        mount_id: *mount_id,
    })
}

impl NodeResourceRuntime {
    /// Synchronous payload adoption: no async argument can drop the sole live
    /// child, native job, or accepted payload before runtime registration.
    /// Rejection leaves the caller's payload untouched.
    pub(crate) fn queue_scratch_publication(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        output: OutputKey,
        payload: &mut Vec<u8>,
    ) -> Result<Uuid, OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.try_lock().map_err(|_| OperationError::State)?;
        self.queue_scratch_publication_locked(&mut bundle, index, member, output, payload)
    }

    /// Wait for the SAME bundle lock before adopting a producer payload. The
    /// caller retains the payload for the entire wait; every rejection leaves
    /// it untouched.
    pub(in crate::adaptive_resources::runtime) async fn queue_scratch_publication_wait(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        output: OutputKey,
        payload: &mut Vec<u8>,
    ) -> Result<Uuid, OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        self.queue_scratch_publication_locked(&mut bundle, index, member, output, payload)
    }

    fn queue_scratch_publication_locked(
        &self,
        bundle: &mut Bundle,
        index: usize,
        member: Uuid,
        output: OutputKey,
        payload: &mut Vec<u8>,
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
        let state = bootstrap
            .fold
            .as_ref()
            .ok_or(OperationError::State)?
            .state();
        let prior_playlist = if output == OutputKey::Playlist {
            let fold = bootstrap.fold.as_ref().ok_or(OperationError::State)?;
            let mut matches = fold
                .files()
                .iter()
                .filter(|file| file.published == Some(OutputKey::Playlist));
            let first = matches.next();
            if matches.next().is_some() {
                return Err(OperationError::State);
            }
            first
                .map(|file| {
                    if fold.has_reader_for_file(file.file_id) || !file.held_copy_ids.is_empty() {
                        return Err(OperationError::State);
                    }
                    Ok((file.file_id, expected_playlist_object(&file.native_object)?))
                })
                .transpose()?
        } else {
            None
        };
        if state.bootstrap.phase != BootstrapPhase::SinkReady
            || state.asset.lifecycle != v2::scratch::ScratchLifecycleV1::Open
            || state.asset.totals.pending_jobs != 0
            || bootstrap.change.is_some()
            || bootstrap.next_arm.is_some()
            || !bootstrap.native_io_ready()
            || !bootstrap.plan.outputs.contains(&output)
        {
            return Err(OperationError::State);
        }
        let len = u64::try_from(payload.len()).map_err(|_| OperationError::State)?;
        let capacity = u64::try_from(payload.capacity()).map_err(|_| OperationError::State)?;
        let limits = &bootstrap.plan.limits;
        let io_id = v2::Id::new(Uuid::new_v4())?;
        let observe_id = v2::Id::new(Uuid::new_v4())?;
        let join_id = v2::Id::new(Uuid::new_v4())?;
        let copy_id = v2::Id::new(Uuid::new_v4())?;
        let name = match output {
            OutputKey::Init => "init.mp4".to_owned(),
            OutputKey::Segment { index } => format!("seg{index:05}.m4s"),
            OutputKey::Playlist => "index.m3u8".to_owned(),
            OutputKey::IdentityMetadata => "identity.json".to_owned(),
            OutputKey::EncodedProcessMetadata => "encoded-process".to_owned(),
            OutputKey::CompleteArtifactManifest => "complete-v1.json".to_owned(),
        };
        let content_binding = format!("sha256:{:x}", Sha256::digest(payload.as_slice()));
        let metadata = matches!(
            output,
            OutputKey::IdentityMetadata
                | OutputKey::EncodedProcessMetadata
                | OutputKey::CompleteArtifactManifest
        );
        let validated = if matches!(output, OutputKey::Playlist) {
            if let Some((_, old)) = prior_playlist {
                PublicationRequest::for_playlist_replacement(Vec::new(), io_id.uuid(), old)?
            } else {
                PublicationRequest::for_playlist_operation(Vec::new(), io_id.uuid())?
            }
        } else if metadata {
            PublicationRequest::for_metadata_operation(&name, Vec::new(), io_id.uuid())?
        } else {
            PublicationRequest::for_operation(&name, Vec::new(), io_id.uuid())?
        };
        // Concrete Rust-owned cells and actual heap capacities. Runtime task
        // allocator/DB projection overhead still require their node-baseline
        // accounting; this calculation is not a whole-host physical quota.
        let request_memory = validated.resident_memory_bytes()?;
        let fixed = std::mem::size_of::<Publication>()
            .checked_add(std::mem::size_of::<PublicationResult>())
            .and_then(|n| n.checked_add(std::mem::size_of::<JoinHandle<PublicationResult>>()))
            .and_then(|n| n.checked_add(request_memory))
            .and_then(|n| n.checked_add(content_binding.capacity()))
            .and_then(|n| u64::try_from(n).ok())
            .ok_or(OperationError::State)?;
        let memory = state
            .asset
            .totals
            .memory_reserved_bytes
            .checked_add(capacity)
            .and_then(|n| n.checked_add(fixed))
            .ok_or(OperationError::State)?;
        let storage = limits.storage_for(len)?;
        if len == 0
            || len > limits.one_io_payload_bytes
            || len > limits.one_file_logical_bytes
            || memory > limits.resident_memory_bytes
            || state.asset.totals.resident_file_objects >= limits.resident_file_objects
            || state
                .asset
                .totals
                .logical_bytes
                .checked_add(len)
                .is_none_or(|n| n > limits.resident_logical_bytes)
            || state
                .asset
                .totals
                .charged_storage_bytes
                .checked_add(storage)
                .is_none_or(|n| n > limits.reserved_storage_bytes)
            || state
                .asset
                .totals
                .handles_reserved
                .checked_add(1)
                .is_none_or(|n| n > limits.live_handles)
        {
            return Err(OperationError::State);
        }
        drop(validated);
        let request = if matches!(output, OutputKey::Playlist) {
            if let Some((_, old)) = prior_playlist {
                PublicationRequest::for_playlist_replacement(
                    std::mem::take(payload),
                    io_id.uuid(),
                    old,
                )
            } else {
                PublicationRequest::for_playlist_operation(std::mem::take(payload), io_id.uuid())
            }
        } else if metadata {
            PublicationRequest::for_metadata_operation(&name, std::mem::take(payload), io_id.uuid())
        } else {
            PublicationRequest::for_operation(&name, std::mem::take(payload), io_id.uuid())
        }
        .expect("same already-validated name/id and bounded payload length");
        bootstrap.publication_registered = true;
        bootstrap.publication = Some(Publication {
            io_id,
            observe_id,
            join_id,
            copy_id,
            output,
            prior_playlist: prior_playlist.map(|(id, _)| id),
            logical_end: len,
            payload_capacity: capacity,
            content_binding,
            request: Some(request),
            pending: None,
            result: None,
            failed_join: false,
            acknowledged: false,
        });
        Ok(io_id.uuid())
    }

    /// Drive exactly the registered operation. Native completion is persisted
    /// before returning success to an ordinary materialization/reader caller.
    pub(crate) async fn publish_scratch_output(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        publication_id: Uuid,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        if !bundle.installed {
            return Err(OperationError::State);
        }
        // An accepted native result must remain recordable after cancellation;
        // eligibility is checked before native launch, not before its observe.
        let needs_entry = bundle
            .roles
            .get(index)
            .and_then(|r| r.bootstrap.get(&member))
            .and_then(|b| b.publication.as_ref())
            .is_some_and(|p| p.request.is_some());
        if needs_entry {
            self.eligible(&bundle)?;
        }
        let bundle_id = bundle.id;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        let bootstrap = role.bootstrap.get(&member).ok_or(OperationError::State)?;
        let publication = bootstrap
            .publication
            .as_ref()
            .ok_or(OperationError::State)?;
        if publication.io_id.uuid() != publication_id || role.refunded {
            return Err(OperationError::State);
        }
        if publication.acknowledged {
            return Ok(());
        }
        resume_change(&self.journal, role, member, deadline)?;
        if role.transition.is_some() {
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
        let publication = bootstrap
            .publication
            .as_ref()
            .ok_or(OperationError::State)?;
        if publication.request.is_some() && state.asset.totals.pending_jobs == 0 {
            if !matches!(role.phase, Phase::AcquisitionPossible | Phase::Sealed)
                || Instant::now() >= deadline
            {
                return Err(OperationError::State);
            }
            let operation = if let Some(prior_file_id) = publication.prior_playlist {
                let old = bootstrap
                    .fold
                    .as_ref()
                    .ok_or(OperationError::State)?
                    .files()
                    .iter()
                    .find(|file| file.file_id == prior_file_id)
                    .ok_or(OperationError::State)?;
                AssetIoV1::ReplacePlaylist {
                    prior_file_id,
                    expected_native_object: old.native_object.clone(),
                    slot_id: FileSlotId(publication.io_id),
                    logical_end: publication.logical_end,
                    payload_capacity: publication.payload_capacity,
                    copy_id: publication.copy_id,
                }
            } else if matches!(
                publication.output,
                OutputKey::IdentityMetadata | OutputKey::EncodedProcessMetadata
            ) {
                AssetIoV1::PublishMetadata {
                    slot_id: FileSlotId(publication.io_id),
                    output: publication.output,
                    logical_end: publication.logical_end,
                    payload_capacity: publication.payload_capacity,
                    copy_id: publication.copy_id,
                }
            } else {
                AssetIoV1::PublishNew {
                    slot_id: FileSlotId(publication.io_id),
                    output: publication.output,
                    logical_end: publication.logical_end,
                    payload_capacity: publication.payload_capacity,
                    copy_id: publication.copy_id,
                }
            };
            let event = AssetEventV1::ArmIo {
                io_id: publication.io_id,
                operation,
            };
            queue_output_change(&self.journal, role, member, publication_id, event, deadline)?;
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
        let publication = bootstrap
            .publication
            .as_mut()
            .ok_or(OperationError::State)?;
        if publication.request.is_some() {
            if !matches!(role.phase, Phase::AcquisitionPossible | Phase::Sealed)
                || Instant::now() >= deadline
                || bootstrap.change.is_some()
                || state
                    .asset
                    .head
                    .as_ref()
                    .is_none_or(|h| h.operation_id != publication.io_id)
                || state.asset.totals.pending_jobs != 1
            {
                return Err(OperationError::State);
            }
            let directory = state
                .asset
                .directory
                .as_ref()
                .ok_or(OperationError::State)?;
            let enter = publication.launch(
                bootstrap
                    .native_root
                    .as_mut()
                    .ok_or(OperationError::State)?,
                directory,
            )?;
            self.release_native_entry(role.binding, enter)?;
        }
        publication.join().await?;
        let publication = bootstrap
            .publication
            .as_ref()
            .ok_or(OperationError::State)?;
        let result = publication.result.as_ref().ok_or(OperationError::State)?;
        let Some(observed) = result.native.observation() else {
            // Entered failures without an exact native object are still held.
            // No fake NeverEntered result or refund is manufactured.
            return Err(OperationError::State);
        };
        let reserved = bootstrap.plan.limits.storage_for(publication.logical_end)?;
        let charged = reserved
            .max(observed.reported_allocated_bytes())
            .max(observed.identity().size);
        if charged > reserved {
            self.ledger.record_native_overage(
                role.binding,
                publication.io_id.uuid(),
                ResourceVector {
                    scratch_bytes: charged - reserved,
                    ..ResourceVector::default()
                },
            )?;
            // The current bounded projection cannot acknowledge an over-limit
            // result. Keep exact native custody/arm and globally fence admission.
            return Err(OperationError::State);
        }
        let directory = state
            .asset
            .directory
            .as_ref()
            .ok_or(OperationError::State)?;
        let matches_domain = directory.native_object.device
            == v2::NativeValue::Known {
                value: observed.identity().device,
            }
            && matches!((&directory.native_object.mount_id, observed.mount_id()),
                (v2::NativeValue::Known { value }, Some(mount)) if *value == mount);
        let success = result.result.is_ok()
            && result.native.published()
            && !result.native.has_unresolved_object()
            && matches_domain;
        let outcome = if success {
            NativeOutcomeV1::Completed
        } else {
            NativeOutcomeV1::Failed {
                error_class: "owned native publication failed".into(),
            }
        };
        let file = FileObservedV1 {
            file_id: publication.observe_id,
            slot_id: FileSlotId(publication.io_id),
            creation_io_id: publication.io_id,
            native_object: v2::NativeObject {
                file_type: v2::FileType::Regular,
                device: v2::NativeValue::Known {
                    value: observed.identity().device,
                },
                inode: v2::NativeValue::Known {
                    value: observed.identity().inode,
                },
                mount_id: observed.mount_id().map_or_else(
                    || v2::NativeValue::Unknown {
                        reason: "descriptor mount identity unavailable".into(),
                    },
                    |value| v2::NativeValue::Known { value },
                ),
                filesystem_domain: bootstrap.plan.filesystem_domain.clone(),
            },
            temporary: !result.native.published(),
            published: result.native.published().then_some(publication.output),
            logical_len: observed.identity().size,
            reported_allocated_bytes: v2::NativeValue::Known {
                value: observed.reported_allocated_bytes(),
            },
            charged_bytes: charged,
            held_copy_ids: if result.native.has_unresolved_object() {
                vec![publication.copy_id]
            } else {
                vec![]
            },
        };
        let event = AssetEventV1::ObserveIo {
            io_id: publication.io_id,
            native_join_operation_id: publication.join_id,
            outcome,
            observation: AssetObservationV1::File {
                observed: file,
                content_binding: success.then(|| publication.content_binding.clone()),
            },
        };
        let observe_id = publication.observe_id;
        if state
            .asset
            .head
            .as_ref()
            .is_none_or(|h| h.operation_id != observe_id)
        {
            queue_output_change(
                &self.journal,
                role,
                member,
                observe_id.uuid(),
                event,
                deadline,
            )?;
            self.finish_transition(bundle_id, role).await?;
        }
        if !success {
            return Err(OperationError::State);
        }
        // The observation and its acknowledgement are KNOWN before ownership
        // returns to the same root cell or an application may expose the output.
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
        let publication = bootstrap
            .publication
            .as_mut()
            .ok_or(OperationError::State)?;
        let result = publication.result.as_mut().ok_or(OperationError::State)?;
        let directory = result.native.take_held_directory()?;
        held.files.insert(copy, directory);
        result
            .native
            .release_completed_payload()
            .ok_or(OperationError::State)?;
        publication.acknowledged = true;
        Ok(())
    }
}

pub(in crate::adaptive_resources::runtime::operations) fn queue_output_change(
    journal: &CustodyJournal,
    role: &mut OwnedRole,
    member: Uuid,
    operation: Uuid,
    event: AssetEventV1,
    deadline: Instant,
) -> Result<(), OperationError> {
    let bootstrap = role
        .bootstrap
        .get_mut(&member)
        .ok_or(OperationError::State)?;
    if bootstrap.change.is_some() || role.transition.is_some() {
        return Err(OperationError::State);
    }
    let change = AssetChangeV3 {
        scratch_member_id: v2::Id::new(member)?,
        asset_id: bootstrap.plan.asset_id,
        generation_epoch: bootstrap.plan.generation_epoch,
        previous: bootstrap
            .fold
            .as_ref()
            .ok_or(OperationError::State)?
            .state()
            .asset
            .head,
        event: AssetEventV3::OutputV2 { event },
    };
    bootstrap.change = Some((v2::Id::new(operation)?, change.clone()));
    start_named_transition(
        journal,
        role,
        operation,
        Mutation::AssetChangeV3 { change },
        Effect::Bootstrap(member),
        deadline,
    )
}

#[cfg(test)]
mod tests;
