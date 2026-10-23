//! Private, bounded v1 disk codecs. Disk data never becomes a capability.
use super::{CustodyError, ErrorKind};
use crate::adaptive_resources::ResourceVector;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use uuid::Uuid;

pub const JSON_LIMIT: usize = 64 * 1024;
pub const ACTIVE_LIMIT: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "state", rename_all = "snake_case")]
pub enum BootIdentity {
    Known {
        value: Uuid,
        provider: String,
        version: u32,
    },
    Unknown {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    pub version: u32,
    pub pid: u32,
    /// Native creation marker, never a sampled wall-clock timestamp.
    pub creation_marker: Option<u64>,
    pub domain: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformDomain {
    pub version: u32,
    pub platform: String,
    pub host_identity: Option<String>,
    pub namespace: Option<String>,
    /// Unknown continuity is explicit; a node UUID is not this identity.
    pub continuity_provider: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberKind {
    Containment,
    Root,
    Task,
    TypedResource,
}
impl MemberKind {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            Self::Containment => "containment",
            Self::Root => "root",
            Self::Task => "task",
            Self::TypedResource => "typed_resource",
        }
    }
}

/// Recognized document codecs, NOT assertions that the platform is supported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Adapter {
    LinuxCgroupV2,
    WindowsJob,
    ProcessRoot,
    RootPipeTask,
    Scratch,
    TunerSession,
    RemoteAttempt,
}
impl Adapter {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            Self::LinuxCgroupV2 => "linux_cgroup_v2",
            Self::WindowsJob => "windows_job",
            Self::ProcessRoot => "process_root",
            Self::RootPipeTask => "root_pipe_task",
            Self::Scratch => "scratch",
            Self::TunerSession => "tuner_session",
            Self::RemoteAttempt => "remote_attempt",
        }
    }
    fn kind(&self) -> MemberKind {
        match self {
            Self::LinuxCgroupV2 | Self::WindowsJob => MemberKind::Containment,
            Self::ProcessRoot => MemberKind::Root,
            Self::RootPipeTask => MemberKind::Task,
            _ => MemberKind::TypedResource,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "method", rename_all = "snake_case")]
