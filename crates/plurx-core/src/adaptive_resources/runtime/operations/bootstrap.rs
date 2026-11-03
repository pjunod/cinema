//! V3 pre-entry state in the existing retained runtime bundle.
//! No audit DTO is accepted as native acquisition/closure authority. The actual
//! immutable configured-root/native adapter must consume this known arm in place.
use super::*;
use v2::scratch::bootstrap::*;
#[cfg(unix)]
use v2::scratch::{AssetEventV1, ScratchLifecycleV1};
#[cfg(unix)]
pub(super) mod native;
#[cfg(unix)]
pub(in crate::adaptive_resources::runtime) mod prefix_removal;
#[cfg(unix)]
pub(in crate::adaptive_resources::runtime) mod readers;
#[cfg(unix)]
pub(in crate::adaptive_resources::runtime) mod retirement;

/// Heap bytes of one member's shared `io_progress` notification.
pub(super) fn io_progress_heap_bytes() -> usize {
    std::mem::size_of::<tokio::sync::Notify>() + 2 * std::mem::size_of::<usize>()
}

pub(super) struct BootstrapMember {
    plan: ScratchPlanV3,
    fold: Option<BootstrapFold>,
    change: Option<(v2::Id, AssetChangeV3)>,
    root_arm: Option<(v2::Id, v2::Id)>,
    root_ready: bool,
    next_arm: Option<(v2::Id, BootstrapIoV1)>,
    /// Signalled by every acknowledged change of THIS member. The asset has a
    /// single journaled I/O slot; a producer publication or sync that finds
    /// it held by a reader's armed operation waits here, never retries blind.
    io_progress: Arc<tokio::sync::Notify>,
    #[cfg(unix)]
    native_root: Option<native::RootCell>,
    #[cfg(unix)]
    publication: Option<native::publication::Publication>,
    #[cfg(unix)]
    publication_registered: bool,
    #[cfg(unix)]
    sync: Option<native::sync::Sync>,
    #[cfg(unix)]
    sync_registered: bool,
    #[cfg(unix)]
    readers: BTreeMap<Uuid, readers::ReaderEntry>,
    #[cfg(unix)]
    retirement: Option<retirement::CleanupRun>,
    #[cfg(unix)]
    prefix_removal: Option<prefix_removal::LiveRemoval>,
}

/// Fixture only: stage one output change through the SAME fold admission and
/// journal submission every runtime producer and reader uses. It grants no
/// native operation, result or acknowledgement.
#[cfg(all(
    test,
    feature = "fixtures",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub(super) fn fixture_queue_output_change(
    journal: &CustodyJournal,
    role: &mut OwnedRole,
    member: Uuid,
    operation: Uuid,
    event: AssetEventV1,
    deadline: Instant,
) -> Result<(), OperationError> {
    native::publication::queue_output_change(journal, role, member, operation, event, deadline)
}

// Negative fixture custody only: each variant retains the actual acknowledged
// owner taken from the original slot. This type cannot create a run or receipt.
#[cfg(all(test, target_os = "linux", feature = "fixtures"))]
pub(super) enum OriginalNativeIoFixture {
    Publication(Box<native::publication::Publication>),
    Sync(native::sync::Sync),
}

impl BootstrapMember {
    #[cfg(all(
        test,
        feature = "fixtures",
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    pub(super) fn fixture_publication_job_identity(
        &self,
    ) -> Option<(Uuid, Uuid, Uuid, Option<tokio::task::Id>, bool)> {
        self.publication
            .as_ref()
            .map(|owner| owner.fixture_job_identity())
    }
    #[cfg(all(
        test,
        feature = "fixtures",
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    pub(super) fn fixture_sync_job_identity(
        &self,
    ) -> Option<(Uuid, Uuid, Uuid, Option<tokio::task::Id>, bool)> {
        self.sync.as_ref().map(|owner| owner.fixture_job_identity())
    }
    #[cfg(all(
        test,
        feature = "fixtures",
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    pub(super) fn fixture_cleanup_job_identity(
        &self,
    ) -> Option<(Uuid, Uuid, Uuid, Option<tokio::task::Id>, bool)> {
        self.retirement.as_ref()?.fixture_job_identity()
    }
    #[cfg(all(
        test,
        feature = "fixtures",
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    pub(super) fn fixture_live_removal_job_identity(
        &self,
    ) -> Option<(Uuid, Uuid, Uuid, Option<tokio::task::Id>, bool)> {
        self.prefix_removal
            .as_ref()
            .map(|owner| owner.fixture_job_identity())
    }
    #[cfg(all(
        test,
        feature = "fixtures",
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    pub(super) fn fixture_root_arm_operation(&self) -> Option<Uuid> {
        self.root_arm.map(|(io, _)| io.uuid())
    }

