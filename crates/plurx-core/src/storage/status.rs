//! Read-only storage facts shared by native and replicated composition.
//! These values confer no serving, membership or quorum authority.

use serde::{Deserialize, Serialize};

/// The durable backend serving this daemon process.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplicationBackend {
    Sqlite,
    Replicated,
}

/// The conclusion an operator should act on, not a raw Raft server state.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplicationHealth {
    /// SQLite has no replication peer and must not be labelled "synced".
    SingleNode,
    InSync,
    Degraded,
}

/// Safe, user-visible watch-state replication status.
///
/// It carries no node address, media path, account, token, or library data.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ReplicationStatus {
    pub backend: ReplicationBackend,
    pub health: ReplicationHealth,
    /// More than one voter is present. The membership roster belongs to M3.
    pub clustered: bool,
    /// Raft term/index of the last entry applied to this node.
    pub last_applied_term: Option<u64>,
    pub last_applied_index: Option<u64>,
    /// Largest observed gap, or a conservative upper bound for a missing peer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind_by: Option<u64>,
    /// Unix seconds when this process last positively observed convergence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_converged_at: Option<i64>,
    /// Process-local applied-index baseline retained across degraded samples.
    ///
    /// This is classifier state, not part of the public JSON contract.
    #[serde(skip)]
    pub(crate) last_converged_index: Option<u64>,
    /// Unix seconds when this projection was sampled.
    pub checked_at: i64,
    /// Plain-language interpretation for clients that do not speak Raft.
    pub explanation: String,
}

impl ReplicationStatus {
    /// Local storage facts only; this value carries no serving authority.
    pub fn standalone(checked_at: i64) -> Self {
        Self {
            backend: ReplicationBackend::Sqlite,
            health: ReplicationHealth::SingleNode,
            clustered: false,
            last_applied_term: None,
            last_applied_index: None,
            behind_by: None,
            last_converged_at: None,
            last_converged_index: None,
            checked_at,
            explanation:
                "Watch progress is stored only on this server; this SQLite node is not clustered."
                    .to_owned(),
        }
    }
}
