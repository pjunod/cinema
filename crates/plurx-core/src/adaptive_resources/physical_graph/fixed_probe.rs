//! Positive selected Linux FFprobe completion from the ORIGINAL P collector.
//! A caller cannot construct this from a manifest, a path comparison, a task
//! status flag, or a response DTO. The only constructor consumes the move-only
//! core parser completion after the runtime has joined that original task.
use std::io;
use std::path::PathBuf;
use uuid::Uuid;

use crate::process::fixed_probe::{
    AdmittedProbeCompletion, DecodeFactError, FixedProbeImageClosed, FixedProbeRootClosed,
};
use crate::process::observation::ProcessIdentity;
use crate::resource_custody::codec_v2 as v2;
use crate::transcode::DecodeFacts;

#[must_use]
pub(in crate::adaptive_resources) struct FixedProbeKnownClosed {
    version_root_member: Uuid,
    document_root_member: Uuid,
    source_member: Uuid,
    output: Result<(String, DecodeFacts), DecodeFactError>,
    executable: PathBuf,
    source_object_version: String,
    executable_object_version: String,
    executable_sha256: String,
    build_digest: String,
    version_root: FixedProbeRootClosed,
    document_root: FixedProbeRootClosed,
    image_closed: FixedProbeImageClosed,
}

/// The two original native roots remain here when report data moves to M.
/// This value is consumed by P's exact journal settlement, never duplicated.
#[must_use]
pub(in crate::adaptive_resources) struct FixedProbeNativeClosure {
    version_root_member: Uuid,
    document_root_member: Uuid,
    source_member: Uuid,
    version_root: FixedProbeRootClosed,
    document_root: FixedProbeRootClosed,
    image_closed: FixedProbeImageClosed,
}

pub(in crate::adaptive_resources) struct FixedProbeReportData {
    /// A decoder/profile refusal is retained independently of the original
    /// process, image and source close capability.
    pub output: Result<(String, DecodeFacts), DecodeFactError>,
    pub executable: PathBuf,
    pub source_object_version: String,
    pub executable_object_version: String,
    pub executable_sha256: String,
    pub build_digest: String,
}

impl FixedProbeNativeClosure {
    pub(in crate::adaptive_resources) fn version_member(&self) -> Uuid {
        self.version_root_member
    }
    pub(in crate::adaptive_resources) fn document_member(&self) -> Uuid {
        self.document_root_member
    }
    pub(in crate::adaptive_resources) fn source_member(&self) -> Uuid {
        self.source_member
    }
    pub(in crate::adaptive_resources) fn version_identity(&self) -> ProcessIdentity {
        self.version_root.identity()
    }
    pub(in crate::adaptive_resources) fn document_identity(&self) -> ProcessIdentity {
        self.document_root.identity()
    }
    pub(in crate::adaptive_resources) fn document_close_operation_id(&self) -> Uuid {
        self.document_root.close_operation_id()
    }

    /// P's probe-task closed fact. Only the joined original closure can call
    /// this: both roots passed their original native close (exit, session,
    /// bounded drains, stdout/stderr/pidfd bound to the same root close), the
    /// document root inherited and outlived the source child, and both image
    /// Files had their checked close. The plan must name these same owners.
    pub(in crate::adaptive_resources) fn closed_fact(
        &self,
        plan: &v2::FixedProbeDiscoverTaskV1,
        join_operation_id: v2::Id,
    ) -> io::Result<v2::ClosedFact> {
        if plan.version != 1
            || plan.version_root_member_id.uuid() != self.version_root_member
            || plan.document_root_member_id.uuid() != self.document_root_member
            || plan.source_member_id.uuid() != self.source_member
            || plan.version_close_operation_id.uuid() != self.version_root.close_operation_id()
            || plan.document_close_operation_id.uuid() != self.document_root.close_operation_id()
            || !self.version_root.original_native_close_verified()
            || !self.document_root.original_native_close_verified()
            || self.version_root.inherited_source()
            || !self.document_root.inherited_source()
            || self.image_closed.original_object_version().is_empty()
            || join_operation_id.uuid().is_nil()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "probe closure does not match the original P task plan",
            ));
        }
        let owners = [
            (plan.image_owner_id, plan.image_close_operation_id),
            (plan.snapshot_owner_id, plan.snapshot_close_operation_id),
            (
                plan.source_child_owner_id,
                plan.source_child_close_operation_id,
            ),
            (
                plan.version_stdout_owner_id,
                plan.version_stdout_close_operation_id,
            ),
            (plan.stdout_owner_id, plan.stdout_close_operation_id),
            (plan.diagnostic_owner_id, plan.diagnostic_close_operation_id),
        ]
        .into_iter()
        .map(|(owner_id, operation_id)| v2::OwnerClosed {
            owner_id,
            operation_id,
        })
        .collect();
        Ok(v2::ClosedFact::FixedProbeDiscoverTaskV1 {
            task: plan.clone(),
            join_operation_id,
            version_close_operation_id: plan.version_close_operation_id,
            document_close_operation_id: plan.document_close_operation_id,
            owners,
        })
    }

    /// Only the original document process inherited the held source FD. Its
    /// private close record includes the checked session outcome, bounded
    /// drains, successful original wait, and dropped ChildJob.
    pub(in crate::adaptive_resources) fn closed_document_source_copy(
        &self,
        copy: &v2::SourceCopy,
    ) -> io::Result<v2::CopyClosed> {
        if !self.document_root.original_native_close_verified()
            || !self.document_root.inherited_source()
            || !matches!(copy.owner, v2::CopyOwner::OwnedRoot { root_member_id, .. }
                if root_member_id.uuid() == self.document_root_member)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "source copy lacks original document process close",
            ));
        }
        let operation_id = v2::Id::new(self.document_root.close_operation_id())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        Ok(v2::CopyClosed::ActualOwnedClose {
            copy: copy.clone(),
            operation_id,
        })
    }
}