    #[cfg(all(
        test,
        feature = "fixtures",
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    pub(super) fn fixture_root_job_identity(
        &self,
    ) -> Option<(Uuid, Uuid, Uuid, Option<tokio::task::Id>)> {
        self.native_root
            .as_ref()
            .map(native::RootCell::fixture_job_identity)
    }

    #[cfg(all(test, target_os = "linux", feature = "fixtures"))]
    pub(super) fn fixture_take_original_native_io(
        &mut self,
        publication: bool,
    ) -> OriginalNativeIoFixture {
        assert!(self.native_io_ready(), "only actual acknowledged owners");
        let original = if publication {
            assert!(self.publication_registered);
            OriginalNativeIoFixture::Publication(Box::new(
                self.publication.take().expect("original publication owner"),
            ))
        } else {
            assert!(self.sync_registered);
            OriginalNativeIoFixture::Sync(self.sync.take().expect("original sync owner"))
        };
        assert!(!self.native_io_slots_consistent());
        original
    }

    #[cfg(all(test, target_os = "linux", feature = "fixtures"))]
    pub(super) fn fixture_restore_original_native_io(&mut self, original: OriginalNativeIoFixture) {
        assert!(!self.native_io_slots_consistent());
        match original {
            OriginalNativeIoFixture::Publication(run) => {
                assert!(self.publication_registered && self.publication.is_none());
                self.publication = Some(*run);
            }
            OriginalNativeIoFixture::Sync(run) => {
                assert!(self.sync_registered && self.sync.is_none());
                self.sync = Some(run);
            }
        }
        assert!(self.native_io_ready(), "same acknowledged owner restored");
    }
    // Sticky registration provenance, not native success. A run is stored
    // synchronously before any Arm/worker/native entry. Missing issued owners
    // remain inconsistent; only a genuinely dormant slot may be absent.
    #[cfg(unix)]
    fn native_io_slots_consistent(&self) -> bool {
        self.publication_registered == self.publication.is_some()
            && self.sync_registered == self.sync.is_some()
    }

    #[cfg(unix)]
    fn native_io_ready(&self) -> bool {
        self.native_io_slots_consistent()
            && self
                .publication
                .as_ref()
                .is_none_or(|run| run.ready_for_transfer())
            && self
                .sync
                .as_ref()
                .is_none_or(|run| run.ready_for_transfer())
    }

    #[cfg(unix)]
    fn native_io_dormant(&self) -> bool {
        !self.publication_registered
            && !self.sync_registered
            && self.publication.is_none()
            && self.sync.is_none()
    }

    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current rolling producer callers are Linux-only; preserve the original scratch fold and slot guards on other platforms."
        )
    )]
    pub(in crate::adaptive_resources::runtime) fn fold(&self) -> Option<&BootstrapFold> {
        self.fold.as_ref()
    }
}

/// BTree node storage reserves eleven entries, including unused slots. One
/// leaf plus one internal node per item is a conservative sparse-tree bound.
/// The requested-capacity scope excludes allocator headers and RSS.
pub(super) fn btree_requested_upper_bound<K, V>(len: usize) -> Option<usize> {
    if len == 0 {
        return Some(0);
    }
    let entry = std::mem::size_of::<K>().checked_add(std::mem::size_of::<V>())?;
    let node = entry
        .checked_mul(11)?
        .checked_add(16 * std::mem::size_of::<usize>())?;
    node.checked_mul(len)?.checked_mul(2)
}

/// Scratch routing while the producer process root never entered. These
/// dispositions do not close an acquired filesystem prefix or grant a refund.
#[cfg(unix)]
pub(super) enum NeverRootBootstrap {
    UnarmedPlaceholder,
    ObservedNeverEntered,
    /// Keep the same advanced scratch prefix for its actual native driver.
    /// This is neither absence nor an observed NeverEntered witness.
    RetainedPrefix,
}

/// Fully validated target fold while the actual native cells remain owned by
/// the source member. Construction and consumption must occur under the same
/// retained-bundle lock, with no await or source mutation between them.
#[cfg(unix)]
pub(super) struct PreparedBootstrapTransfer {
    target_plan: ScratchPlanV3,
    target_fold: BootstrapFold,
}

#[cfg(unix)]
impl PreparedBootstrapTransfer {
    pub(super) fn retained_plan(&self) -> &ScratchPlanV3 {
        &self.target_plan
    }
    pub(super) fn resident_inventory_bytes(&self) -> Result<u64, OperationError> {
        self.target_fold
            .resident_inventory_bytes()
            .map_err(Into::into)
    }

