//! The cluster facade: the one place feature code asks the cluster anything.
//!
//! Build contract: `docs/cluster/CLUSTER-FACADE-BUILD.md` (audit items B1 and
//! B2 of `docs/cluster/CLUSTER-LAYER-AUDIT-2026-11-14.md`).
//!
//! # The six questions
//!
//! Feature code — HLS, Live TV, sharing, jobs, artwork, the UI — asks the
//! cluster six questions, and asks them of `state.cluster`:
//!
//! | Question | Method | Slice |
//! |---|---|---|
//! | May this node serve or commit now? | `authority()` | 1 |
//! | Is this node a voter, a learner, standalone or fenced? | `role()` | 1 |
//! | Who can help, for this purpose? | `peers(purpose)` | 1 |
//! | Read this, applied or linearizable? | `read(class)` | B3, reserved |
//! | Tell me when this table changes | `watch(table)` | B4, reserved |
//! | Call that peer | `call(peer, ..)` | A5/B5, reserved |
//!
//! The reserved names have no body yet. A feature that needs one of them
//! today uses the existing route and is counted by the census below.
//!
//! # Who may touch the raw parts
//!
//! The `membership`, `replication` and `serving` handles, the Raft metrics
//! types, `hiqlite::` and `PeerTransport::new` belong to this module, the
//! serving fence and the peer transport. Every other production file is
//! counted by `cluster::census`: its reach-arounds are pinned per file and
//! per class, a new one fails the census, and the total (`CEILING`) may only
//! shrink (`tests/operations/test_cluster_facade_ratchet.py`). A file whose
//! count falls must lower its pin in the same change.
//!
//! # Staleness
//!
//! `peers()` answers from this node's applied replica: never ahead of what
//! it has applied, behind the leader by its apply lag. It does not consult
//! the serving fence, as the rosters it replaces did not — a node without a
//! serving proof still needs its roster to render status and to recover.
//! Callers that place work already hold `authority()`. Reachability and
//! progress freshness are functions of the clock, so they are evaluated at
//! every call, also on a cached roster.
//!
//! # What stays linearizable
//!
//! These are never `peers()`; each becomes an explicit facade method in a
//! later slice: the cache-admin revocation roster, the clock rosters, every
//! internal-peer authorizer, `local_node_is_active_voter`, the learner branch
//! of job execution, maintenance reconciliation, the sharing member and
//! custody observations, and every membership mutation.

#[cfg(test)]
mod census;
