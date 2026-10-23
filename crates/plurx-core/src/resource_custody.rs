//! Node-local durable physical-obligation custody.
//!
//! Only the core runtime may mutate custody. Public snapshots/codecs are data,
//! never receipt or launch authority. Initialize under the existing daemon lock
//! before startup cleanup. No journal operation touches the accounting mutex.
mod codec;
pub(crate) mod codec_v2;
#[cfg(target_os = "linux")]
pub(crate) mod recovery;
mod schema;
mod storage;
#[cfg(test)]
mod tests;
mod transaction;
mod worker;

use crate::adaptive_resources::{
    PhysicalCloseReceipt, PhysicalRetirementReceipt, ReservationBinding, ResourceVector,
};
pub use codec::{
    Adapter, BootIdentity, ClosedMemberFact, Manifest, MemberKind, ObservedIdentity, PlannedMember,
    PlatformDomain, ProcessIdentity, RecoveryMethod,
};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;
pub use worker::{CustodyJournal, PendingOperation};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    UnsupportedStorage,
    UnreadableAuthority,
    IdentityMismatch,
    InvalidCommand,
    Conflict,
    Exhausted,
    QueueFull,
    Deadline,
    WorkerStopped,
    Storage,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitCertainty {
    NotStarted,
    RolledBack,
    Unknown,
}
#[derive(Clone, Debug, thiserror::Error, Serialize)]
#[error("{message}")]
pub struct CustodyError {
    pub kind: ErrorKind,
    pub operation_id: Option<Uuid>,
    pub obligations: Vec<Uuid>,
    pub certainty: CommitCertainty,
    /// Sanitized constant description; never native paths, SQL, or owner data.
    pub message: &'static str,
}
impl CustodyError {
    fn new(kind: ErrorKind, message: &'static str) -> Self {
        Self {
            kind,
            operation_id: None,
            obligations: Vec::new(),
            certainty: CommitCertainty::NotStarted,
            message,
        }
    }
    fn bind(mut self, operation: Uuid, obligations: Vec<Uuid>) -> Self {
        self.operation_id = Some(operation);
        self.obligations = obligations;
        self
    }
}
impl From<std::io::Error> for CustodyError {
    fn from(_: std::io::Error) -> Self {
        Self::new(ErrorKind::Storage, "custody filesystem operation failed")
    }
}
impl From<rusqlite::Error> for CustodyError {
    fn from(_: rusqlite::Error) -> Self {
        Self::new(ErrorKind::Storage, "custody SQLite operation failed")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallationContext {
    FirstInstallation,
    Existing,
}
#[derive(Clone, Debug)]
pub struct OwnerIdentity {
    pub owner_id: Uuid,
    pub node_id: String,
    pub cluster_id: String,
    pub os_boot: BootIdentity,
    pub daemon_process: ProcessIdentity,
    pub platform_domain: PlatformDomain,
}
/// Startup-only composition inputs. The caller must clone the actual held
/// SelectedStore lock, not an unrelated file. The worker retains it until drain.
pub struct OpenOptions {
    pub data_dir: PathBuf,
    pub excluded_roots: Vec<PathBuf>,
    pub installation: InstallationContext,
    pub owner: OwnerIdentity,
    pub daemon_lock: DaemonLockGuard,
    pub queue_capacity: usize,
}

/// Only existing core startup/store ownership can construct this guard. A path
/// or arbitrary public file supplied by a daemon/request caller is not authority.
pub struct DaemonLockGuard(Arc<File>);
impl DaemonLockGuard {
    pub(crate) fn from_held_lock(file: File) -> Self {
        Self(Arc::new(file))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Prepared,
    AcquisitionPossible,
    Sealed,
    UnknownHeld,
    Settled,
}
impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::AcquisitionPossible => "acquisition_possible",
            Self::Sealed => "sealed",
            Self::UnknownHeld => "unknown_held",
            Self::Settled => "settled",
        }
    }
    fn parse(s: &str) -> Result<Self, CustodyError> {
        match s {
            "prepared" => Ok(Self::Prepared),
            "acquisition_possible" => Ok(Self::AcquisitionPossible),
            "sealed" => Ok(Self::Sealed),
            "unknown_held" => Ok(Self::UnknownHeld),
            "settled" => Ok(Self::Settled),
            _ => Err(codec::invalid()),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Video,
    Audio,
    Companion,
}
impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Companion => "companion",
        }
    }
    fn parse(s: &str) -> Result<Self, CustodyError> {
        match s {
            "video" => Ok(Self::Video),
            "audio" => Ok(Self::Audio),
            "companion" => Ok(Self::Companion),
            _ => Err(codec::invalid()),
        }
    }
}
#[derive(Clone, Debug)]
pub struct MemberRecord {
    pub plan: PlannedMember,
    pub phase: String,
    pub observed: Option<ObservedIdentity>,
}
#[derive(Clone, Debug)]
pub(crate) struct TypedMemberRecord {
    pub envelope: codec_v2::Envelope,
    pub phase: String,
}
#[derive(Clone, Debug)]
pub(crate) enum ObligationInventory {
    LegacyV1 {
        manifest: Manifest,
        members: Vec<MemberRecord>,
    },
    TypedV2 {
        manifest: codec_v2::Manifest,
        members: Vec<TypedMemberRecord>,
    },
}
#[derive(Clone, Debug)]
pub(crate) struct RetainedAssetOwner {
    pub obligation_id: Uuid,
    pub custody: codec_v2::RetainedAssetsDocument,
}
#[derive(Clone, Debug)]
pub struct ObligationRecord {
    pub obligation_id: Uuid,
    pub graph_id: Uuid,
    pub owner_id: Uuid,
    pub allocation_epoch: u64,
    pub resources: ResourceVector,
    pub(crate) memory_claim: crate::adaptive_resources::MemoryClaim,
    pub trial_group_id: Option<Uuid>,
    pub phase: Phase,
    pub sequence: i64,
    pub(crate) inventory: ObligationInventory,
    pub hold_reason: Option<String>,
    /// Reconstruction is cleanup-only regardless of the persisted phase.
    pub cleanup_only: bool,
}
#[derive(Clone, Debug)]
pub struct FamilyEdge {
    pub family_id: Uuid,
    pub role: Role,
    pub obligation_id: Uuid,
}
/// Complete accounting, not a paginated diagnostic. ALL records are cleanup-only
/// after restart, including intact prepared candidates lacking a fencing proof.
#[derive(Clone, Debug)]
pub struct RecoverySnapshot {
    pub journal_id: Uuid,
    pub installation_id: Uuid,
    pub obligations: Vec<ObligationRecord>,
    pub families: Vec<FamilyEdge>,
    pub(crate) retained_assets: Vec<RetainedAssetOwner>,
    pub total: ResourceVector,
    pub trial_groups: Vec<Uuid>,
    pub protected_paths: Vec<PathBuf>,
    /// Native old-owner identities needed by a positive fencing backend.
    pub owners: Vec<OwnerIdentity>,
    /// Append-only effective custodians for retained S; historical R→S owners
    /// in obligations and custody documents remain unchanged.
    pub(crate) recovered_s_holders: Vec<RecoveredSHolder>,
    /// Append-only current custodians for imported, exited fixed producers.
    pub(crate) recovered_r_holders: Vec<RecoveredRHolder>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecoveredSHolder {
    pub(crate) obligation_id: Uuid,
    pub(crate) adoption_sequence: i64,
    pub(crate) operation_id: Uuid,
    pub(crate) previous_owner_id: Uuid,
    pub(crate) current_owner_id: Uuid,
    pub(crate) original_s_sequence: i64,
    pub(crate) head: codec_v2::scratch::AssetEventRefV1,
    pub(crate) frozen_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecoveredRHolder {
    pub(crate) obligation_id: Uuid,
    pub(crate) adoption_sequence: i64,
    pub(crate) operation_id: Uuid,
    pub(crate) previous_owner_id: Uuid,
    pub(crate) current_owner_id: Uuid,
    pub(crate) original_r_sequence: i64,
    pub(crate) head: codec_v2::scratch::AssetEventRefV1,
    pub(crate) root_member_id: Uuid,
    pub(crate) root_pid: u32,
    pub(crate) root_creation_marker: u64,
}

/// Private runtime capability, never deserialized from an obligation row.
#[derive(Clone, Debug)]
pub(crate) struct ObligationHandle {
    journal_id: Uuid,
    owner_id: Uuid,
    obligation_id: Uuid,
    graph_id: Uuid,
    runtime_binding: ReservationBinding,
    #[cfg(target_os = "linux")]
    recovery: Option<RecoveryAuthority>,
}
#[cfg(target_os = "linux")]
#[derive(Clone, Debug)]
enum RecoveryAuthority {
    RetainedS(crate::adaptive_resources::runtime::recovery::RecoveredSAuthority),
    ProducerR(crate::adaptive_resources::runtime::recovery::RecoveredRAuthority),
}
#[cfg(target_os = "linux")]
impl RecoveryAuthority {
    fn journal_id(&self) -> Uuid {
        match self {
            Self::RetainedS(value) => value.journal_id(),
            Self::ProducerR(value) => value.journal_id(),
        }
    }
    fn current_owner_id(&self) -> Uuid {
        match self {
            Self::RetainedS(value) => value.current_owner_id(),
            Self::ProducerR(value) => value.current_owner_id(),
        }
    }
    fn obligation_id(&self) -> Uuid {
        match self {
            Self::RetainedS(value) => value.obligation_id(),
            Self::ProducerR(value) => value.obligation_id(),
        }
    }
    fn binding(&self) -> ReservationBinding {
        match self {
            Self::RetainedS(value) => value.binding(),
            Self::ProducerR(value) => value.binding(),
        }
    }
}
impl ObligationHandle {
    #[cfg(target_os = "linux")]
    pub(crate) fn recovery_authority(
        &self,
    ) -> Option<&crate::adaptive_resources::runtime::recovery::RecoveredSAuthority> {
        match self.recovery.as_ref() {
            Some(RecoveryAuthority::RetainedS(authority)) => Some(authority),
            _ => None,
        }
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn producer_recovery_authority(
        &self,
    ) -> Option<&crate::adaptive_resources::runtime::recovery::RecoveredRAuthority> {
        match self.recovery.as_ref() {
            Some(RecoveryAuthority::ProducerR(authority)) => Some(authority),
            _ => None,
        }
    }
    pub(crate) fn obligation_id(&self) -> Uuid {
        self.obligation_id
    }
    pub(crate) fn graph_id(&self) -> Uuid {
        self.graph_id
    }
}
#[derive(Clone, Debug)]
pub(crate) struct FamilyHandle {
    journal_id: Uuid,
    family_id: Uuid,
}
/// Each role names either a new index in this atomic bundle or retained custody.
#[derive(Clone, Debug)]
pub(crate) enum RoleTarget {
    New(usize),
    Retained(ObligationHandle),
}
#[derive(Clone, Debug)]
pub(crate) struct FamilyRequest {
    pub family: FamilyHandle,
    pub roles: Vec<(Role, RoleTarget)>,
}
#[derive(Clone, Debug)]
pub(crate) struct NewObligation {
    pub runtime_binding: ReservationBinding,
    pub allocation_epoch: u64,
    pub resources: ResourceVector,
    /// The ACTUAL graph generation/process_instance, not another UUID pair.
    pub manifest: Manifest,
}
#[derive(Clone, Debug)]
pub(crate) struct PrepareBatch {
    pub obligations: Vec<NewObligation>,
    pub families: Vec<FamilyRequest>,
    pub trial: bool,
}
/// New typed records use the existing journal/queue and family accounting. They
/// never reinterpret historical v1 documents or acquire resources themselves.
#[derive(Clone, Debug)]
pub(crate) struct TypedNewObligation {
    pub runtime_binding: ReservationBinding,
    pub allocation_epoch: u64,
    pub resources: ResourceVector,
    pub memory_claim: crate::adaptive_resources::MemoryClaim,
    pub manifest: codec_v2::Manifest,
}
#[derive(Clone, Debug)]
pub(crate) struct TypedPrepareBatch {
    pub obligations: Vec<TypedNewObligation>,
    pub families: Vec<FamilyRequest>,
    pub trial: bool,
}
#[derive(Clone, Debug)]
pub(crate) struct PreparedBatch {
    pub obligations: Vec<ObligationHandle>,
    pub sequence: i64,
    pub trial_group_id: Option<Uuid>,
}
#[derive(Clone, Debug)]
pub(crate) struct TransitionKey {
    pub handle: ObligationHandle,
    pub operation_id: Uuid,
    pub expected_sequence: i64,
}
/// No Clone/serde: actual physical proof survives cancellation in the owner book.
#[derive(Debug)]
pub(crate) struct PhysicalSettlement {
    pub receipt: PhysicalCloseReceipt,
    pub facts: Vec<ClosedMemberFact>,
}

/// Typed retained-S close audit paired with the original nonclone physical
/// receipt. The settlement is data; only the receipt can authorize accounting.
#[derive(Debug)]
pub(crate) struct PhysicalTypedSettlement {
    pub receipt: PhysicalCloseReceipt,
    pub settlement: codec_v2::Settlement,
}

/// Actual physical transfer proof and its complete target obligation. This is
/// retained in the original journal command until the runtime takes it after
/// the exact durable retirement acknowledgment.
/// Original physical retirement capability returned only after its exact
/// durable journal acknowledgement. A pre-COMMIT receipt cannot reach the
/// conserving ledger swap through this type.
#[derive(Debug)]
pub(crate) struct CommittedRetirementReceipt {
    receipt: PhysicalRetirementReceipt,
}
impl CommittedRetirementReceipt {
    pub(crate) fn source_binding(&self) -> ReservationBinding {
        self.receipt.source_binding()
    }
    pub(crate) fn target_binding(&self) -> ReservationBinding {
        self.receipt.target_binding()
    }
    #[cfg(test)]
    pub(crate) fn adapter_fixture(receipt: PhysicalRetirementReceipt) -> Self {
        Self { receipt }
    }
}

#[derive(Debug)]
pub(crate) struct PhysicalAssetRetirement {
    pub receipt: PhysicalRetirementReceipt,
    pub settlement: codec_v2::Settlement,
    pub retained: codec_v2::RetainedAssetsV2,
    pub target_manifest: codec_v2::Manifest,
    pub target_resources: ResourceVector,
    pub target: ObligationHandle,
}

#[derive(Debug)]
pub(crate) enum Mutation {
    Arm {
        members: Vec<Uuid>,
    },
    RecordAcquired {
        identity: ObservedIdentity,
    },
    RecordTypedAcquired {
        envelope: codec_v2::Envelope,
    },
    AssetChangeV2 {
        change: codec_v2::scratch::AssetChangeV2,
    },
    AssetChangeV3 {
        change: codec_v2::scratch::bootstrap::AssetChangeV3,
    },
    Seal,
    Hold {
        reason: String,
    },
    CancelPrepared,
    Settle(PhysicalSettlement),
    SettleTyped(Box<PhysicalTypedSettlement>),
    RetireWithAssets(Box<PhysicalAssetRetirement>),
}
/// Queue/deadline refusal returns actual proof to the existing cleanup owner.
#[derive(Debug)]
pub(crate) struct RejectedTransition {
    pub error: CustodyError,
    pub key: TransitionKey,
    pub mutation: Mutation,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommitAck {
    pub operation_id: Uuid,
    pub obligation_id: Uuid,
    pub graph_id: Uuid,
    pub sequence: i64,
    pub phase: Phase,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetirementAck {
    pub(crate) version: u32,
    pub(crate) operation_id: codec_v2::Id,
    pub(crate) sequence: i64,
    pub(crate) source_obligation_id: codec_v2::Id,
    pub(crate) source_graph_id: codec_v2::Id,
    pub(crate) target_obligation_id: codec_v2::Id,
    pub(crate) target_graph_id: codec_v2::Id,
    pub(crate) asset_id: codec_v2::Id,
}

struct QueuedOperation {
    id: Uuid,
    deadline: Instant,
    command: Command,
}
enum Command {
    #[cfg(target_os = "linux")]
    RecoverRetainedS {
        proof: crate::adaptive_resources::runtime::recovery::NativeRetainedSImportProof,
        exit: recovery::PositivePreviousOwnerExit,
    },
    #[cfg(target_os = "linux")]
    RecoverActiveR {
        proof: crate::adaptive_resources::runtime::recovery::NativeActiveRImportProof,
    },
    Prepare {
        batch: PrepareBatch,
        handles: Vec<ObligationHandle>,
        trial: Option<Uuid>,
    },
    PrepareTyped {
        batch: TypedPrepareBatch,
        handles: Vec<ObligationHandle>,
        trial: Option<Uuid>,
    },
    Change {
        key: TransitionKey,
        mutation: Mutation,
    },
}
#[derive(Clone, Debug)]
pub(crate) enum OperationResult {
    #[cfg(target_os = "linux")]
    RecoveredSAdoption(RecoveryAdoptionAck),
    #[cfg(target_os = "linux")]
    RecoveredRAdoption(RecoveryAdoptionAck),
    Prepared(PreparedBatch),
    Changed(CommitAck),
    RetiredWithAssets {
        ack: RetirementAck,
        target: ObligationHandle,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryAdoptionAck {
    pub(crate) operation_id: Uuid,
    pub(crate) obligation_id: Uuid,
    pub(crate) previous_owner_id: Uuid,
    pub(crate) current_owner_id: Uuid,
    pub(crate) sequence: i64,
}