impl FixedProbeKnownClosed {
    /// The native collector's positive return is valid only for the two fixed
    /// predeclared roots and the same scanner-bound original source. The
    /// collector independently rechecks its held source File after FFprobe.
    pub(in crate::adaptive_resources) fn from_joined_original(
        native: AdmittedProbeCompletion,
        plan: &v2::FixedProbeDiscoverTaskV1,
        observed_source_object_version: &str,
    ) -> io::Result<Self> {
        let version_root_member = plan.version_root_member_id.uuid();
        let document_root_member = plan.document_root_member_id.uuid();
        let source_member = plan.source_member_id.uuid();
        let version_close_operation_id = plan.version_close_operation_id.uuid();
        let document_close_operation_id = plan.document_close_operation_id.uuid();
        let planned_locator = plan.configured_locator.as_str();
        if !cfg!(target_os = "linux")
            || version_root_member.is_nil()
            || document_root_member.is_nil()
            || source_member.is_nil()
            || version_root_member == document_root_member
            || version_close_operation_id.is_nil()
            || document_close_operation_id.is_nil()
            || version_close_operation_id == document_close_operation_id
            || native.roots().0.close_operation_id() != version_close_operation_id
            || native.roots().1.close_operation_id() != document_close_operation_id
            || native.configured_locator() != planned_locator
            || native.source_object_version() != observed_source_object_version
            || native.executable().as_os_str().is_empty()
            || !native.roots().0.original_native_close_verified()
            || !native.roots().1.original_native_close_verified()
            || native.roots().0.inherited_source()
            || !native.roots().1.inherited_source()
            || native.roots().0.close_operation_id() == native.roots().1.close_operation_id()
            || native.image_closed().original_object_version() != native.object_version()
            || native.image_closed().snapshot_digest() != native.sha256()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "fixed probe native completion does not match original P plan",
            ));
        }
        let (
            output,
            executable,
            _configured_locator,
            source_object_version,
            executable_object_version,
            executable_sha256,
            build_digest,
            version_root,
            document_root,
            image_closed,
        ) = native.into_parts();
        Ok(Self {
            version_root_member,
            document_root_member,
            source_member,
            output,
            executable,
            source_object_version,
            executable_object_version,
            executable_sha256,
            build_digest,
            version_root,
            document_root,
            image_closed,
        })
    }

    pub(in crate::adaptive_resources) fn split(
        self,
    ) -> (FixedProbeReportData, FixedProbeNativeClosure) {
        let report = FixedProbeReportData {
            output: self.output,
            executable: self.executable,
            source_object_version: self.source_object_version,
            executable_object_version: self.executable_object_version,
            executable_sha256: self.executable_sha256,
            build_digest: self.build_digest,
        };
        let closure = FixedProbeNativeClosure {
            version_root_member: self.version_root_member,
            document_root_member: self.document_root_member,
            source_member: self.source_member,
            version_root: self.version_root,
            document_root: self.document_root,
            image_closed: self.image_closed,
        };
        (report, closure)
    }
}

