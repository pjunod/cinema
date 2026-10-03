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
