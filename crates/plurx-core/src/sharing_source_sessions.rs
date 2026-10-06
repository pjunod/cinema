//! Server-only Source identity and capacity handles. None is wire authority.
// Source admission always requires the actual replicated member observation.
// Bare Core preserves its SQLite/type surface, but cannot construct a proof.
#[cfg(feature = "hiqlite-store")]
pub use crate::cluster::membership::{
    MembershipError as SourceAdmissionError, SourceAdmissionMembers,
};
#[cfg(not(feature = "hiqlite-store"))]
pub type SourceAdmissionError = crate::error::StoreError;
#[cfg(not(feature = "hiqlite-store"))]
#[derive(Clone)]
pub enum SourceAdmissionMembers {}
#[cfg(not(feature = "hiqlite-store"))]
impl SourceAdmissionMembers {
    pub fn write_guard(
        &self,
        _now_ms: i64,
        _members_parameter: usize,
        _cutoff_parameter: usize,
        _observed_at_parameter: usize,
    ) -> Result<(String, String, i64, i64), SourceAdmissionError> {
        match *self {}
    }
    pub(crate) fn actual_local_raft_id(&self) -> u64 {
        match *self {}
    }
}

use crate::{
    error::StoreError,
    playback_principal::PlaybackPrincipal,
    secrets::SealedSecret,
    sharing::{invalid, is_hash, SourceId},
    sharing_catalogue_details::{CatalogueRevisionKey, FileRevision, SourceFileWitness},
};
use uuid::Uuid;

/// Untrusted identity inputs. The intent factory validates these and binds a
/// current server-produced witness; these fields are not themselves authority.
#[derive(Clone)]
pub struct SourceSessionRequest {
    pub principal: PlaybackPrincipal,
    pub request_id: String,
    pub request_fingerprint: String,
    pub playback_id: String,
    pub incarnation_id: Uuid,
    /// The actual worker registry boot supplied by the daemon boundary.
    pub ingress_registry_boot_id: Uuid,
    pub now_ms: i64,
    pub claim_expires_at_ms: i64,
    pub credential_hash: String,
    pub item_id: SourceId,
    pub file_id: SourceId,
    pub file_revision: FileRevision,
}

/// No Serialize/Deserialize/Debug: a wire revision cannot mint this witness.
#[derive(Clone)]
pub struct SourceSessionIntent {
    pub(crate) request: SourceSessionRequest,
    pub(crate) witness: SourceFileWitness,
    pub(crate) revision_key_envelope: SealedSecret,
}
impl SourceSessionIntent {
    /// Requires the result of the current authorized Store witness query and
    /// the Source-purpose key. The mutation must still recheck the snapshot.
    pub(crate) fn from_current_witness(
        request: SourceSessionRequest,
        witness: SourceFileWitness,
        revision_key: &CatalogueRevisionKey,
        revision_key_envelope: SealedSecret,
    ) -> Result<Self, StoreError> {
        Self::from_witness(request, witness, revision_key, revision_key_envelope, false)
    }
    pub(crate) fn from_current_owned_witness(
        request: SourceSessionRequest,
        witness: SourceFileWitness,
        revision_key: &CatalogueRevisionKey,
        revision_key_envelope: SealedSecret,
    ) -> Result<Self, StoreError> {
        Self::from_witness(request, witness, revision_key, revision_key_envelope, true)
    }
    fn from_witness(
        request: SourceSessionRequest,
        witness: SourceFileWitness,
        revision_key: &CatalogueRevisionKey,
        revision_key_envelope: SealedSecret,
        resolved: bool,
    ) -> Result<Self, StoreError> {
        if !matches!(request.principal, PlaybackPrincipal::Sharing { .. })
            || !request.principal.valid_admission_shape()
            || request.ingress_registry_boot_id.is_nil()
            || request.ingress_registry_boot_id.get_version_num() != 4
            || !is_hash(&request.credential_hash)
            || !is_hash(&request.request_fingerprint)
            || !(1..=128).contains(&request.request_id.len())
            || request.request_id.chars().any(char::is_control)
            || !(1..=128).contains(&request.playback_id.len())
            || request.playback_id.chars().any(char::is_control)
            || request.now_ms <= 0
            || request.claim_expires_at_ms <= 0
            || (!resolved && request.claim_expires_at_ms <= request.now_ms)
            || witness.item != request.item_id
            || witness.file != request.file_id
            || revision_key.file_revision(&witness)? != request.file_revision
        {
            return Err(invalid());
        }
        Ok(Self {
            request,
            witness,
            revision_key_envelope,
        })
    }
}

