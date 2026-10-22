//! Concrete finite software-VOD preparation interface for the daemon.
//! Request data selects plans, never native facts, callbacks or refund proofs.
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
    pub executable: PathBuf,
    pub sources: Vec<FiniteSourceRequest>,
    pub segment_count: u32,
    pub demand: WorkloadDemand,
    pub output: FiniteOutputLimits,
}

/// Nonserde ownership selection. The runtime book retains all actual resources
/// if a request drops this value or cancels any borrowed setup wait.
pub struct SoftwareVodOwner {
    runtime: Arc<NodeResourceRuntime>,
    preparation: OwnedPreparation,
    source_members: Vec<Uuid>,
    source_specs: Vec<Option<SourceOpenSpec>>,
    scratch_member: Uuid,
    root_member: Uuid,
    directory_path: PathBuf,
}

/// A bounded selection of the same custodian, not an exported File/Arc<File>.
/// Cloning this selection does not duplicate a directory or admission charge.
#[derive(Clone)]
pub struct FiniteOutput {
    runtime: Arc<NodeResourceRuntime>,
    preparation: OwnedPreparation,
    member: Uuid,
    directory_path: PathBuf,
}

/// Opaque identity of a payload already retained by the runtime. Dropping the
/// ticket cannot cancel native work, lose the original result or refund credit.
pub struct PublicationTicket {
    id: Uuid,
}

impl FiniteOutput {
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
        Ok(PublicationTicket { id })
    }
    pub async fn finish(
        &self,
        ticket: &PublicationTicket,
        deadline: Instant,
    ) -> Result<(), FiniteVodError> {
        self.runtime
            .publish_scratch_output(&self.preparation, 0, self.member, ticket.id, deadline)
            .await
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
        })
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
            || request.executable.as_os_str().len() > 4096
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
        let minimum_handles = u64::try_from(request.sources.len())
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
        let mut plans = Vec::with_capacity(request.sources.len() + 2);
        let mut source_members = Vec::with_capacity(request.sources.len());
        let mut source_specs = Vec::with_capacity(request.sources.len());
        for source in request.sources {
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
                        recipe_source_binding: request.recipe_source_binding.clone(),
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
                    naming: OutputNamingV1::FilmSegmentV1,
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
            locator: request.executable.to_str().ok_or_else(invalid)?.to_owned(),
            recovery: RecoveryMethod::LiveOnly {
                reason: "fixed software VOD graph; no old-domain release proof".into(),
            },
            owned_by: None,
            depends_on: vec![],
            maximum_cardinality: 1,
            protected_paths: vec![],
        }));
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
        Ok(SoftwareVodOwner {
            runtime: Arc::clone(self),
            preparation,
            source_members,
            source_specs,
            scratch_member: scratch_member.uuid(),
            root_member: root_member.uuid(),
            directory_path,
        })
    }
}