    /// Infallibly moves the original RootCell/publication/sync owners into the
    /// adopted member after every validation has completed. The caller must
    /// use the exact source member that produced this token under its held
    /// bundle lock, without an await or source mutation between calls.
    pub(super) fn move_to_retained(self, source: &mut BootstrapMember) -> BootstrapMember {
        BootstrapMember {
            plan: self.target_plan,
            fold: Some(self.target_fold),
            change: None,
            root_arm: None,
            root_ready: true,
            next_arm: None,
            // The same notification: a producer waiting on the original
            // member is woken by the adopted member's acknowledgements.
            io_progress: Arc::clone(&source.io_progress),
            native_root: source.native_root.take(),
            publication: source.publication.take(),
            publication_registered: source.publication_registered,
            sync: source.sync.take(),
            sync_registered: source.sync_registered,
            readers: std::mem::take(&mut source.readers),
            retirement: source.retirement.take(),
            prefix_removal: source.prefix_removal.take(),
        }
    }
}

impl BootstrapMember {
    /// Metadata-only preflight for one new rolling segment. A false result
    /// means the original producer may wait for a reader/retention transition;
    /// it does not arm I/O or grant capacity. The queue rechecks everything
    /// under the same role lock immediately before native entry.
    #[cfg(unix)]
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current rolling producer callers are Linux-only; preserve the original scratch fold and slot guards on other platforms."
        )
    )]
    pub(super) fn rolling_segment_slot_ready(
        &self,
        next_bytes: u64,
    ) -> Result<bool, OperationError> {
        let fold = self.fold.as_ref().ok_or(OperationError::State)?;
        let state = fold.state();
        if state.asset.lifecycle != ScratchLifecycleV1::Open
            || state.bootstrap.phase != BootstrapPhase::SinkReady
            || self.retirement.is_some()
        {
            return Err(OperationError::State);
        }
        if self.change.is_some()
            || self.next_arm.is_some()
            || self.prefix_removal.is_some()
            || state.asset.totals.pending_jobs != 0
            || !self.native_io_ready()
        {
            return Ok(false);
        }
        let limits = &self.plan.limits;
        if next_bytes == 0
            || next_bytes > limits.one_file_logical_bytes
            || next_bytes > limits.one_io_payload_bytes
        {
            return Err(OperationError::State);
        }
        let storage = limits
            .storage_for(next_bytes)
            .map_err(|_| OperationError::State)?;
        Ok(
            state.asset.totals.resident_file_objects < limits.resident_file_objects
                && state.asset.totals.handles_reserved < limits.live_handles
                && state
                    .asset
                    .totals
                    .logical_bytes
                    .checked_add(next_bytes)
                    .is_some_and(|n| n <= limits.resident_logical_bytes)
                && state
                    .asset
                    .totals
                    .charged_storage_bytes
                    .checked_add(storage)
                    .is_some_and(|n| n <= limits.reserved_storage_bytes),
        )
    }

    /// Dynamic native-cell metadata surviving the exact R→S move. The staged
    /// fold inventory and each reader's fixed payload have distinct charged
    /// partitions and are deliberately excluded here.
    #[cfg(unix)]
    pub(super) fn retained_native_metadata_bytes(&self) -> Result<usize, OperationError> {
        if self.change.is_some()
            || self.next_arm.is_some()
            || self.retirement.is_some()
            || self.prefix_removal.is_some()
            || !self.native_io_ready()
        {
            return Err(OperationError::State);
        }
        let mut bytes = self
            .native_root
            .as_ref()
            .ok_or(OperationError::State)?
            .retained_heap_bytes()?;
        if let Some(publication) = &self.publication {
            bytes = bytes
                .checked_add(publication.retained_heap_bytes()?)
                .ok_or(OperationError::State)?;
        }
        if let Some(sync) = &self.sync {
            bytes = bytes
                .checked_add(sync.retained_heap_bytes()?)
                .ok_or(OperationError::State)?;
        }
        bytes = bytes
            .checked_add(
                btree_requested_upper_bound::<Uuid, readers::ReaderEntry>(self.readers.len())
                    .ok_or(OperationError::State)?,
            )
            .ok_or(OperationError::State)?;
        for reader in self.readers.values() {
            bytes = bytes
                .checked_add(reader.retained_metadata_bytes()?)
                .ok_or(OperationError::State)?;
        }
        Ok(bytes)
    }
    #[cfg(unix)]
    pub(super) fn has_root_arm(&self) -> bool {
        self.root_arm.is_some()
    }

    /// Inspect only the original retained cell. A missing in-memory
    /// placeholder is never treated as native non-entry after a durable arm.
    #[cfg(unix)]
    pub(super) fn never_root_disposition(
        &self,
        armed: bool,
    ) -> Result<NeverRootBootstrap, OperationError> {
        let fold = self.fold.as_ref().ok_or(OperationError::State)?;
        let state = fold.state();
        if self.change.is_some() || self.next_arm.is_some() {
            return Err(OperationError::State);
        }
        if state.bootstrap.phase != BootstrapPhase::Unstarted {
            // A process-root absence proof says nothing about scratch-root or
            // sink acquisition. Preserve every original owner; the normal
            // close_producer_finite_native driver still owes actual closure.
            return Ok(NeverRootBootstrap::RetainedPrefix);
        }
        if self.root_arm.is_none() {
            if armed
                || self.native_root.is_some()
                || !self.native_io_dormant()
                || !self.readers.is_empty()
                || self.retirement.is_some()
                || state.asset.head.is_some()
                || state.asset.totals.pending_jobs != 0
                || state.asset.directory.is_some()
                || !fold.files().is_empty()
            {
                return Err(OperationError::State);
            }
            return Ok(NeverRootBootstrap::UnarmedPlaceholder);
        }
        if !self
            .native_root
            .as_ref()
            .is_some_and(|root| root.initial_root_never_entered())
        {
            return Err(OperationError::State);
        }
        Ok(NeverRootBootstrap::ObservedNeverEntered)
    }

    /// Move a journal-adopted S and its two newly held directory descriptors
    /// into the live owner. Recovery validates the fold and native identities
    /// before yielding these parts; there is no fallible step after that take.
    #[cfg(target_os = "linux")]
    pub(super) fn from_recovered(
        parts: crate::adaptive_resources::runtime::recovery::RecoveredNativeParts,
    ) -> Self {
        let crate::adaptive_resources::runtime::recovery::RecoveredNativeParts {
            plan,
            fold,
            parent,
            child,
            parent_observed,
            child_observed,
            adoption_id,
        } = parts;
        let native_root = native::RootCell::from_recovered(
            parent,
            child,
            &parent_observed,
            &child_observed,
            adoption_id,
        );
        Self {
            plan,
            fold: Some(fold),
            change: None,
            root_arm: None,
            root_ready: true,
            next_arm: None,
            io_progress: Arc::new(tokio::sync::Notify::new()),
            native_root: Some(native_root),
            publication: None,
            publication_registered: false,
            sync: None,
            sync_registered: false,
            readers: BTreeMap::new(),
            retirement: None,
            prefix_removal: None,
        }
    }

    /// The same actual parent/child descriptors are owned by the recovered R
    /// cleanup actor. Its fold remains Open until the producer-specific journal
    /// retirement sequence removes every observed file and closes the domain.
    #[cfg(target_os = "linux")]
    pub(super) fn from_recovered_producer(
        parts: crate::adaptive_resources::runtime::recovery::RecoveredProducerNativeParts,
    ) -> Self {
        let crate::adaptive_resources::runtime::recovery::RecoveredProducerNativeParts {
            plan,
            fold,
            parent,
            child,
            parent_observed,
            child_observed,
            adoption_id,
        } = parts;
        Self::from_recovered(
            crate::adaptive_resources::runtime::recovery::RecoveredNativeParts {
                plan,
                fold,
                parent,
                child,
                parent_observed,
                child_observed,
                adoption_id,
            },
        )
    }

    pub(super) fn resident_inventory_bytes(&self) -> Result<u64, OperationError> {
        self.fold
            .as_ref()
            .ok_or(OperationError::State)?
            .resident_inventory_bytes()
            .map_err(Into::into)
    }

    pub(super) fn frozen_inventory(&self) -> Result<FrozenScratchV3, OperationError> {
        self.fold
            .as_ref()
            .ok_or(OperationError::State)?
            .frozen_snapshot(&self.plan)
            .map_err(Into::into)
    }

    #[cfg(unix)]
    pub(super) fn prepare_retained_transfer(
        &self,
        restored: &FrozenScratchV3,
        reference: &FrozenScratchRefV1,
        target_plan: &ScratchPlanV3,
        target_obligation: v2::Id,
        target_member: v2::Id,
    ) -> Result<PreparedBootstrapTransfer, OperationError> {
        let fold = self.fold.as_ref().ok_or(OperationError::State)?;
        let state = fold.state();
        if target_plan != &self.plan
            || !self.root_ready
            || self.root_arm.is_none()
            || self.change.is_some()
            || self.next_arm.is_some()
            || self.retirement.is_some()
            || state.asset.lifecycle != ScratchLifecycleV1::Frozen
            || state.asset.totals.pending_jobs != 0
            || state.bootstrap.phase != BootstrapPhase::SinkReady
            || state.bootstrap.current.is_some()
            || state.bootstrap.possible_child.is_some()
        {
            return Err(OperationError::State);
        }
        let full = fold.frozen_snapshot(&self.plan)?;
        if &full != restored || !readers::transfer_readers_match(self, &full.readers) {
            return Err(OperationError::State);
        }
        validate_frozen_snapshot_ref(&self.plan, reference, &full)?;
        let directory = state
            .asset
            .directory
            .as_ref()
            .ok_or(OperationError::State)?;
        let parent = state
            .bootstrap
            .parent
            .as_ref()
            .ok_or(OperationError::State)?;
        let expected_directories = [parent.observed.clone(), directory.clone()];
        if !self
            .native_root
            .as_ref()
            .is_some_and(|root| root.retained_child_ready(&expected_directories))
            || !self.native_io_slots_consistent()
            || !self
                .publication
                .as_ref()
                .is_some_and(|publication| publication.ready_for_transfer())
            || !self
                .sync
                .as_ref()
                .is_some_and(|sync| sync.ready_for_transfer())
        {
            return Err(OperationError::State);
        }
        let target_fold =
            fold.adopt_frozen(&self.plan, reference, target_obligation, target_member)?;
        if target_fold.resident_inventory_bytes()? > target_plan.limits.resident_memory_bytes {
            return Err(OperationError::State);
        }
        Ok(PreparedBootstrapTransfer {
            target_plan: target_plan.clone(),
            target_fold,
        })
    }

    /// Join already-registered native workers in their original cells. This
    /// does not launch work or create journal Arm/Observe transitions.
    pub(super) async fn join_registered_native(&mut self) -> Result<(), OperationError> {
        #[cfg(unix)]
        {
            let mut first_error =
                (!self.native_io_slots_consistent()).then_some(OperationError::State);
            if let Some(root) = self.native_root.as_mut() {
                if let Err(error) = root.join_registered().await {
                    first_error = Some(error);
                }
            }
            if let Some(publication) = self.publication.as_mut() {
                if let Err(error) = publication.join_registered().await {
                    first_error.get_or_insert(error);
                }
            }
            if let Some(sync) = self.sync.as_mut() {
                if let Err(error) = sync.join_registered().await {
                    first_error.get_or_insert(error);
                }
            }
            if let Some(retirement) = self.retirement.as_mut() {
                if let Some(cell) = retirement.pending_cell_mut() {
                    if cell.launched() {
                        if let Err(error) = cell.join_registered().await {
                            first_error.get_or_insert(error);
                        }
                    }
                }
            }
            if let Some(removal) = self.prefix_removal.as_mut() {
                if removal.launched() {
                    if let Err(error) = removal.join_registered().await {
                        first_error.get_or_insert(error);
                    }
                }
            }
            if let Some(error) = first_error {
                return Err(error);
            }
        }
        Ok(())
    }
}

