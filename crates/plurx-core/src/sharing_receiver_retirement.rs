//! Actor-confirmed retirement; metadata and expiry never establish termination.
use crate::sharing_receiver_sessions::{
    ReceiverSessionIntent, ReceiverSourceBinding, ReceiverSourceOwner,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ReceiverRetirementDisposition {
    SourceSettled,
    NeverDispatched,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ReceiverRetirementReason {
    Deleted,
    Superseded,
    AdminStop,
    Revoked,
    Replaced,
}
/// Implemented by the daemon's private factory only after confirmed Source End
/// lineage (or owned no-send CAS) and joined accepted bodies/tasks. Store checks
/// metadata lineage, not the physical evidence. There is no concrete constructor.
pub trait ReceiverRetirementWitness: Send + Sync {
    fn intent(&self) -> &ReceiverSessionIntent;
    fn owner(&self) -> &ReceiverSourceOwner;
    fn binding(&self) -> Option<&ReceiverSourceBinding>;
    fn disposition(&self) -> ReceiverRetirementDisposition;
    fn reason(&self) -> ReceiverRetirementReason;
    /// Stable canonical SHA-256 identity of the actual retained confirmation.
    fn confirmation_id(&self) -> &str;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiverRetirementOutcome {
    Applied,
    Replay,
    Refused,
}

/// Captured claim ownership. Unassigned is a real retained NULL owner, not a
/// wildcard. Assigned retains the original immutable attempted node, including
/// commit-unknown assignment; the cleanup transaction proves the actual exact
/// row. A Refused result may try the retained Unassigned tuple; an error may not.
pub enum ReceiverPendingOwner {
    Unassigned,
    Assigned(String),
}
/// Private daemon evidence requires the owned no-send CAS and joined Start,
/// accepted bodies and independent jobs. No route or Source proof is fabricated.
pub trait ReceiverPendingRetirementWitness: Send + Sync {
    fn intent(&self) -> &ReceiverSessionIntent;
    fn request_id(&self) -> &str;
    fn playback_id(&self) -> &str;
    fn owner(&self) -> &ReceiverPendingOwner;
    fn confirmation_id(&self) -> &str;
}

/// Why an orphaned B route cannot be retired by recovery. Reported, never
/// deleted: the durable route and binding keep their exact lineage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiverOrphanStrandedReason {
    /// Some, but not all, Source binding columns are set.
    PartialBinding,
    /// A pending route without the durable dispatch marker: whether Start was
    /// sent is unknown, so neither End lineage nor no-send proof exists.
    DispatchUnknown,
}
impl ReceiverOrphanStrandedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PartialBinding => "partial_binding",
            Self::DispatchUnknown => "dispatch_unknown",
        }
    }
}

/// Durable Source Start dispatch phase of a pending orphan.
#[derive(Clone)]
pub enum ReceiverOrphanDispatch {
    /// Activation's `none` marker: no dispatch was ever recorded.
    NotDispatched,
    /// Sealed dispatch capsule recorded before the first Start byte.
    Sealed(crate::secrets::SealedSecret),
    /// No marker (NULL): dispatch is unknown.
    Unknown,
}

/// A B RemoteSource route whose owner stopped renewing it: lease past the
/// takeover grace (or the owner node is removed), matching job lease, and a
/// pending or attached binding. Constructed only by the Store from one exact
/// row; it is an inventory observation, never Source or settlement evidence.
#[derive(Clone)]
pub struct ReceiverOrphan {
    pub(crate) intent: ReceiverSessionIntent,
    pub(crate) owner: ReceiverSourceOwner,
    pub(crate) playback_id: String,
    pub(crate) binding: Option<ReceiverSourceBinding>,
    pub(crate) dispatch: ReceiverOrphanDispatch,
    pub(crate) stranded: Option<ReceiverOrphanStrandedReason>,
}
impl ReceiverOrphan {
    /// Retained intent rebuilt from the durable recipe and upstream
    /// generations. The original login need not still exist.
    pub fn intent(&self) -> &ReceiverSessionIntent {
        &self.intent
    }
    /// Owner observed by the inventory (before a claim) or installed by the
    /// claim (after it).
    pub fn owner(&self) -> &ReceiverSourceOwner {
        &self.owner
    }
    pub fn playback_id(&self) -> &str {
        &self.playback_id
    }
    pub fn binding(&self) -> Option<&ReceiverSourceBinding> {
        self.binding.as_ref()
    }
    pub fn dispatch(&self) -> &ReceiverOrphanDispatch {
        &self.dispatch
    }
    /// Set when recovery can never retire this row; such rows are not claimed.
    pub fn stranded(&self) -> Option<ReceiverOrphanStrandedReason> {
        self.stranded
    }
}

/// The orphan after this node's exact takeover claim: owner/epoch/lease are
/// the new fenced values. Only the claim constructs it.
pub struct ClaimedReceiverOrphan(pub(crate) ReceiverOrphan);
impl ClaimedReceiverOrphan {
    pub fn orphan(&self) -> &ReceiverOrphan {
        &self.0
    }
}

pub enum ReceiverOrphanClaimOutcome {
    Claimed(Box<ClaimedReceiverOrphan>),
    /// The row changed: another owner, renewal, retirement or removal.
    Lost,
    /// The exact unchanged row was refused (grace not reached, this node is
    /// removed, or the orphan is not claimable).
    Refused,
}
