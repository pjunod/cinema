//! Strict v2 audit data for the concrete finite-VOD ownership path.
//! None of these disk types constructs a physical receipt or a launch permit.
mod cost;
mod fixed_probe;
mod retained_v2;
pub(crate) mod scratch;
mod settlement;
#[cfg(test)]
pub(in crate::resource_custody) mod tests;
use super::{codec, CustodyError, Role};
use codec::{BootIdentity, MemberEnvelope, MemberKind, PlannedMember};
pub(crate) use cost::{retained_minimum_resources, validate_retained_resources};
#[cfg(any(target_os = "linux", test))]
pub(crate) use fixed_probe::{engine_task_plan_sha256, validate_planning_settlement_size};
#[cfg(target_os = "linux")]
pub(crate) use fixed_probe::{FixedPlanningEngineFileSlotV1, FIXED_PLANNING_ENGINE_MAX_OBJECTS};
pub(crate) use fixed_probe::{
    FixedPlanningEngineTaskObservedV1, FixedPlanningEngineTaskObservedV2,
    FixedPlanningEngineTaskV1, FixedProbeDiscoverSourceV1, FixedProbeDiscoverTaskObservedV1,
    FixedProbeDiscoverTaskV1, FixedProbeMetadataV1, FixedProbeTaskV1,
};
pub(crate) use retained_v2::*;
use serde::{Deserialize, Serialize};
pub(crate) use settlement::*;
use std::collections::BTreeSet;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub(crate) struct Id(Uuid);
impl Id {
    pub(crate) fn new(value: Uuid) -> Result<Self, CustodyError> {
        codec::nonnil(value)?;
        Ok(Self(value))
    }
    pub(crate) fn uuid(self) -> Uuid {
        self.0
    }
}
impl TryFrom<String> for Id {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let id = Uuid::parse_str(&value).map_err(|_| "invalid custody UUID")?;
        if id.is_nil() || id.to_string() != value {
            return Err("noncanonical custody UUID");
        }
        Ok(Self(id))
    }
}
impl From<Id> for String {
    fn from(id: Id) -> String {
        id.0.to_string()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "state", rename_all = "snake_case")]