impl NodeResourceRuntime {
    pub(crate) async fn arm_scratch_bootstrap(
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
        self.eligible(&bundle)?;
        let id = bundle.id;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        let plan = role
            .typed_members
            .as_ref()
            .and_then(|members| {
                members.iter().find_map(|m| match m {
                    v2::MemberPlan::ScratchAssetsV3 { slot, plan }
                        if slot.member_id.uuid() == member =>
                    {
                        Some(plan.clone())
                    }
                    _ => None,
                })
            })
            .ok_or(OperationError::State)?;
        if plan.bootstrap().authority.installation_id.uuid() != self.recovered.installation_id
            || plan.filesystem_domain.daemon_owner_id.uuid() != self.owner.owner_id
            || plan.filesystem_domain.os_boot != self.owner.os_boot
            || self.rendition_root.as_deref()
                != Some(Path::new(
                    &plan.bootstrap().authority.resolved_absolute_root,
                ))
            || role.refunded
            || role.scratch.contains_key(&member)
            || !matches!(
                role.phase,
                Phase::Prepared | Phase::AcquisitionPossible | Phase::Sealed
            )
        {
            return Err(OperationError::State);
        }
        if !role.armed.contains(&member) {
            if role.transition.is_none() {
                start_transition(
                    &self.journal,
                    role,
                    Mutation::Arm {
                        members: vec![member],
                    },
                    Effect::Arm(member),
                    deadline,
                )?;
            } else if role
                .transition
                .as_ref()
                .is_none_or(|t| t.effect != Effect::Arm(member))
            {
                return Err(OperationError::State);
            }
            self.finish_transition(id, role).await?;
        }
        if crate::adaptive_resources::runtime::operations::entry_window_closed(
            role.continuous_entry_deadline.as_ref(),
            deadline,
        ) {
            return Err(OperationError::Deadline);
        }
        if role
            .transition
            .as_ref()
            .is_some_and(|t| t.effect != Effect::Bootstrap(member))
        {
            return Err(OperationError::State);
        }
        if let Some(bootstrap) = role.bootstrap.get(&member) {
            if bootstrap.plan != plan || bootstrap.fold.is_none() {
                return Err(OperationError::State);
            }
        } else {
            let obligation = v2::Id::new(
                role.handle
                    .as_ref()
                    .ok_or(OperationError::State)?
                    .obligation_id(),
            )?;
            role.bootstrap.insert(
                member,
                BootstrapMember {
                    fold: Some(BootstrapFold::empty(
                        &plan,
                        obligation,
                        v2::Id::new(member)?,
                    )?),
                    plan,
                    change: None,
                    root_arm: None,
                    root_ready: false,
                    next_arm: None,
                    io_progress: Arc::new(tokio::sync::Notify::new()),
                    #[cfg(unix)]
                    native_root: None,
                    #[cfg(unix)]
                    publication: None,
                    #[cfg(unix)]
                    publication_registered: false,
                    #[cfg(unix)]
                    sync: None,
                    #[cfg(unix)]
                    sync_registered: false,
                    #[cfg(unix)]
                    readers: BTreeMap::new(),
                    #[cfg(unix)]
                    retirement: None,
                    prefix_removal: None,
                },
            );
        }
        queue_root(&self.journal, role, member, deadline)?;
        if role.transition.is_some() {
            self.finish_transition(id, role).await?;
        }
        let bootstrap = role.bootstrap.get(&member).ok_or(OperationError::State)?;
        let state = bootstrap
            .fold
            .as_ref()
            .ok_or(OperationError::State)?
            .state();
        if bootstrap.change.is_some() || state.asset.totals.pending_jobs != 1 {
            return Err(OperationError::State);
        }
        Ok(())
    }

