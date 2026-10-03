//! Server-only Source identity and capacity handles. None is wire authority.
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
pub struct SourceSessionRequest {
    pub principal: PlaybackPrincipal,
    pub request_id: String,
    pub request_fingerprint: String,
    pub playback_id: String,
    pub incarnation_id: Uuid,
    pub now_ms: i64,
    pub claim_expires_at_ms: i64,
    pub credential_hash: String,
    pub item_id: SourceId,
    pub file_id: SourceId,
    pub file_revision: FileRevision,
}

/// No Serialize/Deserialize/Debug: a wire revision cannot mint this witness.
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
        if !matches!(request.principal, PlaybackPrincipal::Sharing { .. })
            || !request.principal.valid_admission_shape()
            || !is_hash(&request.credential_hash)
            || !is_hash(&request.request_fingerprint)
            || !(1..=128).contains(&request.request_id.len())
            || request.request_id.chars().any(char::is_control)
            || !(1..=128).contains(&request.playback_id.len())
            || request.playback_id.chars().any(char::is_control)
            || request.now_ms <= 0
            || request.claim_expires_at_ms <= request.now_ms
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
pub struct SourceDispatchAssignment {
    pub(crate) binding: SourceBindingHandle,
    pub(crate) owner_node_id: String,
    pub(crate) dispatch_generation: i64,
    pub(crate) members: crate::cluster::membership::SourceAdmissionMembers,
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
    /// Check the original observation's clock before actual queue admission.
    /// Success is not a current grant/floor proof or a physical worker permit.
    pub fn validate_observation_freshness(
        &self,
        now_ms: i64,
    ) -> Result<(), crate::cluster::membership::MembershipError> {
        self.members.write_guard(now_ms, 1, 2, 3).map(|_| ())
    }
}

/// Fresh assignment-stage authority for a first blocked Source activation.
/// It is not renewable route, replacement, or takeover authority.
pub struct SourceSessionWriteAuthority {
    pub(crate) assignment: SourceDispatchAssignment,
    pub(crate) intent: Box<SourceSessionIntent>,
}

pub enum SourceWriteAuthorityRead {
    Ready(Box<SourceSessionWriteAuthority>),
    Unavailable,
    Capacity,
}