/// One-shot absence of planning root/task acquisition in the original sealed
/// graph. The separate SourceCell may still own its original parent File;
/// this witness authorizes only its original cleanup, not a final P refund.
#[must_use]
pub(in crate::adaptive_resources) struct FixedPlanningNoTaskGraph {
    binding: crate::adaptive_resources::ReservationBinding,
    generation: Uuid,
    members: [Uuid; 6],
    source_member: v2::Id,
    holder_id: v2::Id,
    copies: [v2::SourceCopy; 2],
    operation_id: v2::Id,
}
impl FixedPlanningNoTaskGraph {
    pub(in crate::adaptive_resources) fn matches(
        &self,
        binding: crate::adaptive_resources::ReservationBinding,
        generation: Uuid,
        members: [Uuid; 6],
    ) -> bool {
        self.binding == binding && self.generation == generation && self.members == members
    }
    pub(in crate::adaptive_resources) fn source_member(&self) -> Uuid {
        self.source_member.uuid()
    }
    pub(in crate::adaptive_resources) fn operation_id(&self) -> v2::Id {
        self.operation_id
    }
    pub(in crate::adaptive_resources) fn registered_probe_matches(
        &self,
        binding: crate::adaptive_resources::ReservationBinding,
        member: Uuid,
    ) -> bool {
        self.binding == binding && self.members[2] == member
    }
    pub(in crate::adaptive_resources) fn source_matches(
        &self,
        source: &v2::FixedProbeDiscoverSourceV1,
    ) -> bool {
        let v2::SourceRecovery::LocalSourceHandlesV1 {
            graph_id,
            owner_id,
            filesystem_domain,
        } = &source.recovery;
        source.member_id == self.source_member
            && source.holder_id == self.holder_id
            && graph_id.uuid() == self.generation
            && *owner_id == source.filesystem_domain.daemon_owner_id
            && filesystem_domain == &source.filesystem_domain
            && matches!(self.copies[0].owner,
                v2::CopyOwner::DaemonSourceCell { owner_id: held_owner, graph_id: held_graph, member_id }
                    if held_owner == *owner_id && held_graph == *graph_id && member_id == source.member_id)
            && source.planned_copies.as_slice() == self.copies
    }
    pub(in crate::adaptive_resources) fn source_fact(
        &self,
        parent: v2::CopyClosed,
    ) -> v2::ClosedFact {
        v2::ClosedFact::FixedProbeDiscoverSourceV1 {
            holder_id: self.holder_id,
            copies: vec![
                parent,
                v2::CopyClosed::ProvenNeverAcquired {
                    copy: self.copies[1].clone(),
                    operation_id: self.operation_id,
                },
            ],
        }
    }
    pub(in crate::adaptive_resources) fn parent_copy(&self) -> &v2::SourceCopy {
        &self.copies[0]
    }
}

impl super::SealedPhysicalGraph {
    /// Only original sealed custody may mint this NATIVE namespace absence.
    /// It does not establish whether a runtime-book collector was registered.
    /// Runtime additionally retains either untouched inputs, or distinct
    /// original receiver-error AND positive-join witnesses for BOTH tasks.
    /// These paths emit distinct task facts; this graph cannot replace them.
    pub(in crate::adaptive_resources) async fn try_fixed_planning_no_task_graph(
        &self,
        members: [Uuid; 6],
        source: &v2::FixedProbeDiscoverSourceV1,
        operation_id: Uuid,
    ) -> io::Result<FixedPlanningNoTaskGraph> {
        let mut native = self.custody.members.lock().await;
        let v2::SourceRecovery::LocalSourceHandlesV1 {
            graph_id,
            owner_id,
            filesystem_domain,
        } = &source.recovery;
        let [parent, child] = source.planned_copies.as_slice() else {
            return Err(io::Error::other(
                "planning source copy inventory is incomplete",
            ));
        };
        if !cfg!(target_os = "linux")
            || !self
                .custody
                .sealed
                .load(std::sync::atomic::Ordering::Acquire)
            || native.acquired
            || native.uncertain_launch
            || native.unaudited_handoff
            || native.receipt_issued
            || native.finite_process_closed
            || native.copy_process_closed
            || !native.roots.is_empty()
            || !native.writers.is_empty()
            || !native.source_files.is_empty()
            || native.joined_writers != 0
            || native.abandoned_writers != 0
            || members.iter().any(Uuid::is_nil)
            || members
                .iter()
                .enumerate()
                .any(|(index, member)| members[..index].contains(member))
            || members.contains(&source.member_id.uuid())
            || operation_id.is_nil()
            || graph_id.uuid() != self.custody.identity.generation
            || owner_id.uuid() != self.custody.identity.process_instance
            || *owner_id != source.filesystem_domain.daemon_owner_id
            || filesystem_domain != &source.filesystem_domain
            || !matches!(parent.owner, v2::CopyOwner::DaemonSourceCell { owner_id: held_owner, graph_id: held_graph, member_id }
                if held_owner == *owner_id && held_graph == *graph_id && member_id == source.member_id)
            || parent.parent_copy_id.is_some()
            || !matches!(child.owner, v2::CopyOwner::OwnedRoot { graph_id: held_graph, root_member_id }
                if held_graph == *graph_id && root_member_id.uuid() == members[1])
            || child.parent_copy_id != Some(parent.copy_id)
            || parent.copy_id == child.copy_id
        {
            return Err(io::Error::other(
                "original planning no-task inventory is incomplete",
            ));
        }
        let operation_id = v2::Id::new(operation_id).map_err(io::Error::other)?;
        native.receipt_issued = true;
        Ok(FixedPlanningNoTaskGraph {
            binding: self.custody.binding,
            generation: self.custody.identity.generation,
            members,
            source_member: source.member_id,
            holder_id: source.holder_id,
            copies: [parent.clone(), child.clone()],
            operation_id,
        })
    }
}