    /// Advance only the admitted output's existing durable fence/freeze fold.
    /// This requires the original finite producer to be joined and retirement
    /// to have begun; it creates no native IO or close/refund authority.
    #[cfg(unix)]
    pub(in crate::adaptive_resources::runtime) async fn freeze_finite_output(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = loop {
            let mut bundle = owner.state.lock().await;
            if !bundle.installed {
                return Err(OperationError::State);
            }
            let id = bundle.id;
            let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
            let finite = role.finite_vod.as_ref().ok_or(OperationError::State)?;
            if role.refunded || finite.source_drifted || !finite.producer_joined || !finite.retiring
            {
                return Err(OperationError::State);
            }
            let planned = role.typed_members.as_ref().is_some_and(|members| {
                members.iter().any(|planned| {
                    matches!(planned,
                        v2::MemberPlan::ScratchAssetsV3 { slot, plan }
                            if slot.member_id.uuid() == member
                                && role.bootstrap.get(&member)
                                    .is_some_and(|bootstrap| bootstrap.plan == *plan))
                })
            });
            if !planned {
                return Err(OperationError::State);
            }

            if let Some(progress) = readers::reader_transition_progress(role, member) {
                self.wait_for_io_slot(bundle, index, member, progress, deadline)
                    .await?;
                continue;
            }

            // A prior operation belongs to this exact bootstrap member or this
            // retirement cannot advance it. Resume the retained operation ID and
            // journal transition; never synthesize an acknowledgement.
            if role.transition.is_some() {
                if role
                    .transition
                    .as_ref()
                    .is_none_or(|transition| transition.effect != Effect::Bootstrap(member))
                {
                    return Err(OperationError::State);
                }
                self.finish_transition(id, role).await?;
            } else if role
                .bootstrap
                .get(&member)
                .ok_or(OperationError::State)?
                .change
                .is_some()
            {
                resume_change(&self.journal, role, member, deadline)?;
                self.finish_transition(id, role).await?;
            }

            let bootstrap = role.bootstrap.get(&member).ok_or(OperationError::State)?;
            // Settle selected-but-unarmed readers while Open, including an
            // original Arm just recovered above. Frozen allows existing
            // reads/closes but never a new OpenReader operation.
            let held = (bootstrap
                .fold
                .as_ref()
                .ok_or(OperationError::State)?
                .state()
                .asset
                .lifecycle
                == ScratchLifecycleV1::Open
                && readers::transfer_pending(bootstrap))
            .then(|| Arc::clone(&bootstrap.io_progress));
            if let Some(progress) = held {
                self.wait_for_io_slot(bundle, index, member, progress, deadline)
                    .await?;
            } else {
                break bundle;
            }
        };
        let id = bundle.id;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;

        // Each successful acknowledgment is followed by a fresh fold read.
        // At most Fence and Freeze can be newly queued in this call.
        for _ in 0..2 {
            let (lifecycle, exact_head, terminal_epoch, can_fence) = {
                let bootstrap = role.bootstrap.get(&member).ok_or(OperationError::State)?;
                if bootstrap.change.is_some() || role.transition.is_some() {
                    return Err(OperationError::State);
                }
                let state = bootstrap
                    .fold
                    .as_ref()
                    .ok_or(OperationError::State)?
                    .state();
                (
                    state.asset.lifecycle,
                    state.asset.head,
                    bootstrap.plan.generation_epoch,
                    state.asset.totals.pending_jobs == 0
                        && state.bootstrap.phase == BootstrapPhase::SinkReady
                        && state.bootstrap.possible_child.is_none(),
                )
            };
            match lifecycle {
                ScratchLifecycleV1::Open => {
                    if !can_fence {
                        return Err(OperationError::State);
                    }
                    native::publication::queue_output_change(
                        &self.journal,
                        role,
                        member,
                        Uuid::new_v4(),
                        AssetEventV1::Fence { terminal_epoch },
                        deadline,
                    )?;
                    self.finish_transition(id, role).await?;
                }
                ScratchLifecycleV1::Freezing => {
                    let exact_head_before_freeze = exact_head.ok_or(OperationError::State)?;
                    native::publication::queue_output_change(
                        &self.journal,
                        role,
                        member,
                        Uuid::new_v4(),
                        AssetEventV1::Freeze {
                            exact_head_before_freeze,
                        },
                        deadline,
                    )?;
                    self.finish_transition(id, role).await?;
                }
                ScratchLifecycleV1::Frozen => return Ok(()),
                _ => return Err(OperationError::State),
            }
        }
        let bootstrap = role.bootstrap.get(&member).ok_or(OperationError::State)?;
        if bootstrap
            .fold
            .as_ref()
            .ok_or(OperationError::State)?
            .state()
            .asset
            .lifecycle
            == ScratchLifecycleV1::Frozen
        {
            Ok(())
        } else {
            Err(OperationError::State)
        }
    }
}

