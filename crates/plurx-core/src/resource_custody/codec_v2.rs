//! Strict v2 audit data for the concrete finite-VOD ownership path.
//! None of these disk types constructs a physical receipt or a launch permit.
mod cost;
mod settlement;
#[cfg(test)]
pub(in crate::resource_custody) mod tests;
use super::{codec, CustodyError, Role};
use codec::{BootIdentity, MemberEnvelope, MemberKind, PlannedMember};
pub(crate) use cost::{retained_minimum_resources, validate_retained_resources};
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
    VodOutputTaskV1 {
        slot: MemberSlot,
        task: VodOutputTask,
    },
    ScratchAssetsV1 {
        slot: MemberSlot,
        scratch: ScratchAssets,
    },
}
impl MemberPlan {
    pub(crate) fn member_id(&self) -> Uuid {
        match self {
            Self::LegacyV1 { planned } => planned.member_id,
            Self::SourceV1 { slot, .. }
            | Self::VodOutputTaskV1 { slot, .. }
            | Self::ScratchAssetsV1 { slot, .. } => slot.member_id.uuid(),
        }
    }
    pub(crate) fn kind(&self) -> MemberKind {
        match self {
            Self::LegacyV1 { planned } => planned.kind.clone(),
            Self::VodOutputTaskV1 { .. } => MemberKind::Task,
            _ => MemberKind::TypedResource,
        }
    }
    pub(crate) fn adapter(&self) -> &'static str {
        match self {
            Self::LegacyV1 { planned } => planned.adapter.as_str(),
            Self::SourceV1 { .. } => "source_file",
            Self::VodOutputTaskV1 { .. } => "vod_output_task",
            Self::ScratchAssetsV1 { .. } => "scratch",
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
            | Self::VodOutputTaskV1 { slot, .. }
            | Self::ScratchAssetsV1 { slot, .. } => slot
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
pub(crate) enum Observed {
    LegacyV1 { identity: super::ObservedIdentity },
    SourceV1 { source: SourceObserved },
    VodOutputTaskV1 { task: VodOutputTask },
    ScratchAssetsV1 { scratch: ScratchAssets },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Envelope {
    pub version: u32,
    pub planned: MemberPlan,
    pub observed: Option<Observed>,
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
    if task.version != 1 || task.graph_id != manifest.graph_id
        || task.stdout_owner_id == task.diagnostic_owner_id
        || !manifest.members.iter().any(|m| matches!(m, MemberPlan::SourceV1 { source, .. } if source.source_association == task.source_association))
        || !manifest.members.iter().any(|m| matches!(m, MemberPlan::ScratchAssetsV1 { scratch, .. } if scratch.asset_id == task.asset_id))
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
            MemberPlan::VodOutputTaskV1 { slot, task } => {
                validate_slot(slot)?;
                if slot.member_id != task.member_id {
                    return Err(codec::invalid());
                }
                validate_task(task, value)?;
            }
            MemberPlan::ScratchAssetsV1 { slot, scratch } => {
                validate_slot(slot)?;
                if !slot.protected_paths.contains(&scratch.directory_path) {
                    return Err(codec::invalid());
                }
                validate_scratch(scratch)?;
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
    if envelope.version != 2 || !manifest.members.contains(&envelope.planned) {
        return Err(codec::invalid());
    }
    match (&envelope.planned, &envelope.observed) {
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
            MemberPlan::VodOutputTaskV1 { task, .. },
            Some(Observed::VodOutputTaskV1 { task: observed }),
        ) if task == observed => {}
        (
            MemberPlan::ScratchAssetsV1 { scratch, .. },
            Some(Observed::ScratchAssetsV1 { scratch: observed }),
        ) if scratch == observed => {}
        _ => return Err(codec::invalid()),
    }
    codec::encode(envelope)?;
    Ok(())
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
