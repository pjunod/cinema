//! Caller-owned admission at the actual local Raft proposal boundary.
//! Hiqlite does not know clock policy or application authority. Preparation is
//! pure and precedes management awaits; redemption is synchronous and occurs
//! only for a new submission, never committed-outcome reconciliation.

use crate::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MembershipAcquisition {
    AddLearner {
        node_id: u64,
    },
    Promote {
        node_id: u64,
    },
    /// An untrusted exact durable reference; no clock authority crosses wire.
    Reduction {
        reference: crate::ReductionFenceReference,
        retain_as_learner: bool,
    },
}

pub trait PreparedMembershipAdmission: Send + Sync {
    /// Reduction resolves its actual durable fence using the ORIGINAL capture.
    /// Acquisition is pure and needs no asynchronous proof preparation.
    fn prepare_proof(
        &mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), Error>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
    fn redeem(&self) -> Result<(), Error>;
}

pub trait MembershipAdmission: Send + Sync {
    /// Recover the caller's exact node-local context through a local Client,
    /// without a global registry or knowledge of the application's policy.
    fn as_any(&self) -> &dyn std::any::Any;
    fn prepare(
        &self,
        operation: MembershipAcquisition,
    ) -> Box<dyn PreparedMembershipAdmission + '_>;

    #[cfg(feature = "sqlite")]
    fn bind_membership(&self, metrics: crate::LocalDbRaftMetrics) -> Result<(), Error>;
}