fn queue_root(
    journal: &CustodyJournal,
    role: &mut OwnedRole,
    member: Uuid,
    deadline: Instant,
) -> Result<(), OperationError> {
    if role.refunded
        || role.handle.is_none()
        || !role.armed.contains(&member)
        || !matches!(role.phase, Phase::AcquisitionPossible | Phase::Sealed)
        || role
            .transition
            .as_ref()
            .is_some_and(|t| t.effect != Effect::Bootstrap(member))
    {
        return Err(OperationError::State);
    }
    let bootstrap = role
        .bootstrap
        .get_mut(&member)
        .ok_or(OperationError::State)?;
    if bootstrap.root_arm.is_some() {
        return resume_change(journal, role, member, deadline);
    }
    if role.transition.is_some() {
        return Err(OperationError::State);
    }
    let io_id = v2::Id::new(Uuid::new_v4())?;
    let copy_id = v2::Id::new(Uuid::new_v4())?;
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
        event: AssetEventV3::ArmBootstrapV1 {
            io_id,
            operation: BootstrapIoV1::OpenNamespaceRoot { copy_id },
        },
    };
    stage_change(bootstrap, io_id, change)?;
    bootstrap.root_arm = Some((io_id, copy_id));
    resume_change(journal, role, member, deadline)
}