pub enum RecoveryMethod {
    OriginalCgroup {
        delegated_root: String,
        locator: String,
    },
    LiveOnly {
        reason: String,
    },
    /// A fixed, no-helper software root. These persisted fields select a
    /// possible recovery path; they never prove process exit or close custody.
    TrustedFixedSoftwareSameBootV1 {
        pid_namespace: String,
        executable_binding: String,
    },
    AuthoritativeResource {
        authority: String,
        operation: Uuid,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedMember {
    pub member_id: Uuid,
    pub kind: MemberKind,
    pub adapter: Adapter,
    pub adapter_version: u32,
    pub domain: String,
    pub locator: String,
    pub recovery: RecoveryMethod,
    /// Ownership/dependency edges refer to members in this manifest only.
    pub owned_by: Option<Uuid>,
    pub depends_on: Vec<Uuid>,
    /// Each named slot represents exactly one possible owner.
    pub maximum_cardinality: u32,
    /// Paths requiring preservation before generic startup scratch cleanup.
    pub protected_paths: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub graph_id: Uuid,
    pub owner_id: Uuid,
    pub attempt_id: Option<Uuid>,
    pub members: Vec<PlannedMember>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedIdentity {
    pub version: u32,
    pub member_id: Uuid,
    pub adapter: Adapter,
    pub adapter_version: u32,
    pub domain: String,
    pub object_identity: String,
    pub process: Option<ProcessIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MemberEnvelope {
    pub version: u32,
    pub planned: PlannedMember,
    pub observed: Option<ObservedIdentity>,
}

/// Compact audit facts. These fields never grant settlement authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClosedMemberFact {
    pub member_id: Uuid,
    pub adapter: Adapter,
    pub adapter_version: u32,
    pub domain: String,
    pub object_identity: String,
    pub evidence: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub(super) enum SettlementRecord {
    PreparedNeverArmed {
        version: u32,
        owner_id: Uuid,
    },
    Physical {
        version: u32,
        owner_id: Uuid,
        graph_id: Uuid,
        members: Vec<ClosedMemberFact>,
    },
}

pub(super) fn invalid() -> CustodyError {
    CustodyError::new(
        ErrorKind::UnreadableAuthority,
        "invalid v1 custody document",
    )
}
pub(super) fn nonnil(id: Uuid) -> Result<(), CustodyError> {
    if id.is_nil() {
        Err(invalid())
    } else {
        Ok(())
    }
}
pub(super) fn text(value: &str) -> Result<(), CustodyError> {
    if value.is_empty() || value.len() > 256 || value.contains('\0') {
        Err(invalid())
    } else {
        Ok(())
    }
}
pub(super) fn long_text(value: &str) -> Result<(), CustodyError> {
    if value.is_empty() || value.len() > 4096 || value.contains('\0') {
        Err(invalid())
    } else {
        Ok(())
    }
}
pub(super) fn encode<T: Serialize>(value: &T) -> Result<String, CustodyError> {
    let encoded = serde_json::to_string(value).map_err(|_| invalid())?;
    if encoded.len() > JSON_LIMIT {
        return Err(invalid());
    }
    Ok(encoded)
}
pub(super) fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, CustodyError> {
    if value.len() > JSON_LIMIT {
        return Err(invalid());
    }
    serde_json::from_str(value).map_err(|_| invalid())
}
pub(super) fn validate_process(value: &ProcessIdentity) -> Result<(), CustodyError> {
    if value.version != 1 || value.pid == 0 {
        return Err(invalid());
    }
    text(&value.domain)
}
pub(super) fn validate_boot(value: &BootIdentity) -> Result<(), CustodyError> {
    match value {
        BootIdentity::Known {
            value,
            provider,
            version,
        } => {
            nonnil(*value)?;
            text(provider)?;
            if *version != 1 {
                return Err(invalid());
            }
        }
        BootIdentity::Unknown { reason } => text(reason)?,
    }
    Ok(())
}
pub(super) fn validate_domain(value: &PlatformDomain) -> Result<(), CustodyError> {
    if value.version != 1 {
        return Err(invalid());
    }
    text(&value.platform)?;
    for field in [
        &value.host_identity,
        &value.namespace,
        &value.continuity_provider,
    ]
    .into_iter()
    .flatten()
    {
        text(field)?;
    }
    Ok(())
}
pub(super) fn validate_manifest(manifest: &Manifest) -> Result<(), CustodyError> {
    if manifest.version != 1 || manifest.members.is_empty() || manifest.members.len() > 129 {
        return Err(invalid());
    }
    nonnil(manifest.graph_id)?;
    nonnil(manifest.owner_id)?;
    if let Some(id) = manifest.attempt_id {
        nonnil(id)?;
    }
    let ids: BTreeSet<_> = manifest.members.iter().map(|m| m.member_id).collect();
    if ids.len() != manifest.members.len() {
        return Err(invalid());
    }
    let mut counts = [0usize; 4];
    for m in &manifest.members {
        nonnil(m.member_id)?;
        text(&m.domain)?;
        long_text(&m.locator)?;
        if m.adapter_version != 1
            || m.adapter.kind() != m.kind
            || m.maximum_cardinality != 1
            || m.depends_on.len() > 129
            || m.protected_paths.len() > 16
        {
            return Err(invalid());
        }
        counts[match m.kind {
            MemberKind::Containment => 0,
            MemberKind::Root => 1,
            MemberKind::Task => 2,
            MemberKind::TypedResource => 3,
        }] += 1;
        let mut dependencies = BTreeSet::new();
        for id in m.depends_on.iter().copied().chain(m.owned_by) {
            if id == m.member_id || !ids.contains(&id) {
                return Err(invalid());
            }
        }
        for id in &m.depends_on {
            if !dependencies.insert(id) {
                return Err(invalid());
            }
        }
        for path in &m.protected_paths {
            long_text(path)?;
            if !std::path::Path::new(path).is_absolute() {
                return Err(invalid());
            }
        }
        match &m.recovery {
            RecoveryMethod::OriginalCgroup {
                delegated_root,
                locator,
            } => {
                if m.adapter != Adapter::LinuxCgroupV2 {
                    return Err(invalid());
                }
                long_text(delegated_root)?;
                long_text(locator)?;
                if !std::path::Path::new(delegated_root).is_absolute() || locator != &m.locator {
                    return Err(invalid());
                }
            }
            RecoveryMethod::LiveOnly { reason } => text(reason)?,
            RecoveryMethod::TrustedFixedSoftwareSameBootV1 {
                pid_namespace,
                executable_binding,
            } => {
                if m.kind != MemberKind::Root || m.adapter != Adapter::ProcessRoot {
                    return Err(invalid());
                }
                text(pid_namespace)?;
                text(executable_binding)?;
                let (device, inode) = pid_namespace.split_once(':').ok_or_else(invalid)?;
                let device = device.parse::<u64>().map_err(|_| invalid())?;
                let inode = inode.parse::<u64>().map_err(|_| invalid())?;
                let Some(digest) =
                    executable_binding.strip_prefix("software-avc-executable-sha256:")
                else {
                    return Err(invalid());
                };
                if device == 0
                    || inode == 0
                    || pid_namespace != &format!("{device}:{inode}")
                    || digest.len() != 64
                    || !digest
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                {
                    return Err(invalid());
                }
            }
            RecoveryMethod::AuthoritativeResource {
                authority,
                operation,
            } => {
                if m.kind != MemberKind::TypedResource {
                    return Err(invalid());
                }
                text(authority)?;
                nonnil(*operation)?;
            }
        }
    }
    if counts[0] > 1 || counts[1] > 32 || counts[2] > 32 || counts[3] > 64 {
        return Err(invalid());
    }
    // Validate both dependency and ownership cycles (no recursive unbounded walk).
    let mut done = BTreeSet::new();
    loop {
        let before = done.len();
        for member in &manifest.members {
            if member
                .depends_on
                .iter()
                .chain(member.owned_by.iter())
                .all(|id| done.contains(id))
            {
                done.insert(member.member_id);
            }
        }
        if done.len() == manifest.members.len() {
            break;
        }
        if done.len() == before {
            return Err(invalid());
        }
    }
    encode(manifest)?;
    Ok(())
}
pub(super) fn validate_observed(
    planned: &PlannedMember,
    observed: &ObservedIdentity,
) -> Result<(), CustodyError> {
    if observed.version != 1
        || observed.member_id != planned.member_id
        || observed.adapter != planned.adapter
        || observed.adapter_version != planned.adapter_version
        || observed.domain != planned.domain
    {
        return Err(invalid());
    }
    long_text(&observed.object_identity)?;
    if let Some(process) = &observed.process {
        validate_process(process)?;
        if process.domain != planned.domain {
            return Err(invalid());
        }
    }
    encode(observed)?;
    Ok(())
}
pub(super) fn vector_bytes(v: ResourceVector) -> Vec<u8> {
    [
        v.cpu_ns_per_second,
        v.peak_memory_bytes,
        v.handles,
        v.processes,
        v.hardware_slots,
        v.tuner_slots,
        v.scratch_bytes,
    ]
    .into_iter()
    .flat_map(u64::to_le_bytes)
    .collect()
}
pub(super) fn vector_from_bytes(bytes: &[u8]) -> Result<ResourceVector, CustodyError> {
    if bytes.len() != 56 {
        return Err(invalid());
    }
    let mut parts = [0u64; 7];
    for (part, chunk) in parts.iter_mut().zip(bytes.chunks_exact(8)) {
        *part = u64::from_le_bytes(chunk.try_into().map_err(|_| invalid())?);
    }
    Ok(ResourceVector {
        cpu_ns_per_second: parts[0],
        peak_memory_bytes: parts[1],
        handles: parts[2],
        processes: parts[3],
        hardware_slots: parts[4],
        tuner_slots: parts[5],
        scratch_bytes: parts[6],
    })
}
pub(super) fn epoch_from_bytes(bytes: &[u8]) -> Result<u64, CustodyError> {
    Ok(u64::from_le_bytes(bytes.try_into().map_err(|_| invalid())?))
}
pub(super) fn uuid_from_bytes(bytes: &[u8]) -> Result<Uuid, CustodyError> {
    let id = Uuid::from_slice(bytes).map_err(|_| invalid())?;
    nonnil(id)?;
    Ok(id)
}