pub enum SourceIntentRead {
    Ready(Box<SourceSessionIntent>),
    Unavailable,
    Capacity,
}

/// Immutable persisted identity. This handle confers no current grant,
/// membership or producer authority and deliberately has no wire encoding.
#[derive(Clone)]
pub struct SourceBindingHandle {
    pub(crate) incarnation_id: Uuid,
    pub(crate) principal: PlaybackPrincipal,
    pub(crate) request_id: String,
    pub(crate) request_fingerprint: String,
    pub(crate) playback_id: String,
    pub(crate) source_server_id: Uuid,
    pub(crate) catalogue_epoch: Uuid,
    pub(crate) library_id: crate::sharing::SourceId,
    pub(crate) item_id: crate::sharing::SourceId,
    pub(crate) file_id: crate::sharing::SourceId,
    pub(crate) file_revision: FileRevision,
    pub(crate) released: bool,
}
impl SourceBindingHandle {
    pub fn incarnation_id(&self) -> Uuid {
        self.incarnation_id
    }
    pub fn principal(&self) -> &PlaybackPrincipal {
        &self.principal
    }
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    pub fn is_released(&self) -> bool {
        self.released
    }

    /// Compare the complete immutable association, independent of retirement.
    /// Equality does not authorize a write or certify physical settlement.
    pub fn same_identity(&self, other: &Self) -> bool {
        self.incarnation_id == other.incarnation_id
            && self.principal == other.principal
            && self.request_id == other.request_id
            && self.request_fingerprint == other.request_fingerprint
            && self.playback_id == other.playback_id
            && self.source_server_id == other.source_server_id
            && self.catalogue_epoch == other.catalogue_epoch
            && self.library_id == other.library_id
            && self.item_id == other.item_id
            && self.file_id == other.file_id
            && self.file_revision == other.file_revision
    }
    pub fn playback_id(&self) -> &str {
        &self.playback_id
    }
    pub fn request_fingerprint(&self) -> &str {
        &self.request_fingerprint
    }
    pub fn source_server_id(&self) -> Uuid {
        self.source_server_id
    }
    pub fn catalogue_epoch(&self) -> Uuid {
        self.catalogue_epoch
    }
    pub fn library_id(&self) -> &SourceId {
        &self.library_id
    }
    pub fn item_id(&self) -> &SourceId {
        &self.item_id
    }
    pub fn file_id(&self) -> &SourceId {
        &self.file_id
    }
    pub fn file_revision(&self) -> &FileRevision {
        &self.file_revision
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceCapacity {
    SourceSlots,
    GrantSlots,
    PendingStarts,
    RetainedBindings,
    RetainedRequests,
}

/// Exact retries retain the original incarnation, including after retirement.
pub enum SourceClaimOutcome {
    Acquired(SourceBindingHandle),
    InFlight(SourceBindingHandle),
    Resolved {
        binding: SourceBindingHandle,
        response_json: String,
    },
    Retired(SourceBindingHandle),
    Conflict,
    Unavailable,
    Capacity(SourceCapacity),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceReleaseOutcome {
    Released,
    ExactReplay,
    Refused,
}

/// An actual local worker's committed assignment. A retained binding or a
/// caller-supplied node string cannot construct this handle. This is not an
/// encoder permit, an active route lease, or takeover authority.
#[derive(Clone)]
pub struct SourceDispatchAssignment {
    pub(crate) binding: SourceBindingHandle,
    pub(crate) owner_node_id: String,
    pub(crate) dispatch_generation: i64,
    pub(crate) members: SourceAdmissionMembers,
}
impl SourceDispatchAssignment {
    pub fn binding(&self) -> &SourceBindingHandle {
        &self.binding
    }
    pub fn owner_node_id(&self) -> &str {
        &self.owner_node_id
    }
    pub fn dispatch_generation(&self) -> i64 {
        self.dispatch_generation
    }

    /// Canonical accounting identity for this exact dispatch. This hash carries
    /// no admission or physical closure authority, and survives grant rotation.
    pub fn custody_identity(&self) -> String {
        binding_custody_identity(&self.binding, &self.owner_node_id, self.dispatch_generation)
    }

    /// Membership observations may refresh without changing dispatch lineage.
    pub fn same_identity(&self, other: &Self) -> bool {
        self.binding.same_identity(&other.binding)
            && self.owner_node_id == other.owner_node_id
            && self.dispatch_generation == other.dispatch_generation
    }
    /// Check the original observation's clock before actual queue admission.
    /// Success is not a current grant/floor proof or a physical worker permit.
    pub fn validate_observation_freshness(&self, now_ms: i64) -> Result<(), SourceAdmissionError> {
        self.members.write_guard(now_ms, 1, 2, 3).map(|_| ())
    }
}

pub(crate) fn binding_custody_identity(
    binding: &SourceBindingHandle,
    owner: &str,
    generation: i64,
) -> String {
    let PlaybackPrincipal::Sharing {
        grant_id,
        viewer_key,
    } = &binding.principal
    else {
        unreachable!("Source binding factory accepts only sharing principals")
    };
    let values = [
        binding.principal.owner_key(),
        grant_id.to_string(),
        viewer_key.as_str().to_owned(),
        binding.request_id.clone(),
        binding.request_fingerprint.clone(),
        binding.playback_id.clone(),
        binding.incarnation_id.to_string(),
        owner.to_owned(),
        generation.to_string(),
        binding.source_server_id.to_string(),
        binding.catalogue_epoch.to_string(),
        binding.library_id.as_str().to_owned(),
        binding.item_id.as_str().to_owned(),
        binding.file_id.as_str().to_owned(),
        binding.file_revision.as_str().to_owned(),
    ];
    custody_identity_fields(values)
}
/// Fixed ordered metadata fields, not an opaque binding/assignment factory.
pub(crate) fn custody_identity_fields(values: [String; 15]) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(b"plurx.sharing-source-ingress-owner.v1\0");
    for value in values {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

/// Additive ingress activation authority. It is neither driver closure nor
/// a negative-admission receipt; only a guarded Store factory constructs it.
#[derive(Clone)]
pub struct SourceIngressAdmissionPermission {
    pub(crate) assignment: SourceDispatchAssignment,
    pub(crate) registry_boot_id: Uuid,
    pub(crate) members: SourceAdmissionMembers,
}
impl SourceIngressAdmissionPermission {
    pub fn assignment(&self) -> &SourceDispatchAssignment {
        &self.assignment
    }
    pub fn registry_boot_id(&self) -> Uuid {
        self.registry_boot_id
    }
    pub fn validate_observation_freshness(&self, now_ms: i64) -> Result<(), SourceAdmissionError> {
        self.members.write_guard(now_ms, 1, 2, 3).map(|_| ())
    }
}

/// Fresh assignment-stage authority for a first blocked Source activation.
/// It is not renewable route, replacement, or takeover authority.
#[derive(Clone)]
pub struct SourceSessionWriteAuthority {
    pub(crate) assignment: SourceDispatchAssignment,
    pub(crate) intent: Box<SourceSessionIntent>,
}
impl SourceSessionWriteAuthority {
    pub fn assignment(&self) -> &SourceDispatchAssignment {
        &self.assignment
    }
    /// Required again after commit and before actual queue admission. This
    /// checks the original snapshot clock, not current grant or physical work.
    pub fn validate_observation_freshness(&self, now_ms: i64) -> Result<(), SourceAdmissionError> {
        self.assignment.validate_observation_freshness(now_ms)
    }
}

pub enum SourceWriteAuthorityRead {
    Ready(Box<SourceSessionWriteAuthority>),
    Unavailable,
    Capacity,
}

/// Current resolved Source route and exact live lease, freshly authorized by
/// the actual local worker. This cannot publish, take over, or release work.
#[derive(Clone)]
pub struct SourceOwnedRouteAuthority {
    pub(crate) assignment: SourceDispatchAssignment,
    pub(crate) intent: Box<SourceSessionIntent>,
    pub(crate) session_id: String,
    pub(crate) lease_expires_at_ms: i64,
    pub(crate) lease_revision: i64,
}
impl SourceOwnedRouteAuthority {
    pub fn validate_observation_freshness(&self, now_ms: i64) -> Result<(), SourceAdmissionError> {
        self.assignment.validate_observation_freshness(now_ms)
    }
    /// Whether `fresh` is this same owned route after another renewal by the
    /// same owner advanced its lease revision. The renewal guard pins the
    /// exact lease a proof observed, so two renewals by one owner race; the
    /// loser may retry against `fresh`. Any other difference is a real change
    /// of authority and must not be retried.
    pub fn renewed_by_same_owner(&self, fresh: &Self) -> bool {
        self.assignment.same_identity(&fresh.assignment)
            && self.session_id == fresh.session_id
            && fresh.lease_revision > self.lease_revision
    }
}
pub enum SourceOwnedRouteAuthorityRead {
    Ready(Box<SourceOwnedRouteAuthority>),
    Unavailable,
    Capacity,
}

#[cfg(all(test, feature = "hiqlite-store"))]
mod association_tests {
    use super::*;

    #[test]
    fn source_association_identity_never_collapses_grant_viewer_request_or_file() {
        let binding = SourceBindingHandle {
            incarnation_id: Uuid::new_v4(),
            principal: PlaybackPrincipal::sharing(Uuid::new_v4(), &"a".repeat(64))
                .expect("principal"),
            request_id: "request".into(),
            request_fingerprint: "b".repeat(64),
            playback_id: "playback".into(),
            source_server_id: Uuid::new_v4(),
            catalogue_epoch: Uuid::new_v4(),
            library_id: SourceId::parse("1").expect("library"),
            item_id: SourceId::parse("2").expect("item"),
            file_id: SourceId::parse("3").expect("file"),
            file_revision: FileRevision::parse(&"c".repeat(64)).expect("revision"),
            released: false,
        };
        let mut changed = binding.clone();
        changed.released = true;
        assert!(binding.same_identity(&changed), "retirement keeps identity");
        for dimension in 0..12 {
            let mut changed = binding.clone();
            match dimension {
                0 => changed.incarnation_id = Uuid::new_v4(),
                1 => {
                    changed.principal = PlaybackPrincipal::sharing(Uuid::new_v4(), &"a".repeat(64))
                        .expect("other grant")
                }
                2 => {
                    changed.principal = PlaybackPrincipal::sharing(
                        match &binding.principal {
                            PlaybackPrincipal::Sharing { grant_id, .. } => *grant_id,
                            _ => panic!("sharing fixture"),
                        },
                        &"d".repeat(64),
                    )
                    .expect("other viewer")
                }
                3 => changed.request_id.push('2'),
                4 => changed.request_fingerprint = "d".repeat(64),
                5 => changed.playback_id.push('2'),
                6 => changed.source_server_id = Uuid::new_v4(),
                7 => changed.catalogue_epoch = Uuid::new_v4(),
                8 => changed.library_id = SourceId::parse("4").expect("library"),
                9 => changed.item_id = SourceId::parse("4").expect("item"),
                10 => changed.file_id = SourceId::parse("4").expect("file"),
                11 => {
                    changed.file_revision = FileRevision::parse(&"d".repeat(64)).expect("revision")
                }
                _ => unreachable!(),
            }
            assert!(!binding.same_identity(&changed), "dimension {dimension}");
        }
        let members = crate::cluster::membership::source_admission_members_for_unit_test(
            1,
            &std::collections::BTreeSet::from([1]),
            1,
        )
        .expect("fixture observation");
        let assignment = SourceDispatchAssignment {
            binding,
            owner_node_id: "node".into(),
            dispatch_generation: 1,
            members,
        };
        let mut other = assignment.clone();
        other.owner_node_id.push('2');
        assert!(!assignment.same_identity(&other));
        other = assignment.clone();
        other.dispatch_generation = 2;
        assert!(!assignment.same_identity(&other));
    }
}

/// Current blocked-or-exactly-published Source route permission. This has no
/// wire encoding and does not certify producer readiness. Only the actual
/// worker owner may invoke publication after its registered readiness barrier.
#[derive(Clone)]
pub struct SourcePublicationAuthority {
    pub(crate) owned: SourceOwnedRouteAuthority,
    pub(crate) phase: SourcePublicationPhase,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourcePublicationPhase {
    Pending,
    Published,
}
impl SourcePublicationAuthority {
    pub fn assignment(&self) -> &SourceDispatchAssignment {
        &self.owned.assignment
    }
    pub fn session_id(&self) -> &str {
        &self.owned.session_id
    }
    pub fn validate_observation_freshness(&self, now_ms: i64) -> Result<(), SourceAdmissionError> {
        self.owned.validate_observation_freshness(now_ms)
    }
}
pub enum SourcePublicationAuthorityRead {
    Ready(Box<SourcePublicationAuthority>),
    Unavailable,
    Capacity,
}