/// Stage `change` as this member's one retained journal command, only if the
/// member's fold admits it now. The journal commits a change before
/// `apply_committed` folds it, so the fold's own preconditions (chain linkage
/// and the asset's single pending I/O slot) are checked here, before the
/// change can reach the journal. A refusal stages nothing and leaves the fold,
/// the role and the journal untouched.
fn stage_change(
    bootstrap: &mut BootstrapMember,
    id: v2::Id,
    change: AssetChangeV3,
) -> Result<(), OperationError> {
    bootstrap
        .fold
        .as_ref()
        .ok_or(OperationError::State)?
        .admits(&bootstrap.plan, id, &change)
        .map_err(|_| OperationError::State)?;
    bootstrap.change = Some((id, change));
    Ok(())
}

/// Whether THIS member's single journaled I/O slot is held by work that clears
/// only through `acknowledge`, which signals the returned notification under
/// the bundle lock: the member's own committed transition, its staged change,
/// or an armed native operation not yet observed (a client's read, a
/// publication or a sync). A transition of another effect is not this slot's
/// and is left to the caller's own check.
#[cfg(unix)]
pub(super) fn io_slot_held(role: &OwnedRole, member: Uuid) -> Option<Arc<tokio::sync::Notify>> {
    if role
        .transition
        .as_ref()
        .is_some_and(|transition| transition.effect != Effect::Bootstrap(member))
    {
        return None;
    }
    let bootstrap = role.bootstrap.get(&member)?;
    let state = bootstrap.fold.as_ref()?.state();
    (role.transition.is_some()
        || bootstrap.change.is_some()
        || state.asset.totals.pending_jobs != 0)
        .then(|| Arc::clone(&bootstrap.io_progress))
}