pub(crate) enum NativeValue<T> {
    Known { value: T },
    Unknown { reason: String },
}
impl<T> NativeValue<T> {
    fn resident_heap_bytes(&self) -> Result<u64, CustodyError> {
        match self {
            Self::Known { .. } => Ok(0),
            Self::Unknown { reason } => {
                u64::try_from(reason.capacity()).map_err(|_| codec::invalid())
            }
        }
    }
}
impl<T> NativeValue<T> {
    fn validate(&self) -> Result<(), CustodyError> {
        if let Self::Unknown { reason } = self {
            codec::text(reason)?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FilesystemPlatform {
    Linux,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FilesystemDomain {
    pub platform: FilesystemPlatform,
    pub domain_id: Id,
    pub daemon_owner_id: Id,
    pub os_boot: BootIdentity,
    pub mount_namespace: NativeValue<u64>,
}
impl FilesystemDomain {
    fn validate(&self) -> Result<(), CustodyError> {
        codec::validate_boot(&self.os_boot)?;
        self.mount_namespace.validate()
    }
    fn resident_heap_bytes(&self) -> Result<u64, CustodyError> {
        let boot = match &self.os_boot {
            codec::BootIdentity::Known { provider, .. } => {
                u64::try_from(provider.capacity()).map_err(|_| codec::invalid())?
            }
            codec::BootIdentity::Unknown { reason } => {
                u64::try_from(reason.capacity()).map_err(|_| codec::invalid())?
            }
        };
        boot.checked_add(self.mount_namespace.resident_heap_bytes()?)
            .ok_or_else(codec::invalid)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceAssociation {
    pub media_file_id: String,
    pub object_version: String,
    pub recipe_source_binding: String,
}
impl SourceAssociation {
    fn validate(&self) -> Result<(), CustodyError> {
        codec::long_text(&self.media_file_id)?;
        codec::long_text(&self.object_version)?;
        codec::long_text(&self.recipe_source_binding)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceAccess {
    ReadOnlyRegular,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "owner", rename_all = "snake_case")]
pub(crate) enum CopyOwner {
    DaemonSourceCell {
        owner_id: Id,
        graph_id: Id,
        member_id: Id,
    },
    OwnedRoot {
        graph_id: Id,
        root_member_id: Id,
    },
    RetainedAssets {
        asset_id: Id,
        source_member_id: Id,
    },
    RetainedRoleHolder {
        obligation_id: Id,
        member_id: Id,
        family_id: Id,
        role: Role,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceCopy {
    pub copy_id: Id,
    pub owner: CopyOwner,
    pub acquisition_operation_id: Id,
    pub parent_copy_id: Option<Id>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub(crate) enum SourceRecovery {
    LocalSourceHandlesV1 {
        owner_id: Id,
        graph_id: Id,
        filesystem_domain: FilesystemDomain,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourcePlan {
    pub version: u32,
    pub holder_id: Id,
    pub member_id: Id,
    pub source_association: SourceAssociation,
    pub access: SourceAccess,
    pub filesystem_domain: FilesystemDomain,
    pub planned_copies: Vec<SourceCopy>,
    pub recovery: SourceRecovery,
}

/// A direct reader freezes scanner and dispatch facts before native entry.
/// The exact S1 identity exists only in the original open's observed result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectSourcePlanV1 {
    pub version: u32,
    pub holder_id: Id,
    pub member_id: Id,
    pub media_file_id: String,
    pub recipe_source_binding: String,
    pub provenance: DirectSourceProvenanceV1,
    pub read_domain_obligation_id: Id,
    pub absolute_path: String,
    pub scanner_size: i64,
    pub scanner_mtime: i64,
    pub expectation: DirectSourceExpectationV1,
    pub access: SourceAccess,
    pub filesystem_domain: FilesystemDomain,
    pub planned_copies: Vec<SourceCopy>,
    pub recovery: SourceRecovery,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub(crate) enum DirectSourceExpectationV1 {
    ScannerExact,
    AuthorizedLocalPath,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectReadDomainPlanV1 {
    pub version: u32,
    pub domain_id: Id,
    pub owner_id: Id,
    pub graph_id: Id,
    pub maximum_parallel_reads: u32,
}
/// Audit provenance for the pre-open selected path. Neither arm grants user
/// authorization or stands in for the observed native S1 identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub(crate) enum DirectSourceProvenanceV1 {
    Sharing {
        assignment_custody_identity: String,
    },
    Local {
        authenticated_user_id: i64,
        file_id: i64,
    },
    FileGrant {
        verified_grant_id: Id,
        user_id: i64,
        file_id: i64,
    },
}
impl DirectSourceProvenanceV1 {
    pub(crate) fn binding(&self) -> Result<String, CustodyError> {
        match self {
            Self::Sharing {
                assignment_custody_identity,
            } => {
                if assignment_custody_identity.len() != 64
                    || !assignment_custody_identity
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(codec::invalid());
                }
                Ok(assignment_custody_identity.clone())
            }
            Self::Local {
                authenticated_user_id,
                file_id,
            } => {
                if *authenticated_user_id <= 0 || *file_id <= 0 {
                    return Err(codec::invalid());
                }
                use sha2::{Digest, Sha256};
                let mut digest = Sha256::new();
                digest.update(b"plurx.direct-local-source.v1\0");
                digest.update(authenticated_user_id.to_be_bytes());
                digest.update(file_id.to_be_bytes());
                Ok(format!("{:x}", digest.finalize()))
            }
            Self::FileGrant {
                verified_grant_id,
                user_id,
                file_id,
            } => {
                if *user_id <= 0 || *file_id <= 0 {
                    return Err(codec::invalid());
                }
                use sha2::{Digest, Sha256};
                let mut digest = Sha256::new();
                digest.update(b"plurx.direct-file-grant.v1\0");
                digest.update(verified_grant_id.uuid().as_bytes());
                digest.update(user_id.to_be_bytes());
                digest.update(file_id.to_be_bytes());
                Ok(format!("{:x}", digest.finalize()))
            }
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FileType {
    Regular,
    Directory,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeObject {
    pub file_type: FileType,
    pub device: NativeValue<u64>,
    pub inode: NativeValue<u64>,
    pub mount_id: NativeValue<u64>,
    pub filesystem_domain: FilesystemDomain,
}
impl NativeObject {
    fn validate(&self) -> Result<(), CustodyError> {
        self.device.validate()?;
        self.inode.validate()?;
        self.mount_id.validate()?;
        self.filesystem_domain.validate()
    }
    pub(super) fn resident_heap_bytes(&self) -> Result<u64, CustodyError> {
        let mut bytes = self
            .device
            .resident_heap_bytes()?
            .checked_add(self.inode.resident_heap_bytes()?)
            .ok_or_else(codec::invalid)?;
        bytes = bytes
            .checked_add(self.mount_id.resident_heap_bytes()?)
            .ok_or_else(codec::invalid)?;
        bytes
            .checked_add(self.filesystem_domain.resident_heap_bytes()?)
            .ok_or_else(codec::invalid)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceObserved {
    pub version: u32,
    pub holder_id: Id,
    pub member_id: Id,
    pub copies: Vec<SourceCopy>,
    pub native_object: NativeObject,
    pub access: SourceAccess,
    pub source_association: SourceAssociation,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemberSlot {
    pub member_id: Id,
    pub owned_by: Option<Id>,
    pub depends_on: Vec<Id>,
    pub protected_paths: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OwnerAllocation {
    pub owner_id: Id,
    pub memory_bytes: u64,
    pub handles: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IoInventory {
    pub pending_io: Vec<OwnerAllocation>,
    pub buffers: Vec<OwnerAllocation>,
    pub readers: Vec<OwnerAllocation>,
}
impl IoInventory {
    fn validate(&self) -> Result<(), CustodyError> {
        let mut ids = BTreeSet::new();
        for owners in [&self.pending_io, &self.buffers, &self.readers] {
            if owners.len() > codec::ACTIVE_LIMIT {
                return Err(codec::invalid());
            }
            for owner in owners {
                if !ids.insert(owner.owner_id) {
                    return Err(codec::invalid());
                }
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum VodActor {
    VodGenerationV1,
    RollingCopyV1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VodOutputTask {
    pub version: u32,
    pub actor: VodActor,
    pub graph_id: Id,
    pub member_id: Id,
    pub root_member_id: Id,
    pub generation_epoch: u64,
    pub source_association: SourceAssociation,
    pub stdout_owner_id: Id,
    pub diagnostic_owner_id: Id,
    pub asset_id: Id,
    pub sink_owner_id: Id,
    pub inventory: IoInventory,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AssetFile {
    pub file_id: Id,
    pub relative_path: String,
    pub native_object: NativeObject,
    pub allocated_bytes: u64,
    pub charged_bytes: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScratchAssets {
    pub version: u32,
    pub asset_id: Id,
    pub directory_path: String,
    pub directory_object: NativeObject,
    pub files: Vec<AssetFile>,
    pub inventory: IoInventory,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub(crate) enum MemberPlan {
    LegacyV1 {
        planned: PlannedMember,
    },
    SourceV1 {
        slot: MemberSlot,
        source: SourcePlan,
    },
    DirectSourceV1 {
        slot: MemberSlot,
        source: DirectSourcePlanV1,
    },
    FixedProbeDiscoverSourceV1 {
        slot: MemberSlot,
        source: FixedProbeDiscoverSourceV1,
    },
    DirectReadDomainV1 {
        slot: MemberSlot,
        domain: DirectReadDomainPlanV1,
    },
    VodOutputTaskV1 {
        slot: MemberSlot,
        task: VodOutputTask,
    },
    FixedProbeTaskV1 {
        slot: MemberSlot,
        task: FixedProbeTaskV1,
    },
    FixedProbeDiscoverTaskV1 {
        slot: MemberSlot,
        task: FixedProbeDiscoverTaskV1,
    },
    FixedPlanningEngineTaskV1 {
        slot: MemberSlot,
        task: FixedPlanningEngineTaskV1,
    },
    FixedProbeMetadataV1 {
        slot: MemberSlot,
        metadata: FixedProbeMetadataV1,
    },
    ScratchAssetsV1 {
        slot: MemberSlot,
        scratch: ScratchAssets,
    },
    ScratchAssetsV2 {
        slot: MemberSlot,
        plan: scratch::ScratchPlanV2,
    },
    ScratchAssetsV3 {
        slot: MemberSlot,
        plan: scratch::bootstrap::ScratchPlanV3,
    },
}
impl MemberPlan {
    pub(crate) fn member_id(&self) -> Uuid {
        match self {
            Self::LegacyV1 { planned } => planned.member_id,
            Self::SourceV1 { slot, .. }
            | Self::DirectSourceV1 { slot, .. }
            | Self::FixedProbeDiscoverSourceV1 { slot, .. }
            | Self::DirectReadDomainV1 { slot, .. }
            | Self::VodOutputTaskV1 { slot, .. }
            | Self::FixedProbeTaskV1 { slot, .. }
            | Self::FixedProbeDiscoverTaskV1 { slot, .. }
            | Self::FixedPlanningEngineTaskV1 { slot, .. }
            | Self::FixedProbeMetadataV1 { slot, .. }
            | Self::ScratchAssetsV1 { slot, .. }
            | Self::ScratchAssetsV2 { slot, .. }
            | Self::ScratchAssetsV3 { slot, .. } => slot.member_id.uuid(),
        }
    }
    pub(crate) fn kind(&self) -> MemberKind {
        match self {
            Self::LegacyV1 { planned } => planned.kind.clone(),
            Self::VodOutputTaskV1 { .. }
            | Self::FixedProbeTaskV1 { .. }
            | Self::FixedProbeDiscoverTaskV1 { .. } => MemberKind::Task,
            Self::FixedPlanningEngineTaskV1 { .. } => MemberKind::Task,
            _ => MemberKind::TypedResource,
        }
    }
    pub(crate) fn adapter(&self) -> &'static str {
        match self {
            Self::LegacyV1 { planned } => planned.adapter.as_str(),
            Self::SourceV1 { .. } => "source_file",
            Self::DirectSourceV1 { .. } => "direct_source_file",
            Self::FixedProbeDiscoverSourceV1 { .. } => "fixed_probe_discover_source",
            Self::DirectReadDomainV1 { .. } => "direct_read_domain",
            Self::VodOutputTaskV1 { .. } => "vod_output_task",
            Self::FixedProbeTaskV1 { .. } => "fixed_probe_task",
            Self::FixedProbeDiscoverTaskV1 { .. } => "fixed_probe_discover_task",
            Self::FixedPlanningEngineTaskV1 { .. } => "fixed_planning_engine_task",
            Self::FixedProbeMetadataV1 { .. } => "fixed_probe_metadata",
            Self::ScratchAssetsV1 { .. }
            | Self::ScratchAssetsV2 { .. }
            | Self::ScratchAssetsV3 { .. } => "scratch",
        }
    }
    fn edges(&self) -> Vec<Uuid> {
        match self {
            Self::LegacyV1 { planned } => planned
                .depends_on
                .iter()
                .copied()
                .chain(planned.owned_by)
                .collect(),
            Self::SourceV1 { slot, .. }
            | Self::DirectSourceV1 { slot, .. }
            | Self::FixedProbeDiscoverSourceV1 { slot, .. }
            | Self::DirectReadDomainV1 { slot, .. }
            | Self::VodOutputTaskV1 { slot, .. }
            | Self::FixedProbeTaskV1 { slot, .. }
            | Self::FixedProbeDiscoverTaskV1 { slot, .. }
            | Self::FixedPlanningEngineTaskV1 { slot, .. }
            | Self::FixedProbeMetadataV1 { slot, .. }
            | Self::ScratchAssetsV1 { slot, .. }
            | Self::ScratchAssetsV2 { slot, .. }
            | Self::ScratchAssetsV3 { slot, .. } => slot
                .depends_on
                .iter()
                .copied()
                .chain(slot.owned_by)
                .map(Id::uuid)
                .collect(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    pub version: u32,
    pub graph_id: Id,
    pub owner_id: Id,
    pub attempt_id: Option<Id>,
    pub members: Vec<MemberPlan>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
#[expect(
    clippy::large_enum_variant,
    reason = "Envelope-vector capacity includes this complete inline observation layout; boxing the frozen scratch state adds a separate allocation to the existing owner model."
)]
pub(crate) enum Observed {
    LegacyV1 {
        identity: super::ObservedIdentity,
    },
    SourceV1 {
        source: SourceObserved,
    },
    DirectSourceV1 {
        source: SourceObserved,
    },
    FixedProbeDiscoverSourceV1 {
        source: SourceObserved,
    },
    VodOutputTaskV1 {
        task: VodOutputTask,
    },
    FixedProbeTaskV1 {
        task: FixedProbeTaskV1,
    },
    FixedProbeDiscoverTaskV1 {
        task: FixedProbeDiscoverTaskObservedV1,
    },
    FixedPlanningEngineTaskV1 {
        task: FixedPlanningEngineTaskObservedV1,
    },
    FixedPlanningEngineTaskV2 {
        task: FixedPlanningEngineTaskObservedV2,
    },
    FixedProbeMetadataV1 {
        metadata: FixedProbeMetadataV1,
    },
    ScratchAssetsV1 {
        scratch: ScratchAssets,
    },
    ScratchAssetsV2 {
        state: scratch::ScratchStateV2,
    },
    ScratchAssetsV3 {
        state: scratch::bootstrap::ScratchStateV3,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Envelope {
    pub version: u32,
    pub planned: MemberPlan,
    pub observed: Option<Observed>,
}
/// Synchronous audit view of the SAME retained plan and observation. It has
/// the exact Envelope wire shape but owns no plan/observation allocation.
#[derive(Clone, Copy, Serialize)]
pub(crate) struct EnvelopeRef<'a> {
    pub version: u32,
    pub planned: &'a MemberPlan,
    pub observed: &'a Option<Observed>,
}
impl<'a> From<&'a Envelope> for EnvelopeRef<'a> {
    fn from(value: &'a Envelope) -> Self {
        Self {
            version: value.version,
            planned: &value.planned,
            observed: &value.observed,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ManifestDocument {
    // Not used on disk as a wrapper: decode_manifest selects the actual version.
    V1(super::Manifest),
    V2(Manifest),
}

pub(crate) fn decode_manifest(json: &str) -> Result<ManifestDocument, CustodyError> {
    #[derive(Deserialize)]
    struct Version {
        version: u32,
    }
    // Only the discriminator read is permissive; the selected complete decoder
    // then rejects unknown fields/variants. No v1 payload is reinterpreted as v2.
    match codec::decode::<Version>(json)?.version {
        1 => {
            let value: super::Manifest = codec::decode(json)?;
            codec::validate_manifest(&value)?;
            Ok(ManifestDocument::V1(value))
        }
        2 => {
            let value: Manifest = codec::decode(json)?;
            validate_manifest(&value)?;
            Ok(ManifestDocument::V2(value))
        }
        _ => Err(codec::invalid()),
    }
}

fn validate_slot(slot: &MemberSlot) -> Result<(), CustodyError> {
    if slot.depends_on.len() > 129 || slot.protected_paths.len() > 16 {
        return Err(codec::invalid());
    }
    let unique: BTreeSet<_> = slot.depends_on.iter().collect();
    if unique.len() != slot.depends_on.len() {
        return Err(codec::invalid());
    }
    for path in &slot.protected_paths {
        codec::long_text(path)?;
        if !std::path::Path::new(path).is_absolute() {
            return Err(codec::invalid());
        }
    }
    Ok(())
}
fn validate_copies(copies: &[SourceCopy]) -> Result<(), CustodyError> {
    if copies.is_empty() || copies.len() > 16 {
        return Err(codec::invalid());
    }
    let ids: BTreeSet<_> = copies.iter().map(|c| c.copy_id).collect();
    let operations: BTreeSet<_> = copies.iter().map(|c| c.acquisition_operation_id).collect();
    if ids.len() != copies.len() || operations.len() != copies.len() {
        return Err(codec::invalid());
    }
    let mut done = BTreeSet::new();
    loop {
        let before = done.len();
        for copy in copies {
            if copy
                .parent_copy_id
                .is_none_or(|parent| done.contains(&parent))
            {
                done.insert(copy.copy_id);
            }
        }
        if done.len() == copies.len() {
            return Ok(());
        }
        if done.len() == before {
            return Err(codec::invalid());
        }
    }
}
fn validate_direct_source(
    source: &DirectSourcePlanV1,
    manifest: &Manifest,
) -> Result<(), CustodyError> {
    if source.version != 1
        || source.scanner_size < 0
        || !std::path::Path::new(&source.absolute_path).is_absolute()
        || source.planned_copies.len() != 1
        || source.recipe_source_binding != source.provenance.binding()?
    {
        return Err(codec::invalid());
    }
    match (&source.provenance, &source.expectation) {
        (DirectSourceProvenanceV1::Sharing { .. }, DirectSourceExpectationV1::ScannerExact)
        | (
            DirectSourceProvenanceV1::Local { .. },
            DirectSourceExpectationV1::AuthorizedLocalPath,
        )
        | (
            DirectSourceProvenanceV1::FileGrant { .. },
            DirectSourceExpectationV1::AuthorizedLocalPath,
        ) => {}
        _ => return Err(codec::invalid()),
    }
    for value in [
        &source.media_file_id,
        &source.recipe_source_binding,
        &source.absolute_path,
    ] {
        codec::long_text(value)?;
    }
    match &source.provenance {
        DirectSourceProvenanceV1::Local { file_id, .. }
        | DirectSourceProvenanceV1::FileGrant { file_id, .. }
            if source.media_file_id != file_id.to_string() =>
        {
            return Err(codec::invalid())
        }
        _ => {}
    }
    source.filesystem_domain.validate()?;
    validate_copies(&source.planned_copies)?;
    let SourceRecovery::LocalSourceHandlesV1 {
        owner_id,
        graph_id,
        filesystem_domain,
    } = &source.recovery;
    let copy = &source.planned_copies[0];
    if *owner_id != manifest.owner_id
        || *graph_id != manifest.graph_id
        || filesystem_domain != &source.filesystem_domain
        || source.filesystem_domain.daemon_owner_id != manifest.owner_id
        || !matches!(copy.owner,
            CopyOwner::DaemonSourceCell { owner_id: copy_owner, graph_id: copy_graph, member_id }
                if copy_owner == *owner_id && copy_graph == *graph_id && member_id == source.member_id)
        || copy.parent_copy_id.is_some()
    {
        return Err(codec::invalid());
    }
    Ok(())
}

fn validate_source(source: &SourcePlan, manifest: &Manifest) -> Result<(), CustodyError> {
    if source.version != 1 {
        return Err(codec::invalid());
    }
    source.source_association.validate()?;
    source.filesystem_domain.validate()?;
    validate_copies(&source.planned_copies)?;
    let SourceRecovery::LocalSourceHandlesV1 {
        owner_id,
        graph_id,
        filesystem_domain,
    } = &source.recovery;
    if *owner_id != manifest.owner_id
        || *graph_id != manifest.graph_id
        || filesystem_domain != &source.filesystem_domain
        || source.filesystem_domain.daemon_owner_id != manifest.owner_id
    {
        return Err(codec::invalid());
    }
    for copy in &source.planned_copies {
        match copy.owner {
            CopyOwner::DaemonSourceCell {
                owner_id,
                graph_id,
                member_id,
            } if owner_id == manifest.owner_id
                && graph_id == manifest.graph_id
                && member_id == source.member_id => {}
            CopyOwner::OwnedRoot {
                graph_id,
                root_member_id,
            } if graph_id == manifest.graph_id
                && manifest.members.iter().any(|m| {
                    m.member_id() == root_member_id.uuid() && m.kind() == MemberKind::Root
                }) => {}
            CopyOwner::RetainedAssets {
                source_member_id, ..
            } if source_member_id == source.member_id => {}
            CopyOwner::RetainedRoleHolder { .. } => {} // Actual edge/charge checked transactionally.
            _ => return Err(codec::invalid()),
        }
    }
    Ok(())
}
fn validate_task(task: &VodOutputTask, manifest: &Manifest) -> Result<(), CustodyError> {
    let scratch_matches = manifest
        .members
        .iter()
        .any(|member| match (&task.actor, member) {
            (VodActor::VodGenerationV1, MemberPlan::ScratchAssetsV1 { scratch, .. }) => {
                scratch.asset_id == task.asset_id
            }
            (VodActor::VodGenerationV1, MemberPlan::ScratchAssetsV2 { plan, .. }) => {
                plan.asset_id == task.asset_id
            }
            (VodActor::VodGenerationV1, MemberPlan::ScratchAssetsV3 { plan, .. }) => {
                plan.asset_id == task.asset_id
            }
            (VodActor::RollingCopyV1, MemberPlan::ScratchAssetsV3 { plan, .. }) => {
                plan.asset_id == task.asset_id
                    && plan.generation_epoch == task.generation_epoch
                    && matches!(plan.outputs.naming, scratch::OutputNamingV1::CopyRollingV1)
            }
            _ => false,
        });
    if task.version != 1 || task.graph_id != manifest.graph_id
        || task.stdout_owner_id == task.diagnostic_owner_id
        || !manifest.members.iter().any(|m| matches!(m, MemberPlan::SourceV1 { source, .. } if source.source_association == task.source_association))
        || !scratch_matches
        || !manifest.members.iter().any(|m| m.member_id() == task.root_member_id.uuid() && m.kind() == MemberKind::Root) { return Err(codec::invalid()); }
    task.source_association.validate()?;
    task.inventory.validate()
}
fn validate_scratch(scratch: &ScratchAssets) -> Result<(), CustodyError> {
    if scratch.version != 1
        || scratch.directory_object.file_type != FileType::Directory
        || scratch.files.len() > codec::ACTIVE_LIMIT
    {
        return Err(codec::invalid());
    }
    codec::long_text(&scratch.directory_path)?;
    if !std::path::Path::new(&scratch.directory_path).is_absolute() {
        return Err(codec::invalid());
    }
    scratch.directory_object.validate()?;
    scratch.inventory.validate()?;
    let mut ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for file in &scratch.files {
        codec::long_text(&file.relative_path)?;
        let path = std::path::Path::new(&file.relative_path);
        if !path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
            || file.native_object.file_type != FileType::Regular
            || file.native_object.filesystem_domain != scratch.directory_object.filesystem_domain
            || file.charged_bytes < file.allocated_bytes
            || !ids.insert(file.file_id)
            || !paths.insert(&file.relative_path)
        {
            return Err(codec::invalid());
        }
        file.native_object.validate()?;
    }
    Ok(())
}
pub(crate) fn validate_manifest(value: &Manifest) -> Result<(), CustodyError> {
    if value.version != 2 || value.members.is_empty() || value.members.len() > 129 {
        return Err(codec::invalid());
    }
    let ids: BTreeSet<_> = value.members.iter().map(MemberPlan::member_id).collect();
    if ids.len() != value.members.len() {
        return Err(codec::invalid());
    }
    let mut counts = [0usize; 4];
    for member in &value.members {
        counts[match member.kind() {
            MemberKind::Containment => 0,
            MemberKind::Root => 1,
            MemberKind::Task => 2,
            MemberKind::TypedResource => 3,
        }] += 1;
        match member {
            MemberPlan::LegacyV1 { planned } => {
                // Reuse unchanged v1 member validation; graph edges are checked
                // against the actual mixed graph below, never synthesized IDs.
                if planned.depends_on.iter().collect::<BTreeSet<_>>().len()
                    != planned.depends_on.len()
                {
                    return Err(codec::invalid());
                }
                let mut legacy = super::Manifest {
                    version: 1,
                    graph_id: value.graph_id.uuid(),
                    owner_id: value.owner_id.uuid(),
                    attempt_id: value.attempt_id.map(Id::uuid),
                    members: Vec::new(),
                };
                legacy.members.push(PlannedMember {
                    owned_by: None,
                    depends_on: Vec::new(),
                    ..planned.clone()
                });
                codec::validate_manifest(&legacy)?;
            }
            MemberPlan::SourceV1 { slot, source } => {
                validate_slot(slot)?;
                if slot.member_id != source.member_id {
                    return Err(codec::invalid());
                }
                validate_source(source, value)?;
            }
            MemberPlan::DirectSourceV1 { slot, source } => {
                validate_slot(slot)?;
                if slot.member_id != source.member_id
                    || slot.owned_by.is_some()
                    || !slot.depends_on.is_empty()
                    || slot.protected_paths.as_slice() != [source.absolute_path.as_str()]
                {
                    return Err(codec::invalid());
                }
                validate_direct_source(source, value)?;
            }
            MemberPlan::FixedProbeDiscoverSourceV1 { slot, source } => {
                validate_slot(slot)?;
                if slot.member_id != source.member_id
                    || slot.owned_by.is_some()
                    || !slot.depends_on.is_empty()
                    || slot.protected_paths.as_slice() != [source.absolute_path.as_str()]
                {
                    return Err(codec::invalid());
                }
                fixed_probe::validate_discover_source(source, value)?;
            }
            MemberPlan::DirectReadDomainV1 { slot, domain } => {
                validate_slot(slot)?;
                if domain.version != 1
                    || domain.maximum_parallel_reads != 1
                    || domain.owner_id != value.owner_id
                    || domain.graph_id != value.graph_id
                    || slot.owned_by.is_some()
                    || !slot.depends_on.is_empty()
                    || !slot.protected_paths.is_empty()
                {
                    return Err(codec::invalid());
                }
            }
            MemberPlan::VodOutputTaskV1 { slot, task } => {
                validate_slot(slot)?;
                if slot.member_id != task.member_id {
                    return Err(codec::invalid());
                }
                validate_task(task, value)?;
            }
            MemberPlan::FixedProbeTaskV1 { slot, task } => {
                validate_slot(slot)?;
                if slot.member_id != task.member_id
                    || slot.owned_by != Some(task.root_member_id)
                    || !slot.depends_on.contains(&task.source_member_id)
                    || !slot.depends_on.contains(&task.executable_member_id)
                    || !slot.protected_paths.is_empty()
                {
                    return Err(codec::invalid());
                }
                fixed_probe::validate_task(task, value)?;
            }
            MemberPlan::FixedProbeDiscoverTaskV1 { slot, task } => {
                validate_slot(slot)?;
                if slot.member_id != task.member_id
                    || slot.owned_by != Some(task.document_root_member_id)
                    || !slot.depends_on.contains(&task.version_root_member_id)
                    || !slot.depends_on.contains(&task.source_member_id)
                    || !slot.protected_paths.is_empty()
                {
                    return Err(codec::invalid());
                }
                fixed_probe::validate_discover_task(task, value)?;
            }
            MemberPlan::FixedPlanningEngineTaskV1 { slot, task } => {
                validate_slot(slot)?;
                if slot.member_id != task.member_id
                    || slot.owned_by != Some(task.dependency_root_member_id)
                    || !slot.depends_on.contains(&task.version_root_member_id)
                    || !slot.protected_paths.is_empty()
                {
                    return Err(codec::invalid());
                }
                fixed_probe::validate_engine_task(task, value)?;
            }
            MemberPlan::FixedProbeMetadataV1 { slot, metadata } => {
                validate_slot(slot)?;
                if slot.member_id != metadata.member_id
                    || slot.owned_by.is_some()
                    || !slot.depends_on.is_empty()
                    || !slot.protected_paths.is_empty()
                {
                    return Err(codec::invalid());
                }
                fixed_probe::validate_metadata(metadata)?;
            }
            MemberPlan::ScratchAssetsV1 { slot, scratch } => {
                validate_slot(slot)?;
                if !slot.protected_paths.contains(&scratch.directory_path) {
                    return Err(codec::invalid());
                }
                validate_scratch(scratch)?;
            }
            MemberPlan::ScratchAssetsV2 { slot, plan } => {
                validate_slot(slot)?;
                scratch::validate_plan(plan)?;
                if !slot
                    .protected_paths
                    .contains(&plan.directory.protected_absolute_path)
                    || plan.filesystem_domain.daemon_owner_id != value.owner_id
                    || value
                        .members
                        .iter()
                        .filter(|m| match m {
                            MemberPlan::ScratchAssetsV1 { scratch, .. } => {
                                scratch.asset_id == plan.asset_id
                            }
                            MemberPlan::ScratchAssetsV2 { plan: other, .. } => {
                                other.asset_id == plan.asset_id
                            }
                            MemberPlan::ScratchAssetsV3 { plan: other, .. } => {
                                other.asset_id == plan.asset_id
                            }
                            _ => false,
                        })
                        .count()
                        != 1
                {
                    return Err(codec::invalid());
                }
            }
            MemberPlan::ScratchAssetsV3 { slot, plan } => {
                validate_slot(slot)?;
                scratch::bootstrap::validate_plan(plan)?;
                if !slot
                    .protected_paths
                    .contains(&plan.bootstrap().protected_child_path)
                    || plan.filesystem_domain.daemon_owner_id != value.owner_id
                    || value
                        .members
                        .iter()
                        .filter(|m| match m {
                            MemberPlan::ScratchAssetsV1 { scratch, .. } => {
                                scratch.asset_id == plan.asset_id
                            }
                            MemberPlan::ScratchAssetsV2 { plan: other, .. } => {
                                other.asset_id == plan.asset_id
                            }
                            MemberPlan::ScratchAssetsV3 { plan: other, .. } => {
                                other.asset_id == plan.asset_id
                            }
                            _ => false,
                        })
                        .count()
                        != 1
                {
                    return Err(codec::invalid());
                }
            }
        }
        let edges = member.edges();
        if edges.len() > 130
            || edges
                .iter()
                .any(|id| *id == member.member_id() || !ids.contains(id))
        {
            return Err(codec::invalid());
        }
    }
    if counts[0] > 1 || counts[1] > 32 || counts[2] > 32 || counts[3] > 64 {
        return Err(codec::invalid());
    }
    let mut done = BTreeSet::new();
    loop {
        let before = done.len();
        for member in &value.members {
            if member.edges().iter().all(|id| done.contains(id)) {
                done.insert(member.member_id());
            }
        }
        if done.len() == value.members.len() {
            break;
        }
        if done.len() == before {
            return Err(codec::invalid());
        }
    }
    codec::encode(value)?;
    Ok(())
}
pub(crate) fn validate_envelope(
    manifest: &Manifest,
    envelope: &Envelope,
) -> Result<(), CustodyError> {
    validate_envelope_members(&manifest.members, envelope)
}

/// Preserve the complete envelope checks and serialized bound while borrowing
/// the SAME retained plan slice; no duplicate Manifest or new authority.
pub(crate) fn validate_envelope_members(
    members: &[MemberPlan],
    envelope: &Envelope,
) -> Result<(), CustodyError> {
    validate_envelope_members_ref(members, envelope.into())
}

pub(crate) fn validate_envelope_members_ref(
    members: &[MemberPlan],
    envelope: EnvelopeRef<'_>,
) -> Result<(), CustodyError> {
    if envelope.version != 2 || !members.contains(envelope.planned) {
        return Err(codec::invalid());
    }
    match (envelope.planned, envelope.observed) {
        (_, None) => {}
        (MemberPlan::LegacyV1 { planned }, Some(Observed::LegacyV1 { identity })) => {
            codec::validate_observed(planned, identity)?
        }
        (MemberPlan::SourceV1 { source, .. }, Some(Observed::SourceV1 { source: observed })) => {
            if observed.version != 1
                || observed.holder_id != source.holder_id
                || observed.member_id != source.member_id
                || observed.access != source.access
                || observed.source_association != source.source_association
                || observed.native_object.file_type != FileType::Regular
                || observed.native_object.filesystem_domain != source.filesystem_domain
                || observed.copies.is_empty()
                || observed
                    .copies
                    .iter()
                    .any(|copy| !source.planned_copies.contains(copy))
            {
                return Err(codec::invalid());
            }
            validate_copies(&observed.copies)?;
            observed.native_object.validate()?;
        }
        (
            MemberPlan::FixedProbeDiscoverSourceV1 { source, .. },
            Some(Observed::FixedProbeDiscoverSourceV1 { source: observed }),
        ) => {
            if observed.version != 1
                || observed.holder_id != source.holder_id
                || observed.member_id != source.member_id
                || observed.access != SourceAccess::ReadOnlyRegular
                || observed.source_association.media_file_id != source.media_file_id
                || observed.source_association.recipe_source_binding != source.recipe_source_binding
                || observed.native_object.file_type != FileType::Regular
                || observed.native_object.filesystem_domain != source.filesystem_domain
                // The first durable observation is the original parent File.
                // A second observation adds the child only after the sealed
                // root has inherited it. Closure still requires BOTH planned
                // copies, even if the root was never entered.
                || !matches!(observed.copies.as_slice(), [parent]
                    if Some(parent) == source.planned_copies.first())
                    && observed.copies != source.planned_copies
                || !direct_s1_matches(
                    &observed.source_association.object_version,
                    &observed.native_object,
                    Some(source.scanner_size),
                )
            {
                return Err(codec::invalid());
            }
            validate_copies(&observed.copies)?;
            observed.native_object.validate()?;
        }
        (
            MemberPlan::DirectSourceV1 { source, .. },
            Some(Observed::DirectSourceV1 { source: observed }),
        ) => {
            if observed.version != 1
                || observed.holder_id != source.holder_id
                || observed.member_id != source.member_id
                || observed.access != source.access
                || observed.source_association.media_file_id != source.media_file_id
                || observed.source_association.recipe_source_binding != source.recipe_source_binding
                || observed.native_object.file_type != FileType::Regular
                || observed.native_object.filesystem_domain != source.filesystem_domain
                || observed.copies != source.planned_copies
                || !direct_s1_matches(
                    &observed.source_association.object_version,
                    &observed.native_object,
                    match source.expectation {
                        DirectSourceExpectationV1::ScannerExact => Some(source.scanner_size),
                        DirectSourceExpectationV1::AuthorizedLocalPath => None,
                    },
                )
            {
                return Err(codec::invalid());
            }
            observed.native_object.validate()?;
        }
        (
            MemberPlan::VodOutputTaskV1 { task, .. },
            Some(Observed::VodOutputTaskV1 { task: observed }),
        ) if task == observed => {}
        (
            MemberPlan::FixedProbeTaskV1 { task, .. },
            Some(Observed::FixedProbeTaskV1 { task: observed }),
        ) if task == observed => {}
        (
            MemberPlan::FixedProbeDiscoverTaskV1 { task, .. },
            Some(Observed::FixedProbeDiscoverTaskV1 { task: observed }),
        ) => fixed_probe::validate_discover_task_observed(task, observed)?,
        (
            MemberPlan::FixedPlanningEngineTaskV1 { task, .. },
            Some(Observed::FixedPlanningEngineTaskV1 { task: observed }),
        ) => fixed_probe::validate_engine_task_observed(task, observed)?,
        (
            MemberPlan::FixedPlanningEngineTaskV1 { task, .. },
            Some(Observed::FixedPlanningEngineTaskV2 { task: observed }),
        ) => fixed_probe::validate_engine_task_observed_v2(task, observed)?,
        (
            MemberPlan::FixedProbeMetadataV1 { metadata, .. },
            Some(Observed::FixedProbeMetadataV1 { metadata: observed }),
        ) if metadata == observed => {}
        (
            MemberPlan::ScratchAssetsV1 { scratch, .. },
            Some(Observed::ScratchAssetsV1 { scratch: observed }),
        ) if scratch == observed => {}
        (MemberPlan::ScratchAssetsV2 { plan, .. }, Some(Observed::ScratchAssetsV2 { state })) => {
            scratch::validate_projection(plan, state)?
        }
        (MemberPlan::ScratchAssetsV3 { plan, .. }, Some(Observed::ScratchAssetsV3 { state })) => {
            scratch::bootstrap::validate_projection(plan, state)?
        }
        _ => return Err(codec::invalid()),
    }
    fixed_probe::validate_bounded_audit_size(&envelope)?;
    Ok(())
}

fn direct_s1_matches(identity: &str, native: &NativeObject, scanner_size: Option<i64>) -> bool {
    let mut fields = identity.split(':');
    if fields.next() != Some("s1") {
        return false;
    }
    let parsed = (|| {
        Some(crate::source_identity::SourceIdentity::from_fields(
            fields.next()?.parse().ok()?,
            fields.next()?.parse().ok()?,
            fields.next()?.parse().ok()?,
            fields.next()?.parse().ok()?,
            fields.next()?.parse().ok()?,
            fields.next()?.parse().ok()?,
            fields.next()?.parse().ok()?,
        ))
    })();
    let Some(parsed) = parsed else { return false };
    fields.next().is_none()
        && parsed.render_s1() == identity
        && native.device == (NativeValue::Known { value: parsed.dev })
        && native.inode == (NativeValue::Known { value: parsed.ino })
        && scanner_size.is_none_or(|size| i64::try_from(parsed.size).ok() == Some(size))
}

// Preserve the exact legacy envelope type as well as the strict version selector.
pub(in crate::resource_custody) fn decode_legacy_envelope(
    json: &str,
) -> Result<MemberEnvelope, CustodyError> {
    let value: MemberEnvelope = codec::decode(json)?;
    if value.version != 1 {
        return Err(codec::invalid());
    }
    Ok(value)
}