/// New readers wait across this original joined producer's handoff to S.
/// Join already fenced R's native entry; Frozen never grants OpenReader.
#[cfg(unix)]
pub(super) fn finite_handoff_progress(
    role: &OwnedRole,
    member: Uuid,
) -> Option<Arc<tokio::sync::Notify>> {
    if role.refunded {
        return None;
    }
    if let Some(progress) = role
        .retirement
        .as_ref()
        .and_then(|retirement| retirement.handoff_progress(member))
    {
        return Some(progress);
    }
    let finite = role.finite_vod.as_ref()?;
    let bootstrap = role.bootstrap.get(&member)?;
    // A cancelled reader still owns its queued Arm/Observe and its matching
    // in-memory IO state. Wait for that reader; never reconcile its ACK here.
    let reader_transition = readers::reader_transition_progress(role, member).is_some();
    (!finite.source_drifted
        && finite.producer_joined
        && finite.retiring
        && finite.process_closure.is_some()
        && ((role.transition.is_none() && bootstrap.change.is_none()) || reader_transition)
        && bootstrap.retirement.is_none()
        && matches!(
            bootstrap.fold.as_ref()?.state().asset.lifecycle,
            ScratchLifecycleV1::Open | ScratchLifecycleV1::Frozen
        ))
    .then(|| Arc::clone(&bootstrap.io_progress))
}

#[cfg(unix)]
pub(super) async fn wait_for_handoff(
    bundle: tokio::sync::MutexGuard<'_, Bundle>,
    progress: Arc<tokio::sync::Notify>,
    deadline: Instant,
) -> Result<(), OperationError> {
    native::publication::wait_for_io_slot(bundle, progress, deadline).await
}

#[cfg(unix)]
impl BootstrapMember {
    pub(super) fn progress_signal(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.io_progress)
    }
}

// A command may be retained before queue submission fails (e.g. its deadline).
// Retry that EXACT command/id, not a new native operation or an invented ack.
fn resume_change(
    journal: &CustodyJournal,
    role: &mut OwnedRole,
    member: Uuid,
    deadline: Instant,
) -> Result<(), OperationError> {
    if let Some(transition) = &role.transition {
        return if transition.effect == Effect::Bootstrap(member) {
            Ok(())
        } else {
            Err(OperationError::State)
        };
    }
    if let Some((id, change)) = role
        .bootstrap
        .get(&member)
        .ok_or(OperationError::State)?
        .change
        .as_ref()
        .cloned()
    {
        start_named_transition(
            journal,
            role,
            id.uuid(),
            Mutation::AssetChangeV3 { change },
            Effect::Bootstrap(member),
            deadline,
        )?;
    }
    Ok(())
}

pub(super) fn apply_committed(
    role: &mut OwnedRole,
    member: Uuid,
    operation: Uuid,
) -> Result<(), OperationError> {
    let bootstrap = role
        .bootstrap
        .get_mut(&member)
        .ok_or(OperationError::State)?;
    let (id, change) = bootstrap.change.as_ref().ok_or(OperationError::State)?;
    if id.uuid() != operation {
        return Err(OperationError::State);
    }
    if bootstrap
        .fold
        .as_ref()
        .is_some_and(|f| f.state().asset.head.is_some_and(|h| h.operation_id == *id))
    {
        return Ok(());
    }
    let fold = bootstrap.fold.take().ok_or(OperationError::State)?;
    bootstrap.fold = Some(fold.apply(&bootstrap.plan, *id, change)?);
    Ok(())
}

pub(super) fn acknowledge(role: &mut OwnedRole, member: Uuid) -> Result<(), OperationError> {
    let bootstrap = role
        .bootstrap
        .get_mut(&member)
        .ok_or(OperationError::State)?;
    bootstrap.change.take().ok_or(OperationError::State)?;
    // Under the bundle lock, after the change is cleared: a waiter that
    // registered before its locked slot check cannot miss this wakeup.
    bootstrap.io_progress.notify_waiters();
    #[cfg(unix)]
    readers::notify_progress(bootstrap);
    #[cfg(target_os = "linux")]
    if let Some(copy) = role.copy_vod.as_ref() {
        copy.progress.notify_waiters();
    }
    Ok(())
}
