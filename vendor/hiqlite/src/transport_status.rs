//! Bounded, process-local snapshot transport observations.
//!
//! This state is owned by one embedded node. It is intentionally independent
//! of SQL, Raft logs, and Prometheus labels: recording a chunk must never
//! create replicated work or retain an unbounded snapshot/request identity.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::Instant;

#[cfg(feature = "sqlite")]
use openraft::RaftMetrics;
#[cfg(feature = "sqlite")]
use tokio::sync::watch;

const RETIRED_PEER_ALLOWANCE: usize = 4;
// SQLite and cache can each have independent inbound and outbound work.
const OBSERVATIONS_PER_PEER: usize = 4;
const STALLED_AFTER: Duration = Duration::from_secs(30);
const EXPIRE_AFTER: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotTransportPhase {
    Connecting,
    Transferring,
    AwaitingAcknowledgement,
    Installing,
    Retrying,
    Stalled,
    Failed,
    Complete,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotTransportDirection {
    Inbound,
    Outbound,
}

impl SnapshotTransportPhase {
    fn can_stall(self) -> bool {
        matches!(
            self,
            Self::Connecting
                | Self::Transferring
                | Self::AwaitingAcknowledgement
                | Self::Installing
                | Self::Retrying
                | Self::Stalled
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SnapshotTransportObservation {
    pub observing_node_id: u64,
    pub peer_node_id: u64,
    pub raft_group: String,
    pub boot_id: String,
    pub attempt_id: u64,
    pub snapshot_id: Option<String>,
    pub socket_epoch: u64,
    pub direction: SnapshotTransportDirection,
    pub attempted_offset: Option<u64>,
    pub acknowledged_offset: Option<u64>,
    pub locally_received_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    pub attempt_age_ms: Option<u64>,
    pub last_acknowledgement_age_ms: Option<u64>,
    pub last_local_receive_age_ms: Option<u64>,
    pub active_deadline_remaining_ms: Option<u64>,
    pub sample_age_ms: u64,
    pub phase: SnapshotTransportPhase,
    pub reconnect_count: u64,
    pub retry_count: u64,
    pub last_error_category: Option<String>,
    pub operation_owns_work: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SnapshotTransportStatus {
    pub schema_version: u32,
    pub observing_node_id: u64,
    pub observed_at_unix_ms: u64,
    pub observations: Vec<SnapshotTransportObservation>,
}

pub(crate) struct InboundSnapshotChunk<'a> {
    pub(crate) raft_group: &'static str,
    pub(crate) peer_node_id: u64,
    pub(crate) snapshot_id: &'a str,
    pub(crate) offset: u64,
    pub(crate) len: usize,
    pub(crate) done: bool,
    pub(crate) socket_epoch: u64,
    pub(crate) deadline: Instant,
}

#[derive(Clone, Copy)]
pub(crate) struct OutboundSnapshotSocket {
    pub(crate) epoch: u64,
    pub(crate) connected: bool,
}

/// Identity of one outbound snapshot operation and the Raft client that owns it.
///
/// OpenRaft creates ordinary RPC and snapshot clients for the same peer. Both
/// share the node's status handle, so neither the attempt nor connection
/// identity may be allocated by an individual client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct OutboundSnapshotAttempt {
    raft_group: &'static str,
    peer_node_id: u64,
    pub(crate) attempt_id: u64,
    connection_id: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct InboundSnapshotAttempt {
    raft_group: &'static str,
    peer_node_id: u64,
    attempt_id: u64,
    snapshot_id: String,
    socket_epoch: u64,
    attempted_offset: u64,
    total_bytes: Option<u64>,
    attempt_started: Instant,
    deadline: Instant,
    pub(crate) done: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InboundSnapshotDisposition {
    Succeeded,
    Retrying(&'static str),
    Failed(&'static str),
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ObservationKey {
    raft_group: &'static str,
    peer_node_id: u64,
    direction: SnapshotTransportDirection,
}

#[derive(Clone, Debug)]
struct Observation {
    attempt_id: u64,
    snapshot_id: Option<String>,
    socket_epoch: u64,
    attempted_offset: Option<u64>,
    acknowledged_offset: Option<u64>,
    locally_received_bytes: Option<u64>,
    total_bytes: Option<u64>,
    attempt_started: Option<Instant>,
    last_acknowledgement: Option<Instant>,
    last_local_receive: Option<Instant>,
    deadline: Option<Instant>,
    last_update: Instant,
    phase: SnapshotTransportPhase,
    connection_attempt_count: u64,
    retry_count: u64,
    last_error_category: Option<&'static str>,
    operation_owns_work: bool,
    outbound_connection_id: Option<u64>,
}

struct State {
    next_inbound_attempt: u64,
    next_inbound_socket_epoch: u64,
    observations: BTreeMap<ObservationKey, Observation>,
    pending_inbound: BTreeMap<ObservationKey, PendingInboundSnapshot>,
    current_peers: BTreeSet<u64>,
    #[cfg(feature = "sqlite")]
    sqlite_membership: Option<watch::Receiver<RaftMetrics<u64, crate::Node>>>,
}

struct PendingInboundSnapshot {
    attempt: InboundSnapshotAttempt,
    disposition: Option<(Option<Instant>, InboundSnapshotDisposition)>,
}

struct Inner {
    observing_node_id: u64,
    boot_id: String,
    next_outbound_attempt: AtomicU64,
    next_outbound_connection: AtomicU64,
    state: Mutex<State>,
}

/// Cloneable handle for one node's in-memory snapshot transport state.
#[derive(Clone)]
pub struct LocalSnapshotTransportStatus {
    inner: Arc<Inner>,
}

impl LocalSnapshotTransportStatus {
    pub(crate) fn new(observing_node_id: u64, configured_peers: BTreeSet<u64>) -> Self {
        let boot_nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map_or(0, |duration| duration.as_nanos());
        Self {
            inner: Arc::new(Inner {
                observing_node_id,
                boot_id: format!("{observing_node_id}-{boot_nanos}"),
                next_outbound_attempt: AtomicU64::new(0),
                next_outbound_connection: AtomicU64::new(0),
                state: Mutex::new(State {
                    next_inbound_attempt: 0,
                    next_inbound_socket_epoch: 0,
                    observations: BTreeMap::new(),
                    pending_inbound: BTreeMap::new(),
                    current_peers: configured_peers,
                    #[cfg(feature = "sqlite")]
                    sqlite_membership: None,
                }),
            }),
        }
    }

    /// Bind capacity and retention to OpenRaft's committed live membership.
    ///
    /// The receiver is an in-memory watch handle. Status recording and scrape
    /// paths only borrow its latest value; they never query the store or make a
    /// network request.
    #[cfg(feature = "sqlite")]
    pub(crate) fn bind_sqlite_membership(
        &self,
        membership: watch::Receiver<RaftMetrics<u64, crate::Node>>,
    ) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.sqlite_membership = Some(membership);
        refresh_current_peers(&mut state);
    }

    /// Allocate the node-global identity of one OpenRaft network client.
    pub(crate) fn next_outbound_connection_id(&self) -> u64 {
        self.inner
            .next_outbound_connection
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
    }

    /// Allocate the node-global identity of one snapshot transfer.
    pub(crate) fn next_outbound_attempt(
        &self,
        raft_group: &'static str,
        peer_node_id: u64,
        connection_id: u64,
    ) -> OutboundSnapshotAttempt {
        OutboundSnapshotAttempt {
            raft_group,
            peer_node_id,
            attempt_id: self
                .inner
                .next_outbound_attempt
                .fetch_add(1, Ordering::AcqRel)
                .wrapping_add(1),
            connection_id,
        }
    }

    #[cfg(test)]
    pub(crate) fn outbound_attempt_count(&self) -> u64 {
        self.inner.next_outbound_attempt.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn current_capacity(&self) -> usize {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        refresh_current_peers(&mut state);
        observation_capacity(state.current_peers.len())
    }

    /// Allocate an identity for one accepted inbound Raft WebSocket.
    pub(crate) fn next_inbound_socket_epoch(&self) -> u64 {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.next_inbound_socket_epoch = state.next_inbound_socket_epoch.saturating_add(1);
        state.next_inbound_socket_epoch
    }

    #[must_use]
    pub fn snapshot(&self) -> SnapshotTransportStatus {
        let now = Instant::now();
        let observed_at_unix_ms = unix_ms();
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        refresh_current_peers(&mut state);
        let current_peers = state.current_peers.clone();
        state.observations.retain(|key, observation| {
            current_peers.contains(&key.peer_node_id) || !observation_expired(observation, now)
        });
        retain_pending_for_observations(&mut state);
        let capacity = observation_capacity(current_peers.len());
        while state.observations.len() > capacity {
            if !evict_oldest_retired(&mut state, &current_peers) {
                break;
            }
        }
        let observations = state
            .observations
            .iter_mut()
            .filter_map(|(key, observation)| {
                let sample_age = now.saturating_duration_since(observation.last_update);
                if observation_expired(observation, now) {
                    return None;
                }
                let stalled = observation.phase.can_stall()
                    && observation
                        .deadline
                        .map_or(sample_age >= STALLED_AFTER, |deadline| now >= deadline);
                if stalled && observation.phase != SnapshotTransportPhase::Stalled {
                    let previous = observation.phase;
                    observation.phase = SnapshotTransportPhase::Stalled;
                    observation.last_error_category = Some("snapshot_stalled");
                    log_transition(self.inner.observing_node_id, key, previous, observation);
                }
                Some(SnapshotTransportObservation {
                    observing_node_id: self.inner.observing_node_id,
                    peer_node_id: key.peer_node_id,
                    raft_group: key.raft_group.to_owned(),
                    boot_id: self.inner.boot_id.clone(),
                    attempt_id: observation.attempt_id,
                    snapshot_id: observation.snapshot_id.clone(),
                    socket_epoch: observation.socket_epoch,
                    direction: key.direction,
                    attempted_offset: observation.attempted_offset,
                    acknowledged_offset: observation.acknowledged_offset,
                    locally_received_bytes: observation.locally_received_bytes,
                    total_bytes: observation.total_bytes,
                    attempt_age_ms: observation
                        .attempt_started
                        .map(|started| duration_ms(now.saturating_duration_since(started))),
                    last_acknowledgement_age_ms: observation
                        .last_acknowledgement
                        .map(|at| duration_ms(now.saturating_duration_since(at))),
                    last_local_receive_age_ms: observation
                        .last_local_receive
                        .map(|at| duration_ms(now.saturating_duration_since(at))),
                    active_deadline_remaining_ms: observation
                        .deadline
                        .map(|deadline| duration_ms(deadline.saturating_duration_since(now))),
                    sample_age_ms: duration_ms(sample_age),
                    phase: observation.phase,
                    reconnect_count: observation.connection_attempt_count.saturating_sub(1),
                    retry_count: observation.retry_count,
                    last_error_category: observation.last_error_category.map(str::to_owned),
                    operation_owns_work: observation.operation_owns_work,
                })
            })
            .collect();
        SnapshotTransportStatus {
            schema_version: 1,
            observing_node_id: self.inner.observing_node_id,
            observed_at_unix_ms,
            observations,
        }
    }

    pub(crate) fn connecting_owned(
        &self,
        raft_group: &'static str,
        peer_node_id: u64,
        connection_id: u64,
        socket_epoch: u64,
    ) {
        self.update_existing(
            raft_group,
            peer_node_id,
            SnapshotTransportDirection::Outbound,
            |observation, now| {
                if !observation.operation_owns_work
                    || observation.outbound_connection_id != Some(connection_id)
                {
                    return;
                }
                observation.socket_epoch = socket_epoch;
                observation.connection_attempt_count =
                    observation.connection_attempt_count.saturating_add(1);
                observation.phase = SnapshotTransportPhase::Connecting;
                observation.last_update = now;
                observation.last_error_category = None;
                observation.operation_owns_work = true;
            },
        );
    }

    pub(crate) fn begin_owned_outbound_attempt(
        &self,
        attempt: &OutboundSnapshotAttempt,
        snapshot_id: &str,
        socket: OutboundSnapshotSocket,
        deadline: Instant,
    ) {
        self.update(
            attempt.raft_group,
            attempt.peer_node_id,
            SnapshotTransportDirection::Outbound,
            |observation, now| {
                if observation.attempt_id > attempt.attempt_id {
                    return;
                }
                observation.attempt_id = attempt.attempt_id;
                observation.snapshot_id = Some(snapshot_id.to_owned());
                observation.socket_epoch = socket.epoch;
                observation.attempted_offset = None;
                observation.acknowledged_offset = None;
                observation.locally_received_bytes = None;
                observation.total_bytes = None;
                observation.attempt_started = Some(now);
                observation.last_acknowledgement = None;
                observation.last_local_receive = None;
                observation.deadline = Some(deadline);
                observation.phase = SnapshotTransportPhase::Transferring;
                observation.connection_attempt_count = u64::from(socket.connected);
                observation.retry_count = 0;
                observation.last_error_category = None;
                observation.operation_owns_work = true;
                observation.outbound_connection_id = Some(attempt.connection_id);
                observation.last_update = now;
            },
        );
    }

    pub(crate) fn outbound_chunk_owned(
        &self,
        attempt: &OutboundSnapshotAttempt,
        offset: u64,
        len: usize,
        done: bool,
        deadline: Instant,
    ) {
        self.update_outbound_attempt(attempt, |observation, now| {
            let end_offset = offset.saturating_add(len as u64);
            observation.attempted_offset = Some(end_offset);
            if done {
                observation.total_bytes = Some(end_offset);
            }
            observation.deadline = Some(deadline);
            observation.phase = if done {
                SnapshotTransportPhase::Installing
            } else {
                SnapshotTransportPhase::AwaitingAcknowledgement
            };
            observation.operation_owns_work = true;
            observation.last_update = now;
        });
    }

    pub(crate) fn outbound_acknowledged_owned(
        &self,
        attempt: &OutboundSnapshotAttempt,
        acknowledged_offset: u64,
        done: bool,
        transfer_deadline: Option<Instant>,
    ) {
        self.update_outbound_attempt(attempt, |observation, now| {
            observation.acknowledged_offset = Some(acknowledged_offset);
            observation.last_acknowledgement = Some(now);
            observation.deadline = (!done).then_some(transfer_deadline).flatten();
            observation.phase = if done {
                SnapshotTransportPhase::Complete
            } else {
                SnapshotTransportPhase::Transferring
            };
            observation.operation_owns_work = !done;
            observation.last_error_category = None;
            observation.last_update = now;
        });
    }

    pub(crate) fn outbound_retry_owned(
        &self,
        attempt: &OutboundSnapshotAttempt,
        category: &'static str,
        deadline: Option<Instant>,
    ) {
        self.update_outbound_attempt(attempt, |observation, now| {
            observation.retry_count = observation.retry_count.saturating_add(1);
            observation.phase = SnapshotTransportPhase::Retrying;
            observation.deadline = deadline;
            observation.last_error_category = Some(category);
            observation.operation_owns_work = true;
            observation.last_update = now;
        });
    }

    pub(crate) fn outbound_failed_owned(
        &self,
        attempt: &OutboundSnapshotAttempt,
        category: &'static str,
    ) {
        self.update_outbound_attempt(attempt, |observation, now| {
            observation.phase = SnapshotTransportPhase::Failed;
            observation.deadline = None;
            if category != "snapshot_attempt_ended" || observation.last_error_category.is_none() {
                observation.last_error_category = Some(category);
            }
            observation.operation_owns_work = false;
            observation.last_update = now;
        });
    }

    #[cfg(test)]
    pub(crate) fn connecting(
        &self,
        raft_group: &'static str,
        peer_node_id: u64,
        socket_epoch: u64,
    ) {
        self.connecting_owned(raft_group, peer_node_id, 1, socket_epoch);
    }

    #[cfg(test)]
    pub(crate) fn begin_outbound_attempt(
        &self,
        raft_group: &'static str,
        peer_node_id: u64,
        attempt_id: u64,
        snapshot_id: &str,
        socket: OutboundSnapshotSocket,
        deadline: Instant,
    ) -> OutboundSnapshotAttempt {
        let attempt = OutboundSnapshotAttempt {
            raft_group,
            peer_node_id,
            attempt_id,
            connection_id: 1,
        };
        self.begin_owned_outbound_attempt(&attempt, snapshot_id, socket, deadline);
        attempt
    }

    #[cfg(test)]
    pub(crate) fn outbound_chunk(
        &self,
        raft_group: &'static str,
        peer_node_id: u64,
        offset: u64,
        len: usize,
        done: bool,
        deadline: Instant,
    ) {
        if let Some(attempt) = self.current_outbound_attempt(raft_group, peer_node_id) {
            self.outbound_chunk_owned(&attempt, offset, len, done, deadline);
        }
    }

    #[cfg(test)]
    pub(crate) fn outbound_acknowledged(
        &self,
        raft_group: &'static str,
        peer_node_id: u64,
        acknowledged_offset: u64,
        done: bool,
        transfer_deadline: Option<Instant>,
    ) {
        if let Some(attempt) = self.current_outbound_attempt(raft_group, peer_node_id) {
            self.outbound_acknowledged_owned(
                &attempt,
                acknowledged_offset,
                done,
                transfer_deadline,
            );
        }
    }

    #[cfg(test)]
    pub(crate) fn outbound_retry(
        &self,
        raft_group: &'static str,
        peer_node_id: u64,
        category: &'static str,
        deadline: Option<Instant>,
    ) {
        if let Some(attempt) = self.current_outbound_attempt(raft_group, peer_node_id) {
            self.outbound_retry_owned(&attempt, category, deadline);
        }
    }

    #[cfg(test)]
    pub(crate) fn outbound_failed(
        &self,
        raft_group: &'static str,
        peer_node_id: u64,
        category: &'static str,
    ) {
        if let Some(attempt) = self.current_outbound_attempt(raft_group, peer_node_id) {
            self.outbound_failed_owned(&attempt, category);
        }
    }

    #[cfg(test)]
    fn current_outbound_attempt(
        &self,
        raft_group: &'static str,
        peer_node_id: u64,
    ) -> Option<OutboundSnapshotAttempt> {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let observation = state.observations.get(&ObservationKey {
            raft_group,
            peer_node_id,
            direction: SnapshotTransportDirection::Outbound,
        })?;
        Some(OutboundSnapshotAttempt {
            raft_group,
            peer_node_id,
            attempt_id: observation.attempt_id,
            connection_id: observation.outbound_connection_id?,
        })
    }

    pub(crate) fn inbound_received(
        &self,
        chunk: InboundSnapshotChunk<'_>,
    ) -> Option<InboundSnapshotAttempt> {
        let InboundSnapshotChunk {
            raft_group,
            peer_node_id,
            snapshot_id,
            offset,
            len,
            done,
            socket_epoch,
            deadline,
        } = chunk;
        let now = Instant::now();
        let key = ObservationKey {
            raft_group,
            peer_node_id,
            direction: SnapshotTransportDirection::Inbound,
        };
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !self.make_room(&mut state, now, &key) {
            return None;
        }
        let existing = state.observations.get(&key);
        let changed_snapshot = existing.and_then(|observation| observation.snapshot_id.as_deref())
            != Some(snapshot_id);
        let existing_attempt_id = existing.map(|observation| observation.attempt_id);
        let existing_end = existing.and_then(|observation| observation.attempted_offset);
        let existing_started = existing.and_then(|observation| observation.attempt_started);
        let existing_owns_work =
            existing.is_some_and(|observation| observation.operation_owns_work);
        if changed_snapshot {
            state.next_inbound_attempt = state.next_inbound_attempt.saturating_add(1);
        }
        let attempt_id = if changed_snapshot {
            state.next_inbound_attempt
        } else {
            existing_attempt_id.unwrap_or(state.next_inbound_attempt)
        };
        let previous_end = if changed_snapshot {
            0
        } else {
            existing_end.unwrap_or_default()
        };
        let end_offset = offset.saturating_add(len as u64);
        let attempted_offset = previous_end.max(end_offset);
        let attempt = InboundSnapshotAttempt {
            raft_group,
            peer_node_id,
            attempt_id,
            snapshot_id: snapshot_id.to_owned(),
            socket_epoch,
            attempted_offset,
            total_bytes: done.then_some(attempted_offset),
            attempt_started: if changed_snapshot {
                now
            } else {
                existing_started.unwrap_or(now)
            },
            deadline,
            done,
        };

        // The node-owned executor may still be completing an accepted request
        // after its socket disappeared. Keep that owned attempt authoritative
        // until it finishes; the queued replacement publishes itself when the
        // executor actually admits it.
        if changed_snapshot && existing_owns_work {
            state.pending_inbound.insert(
                key,
                PendingInboundSnapshot {
                    attempt: attempt.clone(),
                    disposition: None,
                },
            );
            return Some(attempt);
        }

        if changed_snapshot {
            state.pending_inbound.remove(&key);
        }

        let observation = state
            .observations
            .entry(key.clone())
            .or_insert_with(|| empty_observation(now));
        let previous = observation.phase;
        if changed_snapshot {
            reset_from_inbound_attempt(observation, &attempt, now);
        } else if observation.socket_epoch != socket_epoch {
            observation.connection_attempt_count =
                observation.connection_attempt_count.saturating_add(1);
            observation.socket_epoch = socket_epoch;
        }
        if !changed_snapshot && end_offset <= previous_end {
            observation.retry_count = observation.retry_count.saturating_add(1);
        }
        observation.attempted_offset = Some(attempted_offset);
        if done {
            observation.total_bytes = Some(attempted_offset);
        }
        if !observation.operation_owns_work {
            observation.deadline = Some(deadline);
            observation.phase = if done {
                SnapshotTransportPhase::Installing
            } else {
                SnapshotTransportPhase::Transferring
            };
        }
        observation.last_error_category = None;
        observation.last_update = now;
        log_transition(self.inner.observing_node_id, &key, previous, observation);
        Some(attempt)
    }

    pub(crate) fn inbound_admitted(
        &self,
        attempt: &InboundSnapshotAttempt,
        done: bool,
        deadline: Instant,
    ) {
        self.update_inbound_attempt(attempt, true, |observation, now| {
            observation.deadline = Some(deadline);
            observation.phase = if done {
                SnapshotTransportPhase::Installing
            } else {
                SnapshotTransportPhase::Transferring
            };
            observation.operation_owns_work = true;
            observation.last_error_category = None;
            observation.last_update = now;
        });
    }

    pub(crate) fn inbound_finished(
        &self,
        attempt: &InboundSnapshotAttempt,
        locally_received_offset: u64,
        done: bool,
        next_chunk_deadline: Option<Instant>,
        disposition: InboundSnapshotDisposition,
    ) {
        self.update_inbound_attempt(attempt, false, |observation, now| {
            observation.operation_owns_work = false;
            observation.last_update = now;
            match disposition {
                InboundSnapshotDisposition::Succeeded => {
                    // Finishing the node-owned install only means bytes are
                    // durable locally. The response still has to cross and flush
                    // the WebSocket before the sender can call it acknowledged.
                    observation.deadline = (!done).then_some(next_chunk_deadline).flatten();
                    let previous = observation.locally_received_bytes.unwrap_or_default();
                    if locally_received_offset > previous {
                        observation.locally_received_bytes = Some(locally_received_offset);
                        observation.last_local_receive = Some(now);
                    }
                    if done {
                        observation.total_bytes = Some(
                            observation
                                .total_bytes
                                .unwrap_or_default()
                                .max(locally_received_offset),
                        );
                    }
                    observation.phase = if done {
                        SnapshotTransportPhase::Complete
                    } else {
                        SnapshotTransportPhase::Transferring
                    };
                    observation.last_error_category = None;
                }
                InboundSnapshotDisposition::Retrying(category) => {
                    observation.phase = SnapshotTransportPhase::Retrying;
                    observation.deadline = (!done).then_some(next_chunk_deadline).flatten();
                    observation.last_error_category = Some(category);
                    observation.retry_count = observation.retry_count.saturating_add(1);
                }
                InboundSnapshotDisposition::Failed(category) => {
                    observation.phase = SnapshotTransportPhase::Failed;
                    observation.deadline = None;
                    observation.last_error_category = Some(category);
                }
            }
        });
    }

    pub(crate) fn inbound_request_ended(
        &self,
        attempt: &InboundSnapshotAttempt,
        next_chunk_deadline: Option<Instant>,
        disposition: InboundSnapshotDisposition,
    ) {
        if self.record_pending_inbound_disposition(attempt, next_chunk_deadline, disposition) {
            return;
        }
        self.update_inbound_attempt(attempt, false, |observation, now| {
            // A worker may win the race with socket teardown. Never replace
            // its running or already-published result with an admission error.
            if observation.operation_owns_work
                || observation.phase == SnapshotTransportPhase::Complete
                || observation.last_error_category.is_some()
            {
                return;
            }
            observation.last_update = now;
            match disposition {
                InboundSnapshotDisposition::Succeeded => {}
                InboundSnapshotDisposition::Retrying(category) => {
                    observation.phase = SnapshotTransportPhase::Retrying;
                    observation.deadline = next_chunk_deadline;
                    observation.last_error_category = Some(category);
                    observation.retry_count = observation.retry_count.saturating_add(1);
                }
                InboundSnapshotDisposition::Failed(category) => {
                    observation.phase = SnapshotTransportPhase::Failed;
                    observation.deadline = None;
                    observation.last_error_category = Some(category);
                }
            }
        });
    }

    fn record_pending_inbound_disposition(
        &self,
        attempt: &InboundSnapshotAttempt,
        next_chunk_deadline: Option<Instant>,
        disposition: InboundSnapshotDisposition,
    ) -> bool {
        let key = ObservationKey {
            raft_group: attempt.raft_group,
            peer_node_id: attempt.peer_node_id,
            direction: SnapshotTransportDirection::Inbound,
        };
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(pending) = state.pending_inbound.get_mut(&key) else {
            return false;
        };
        if pending.attempt.attempt_id != attempt.attempt_id {
            return false;
        }
        pending.disposition = Some((next_chunk_deadline, disposition));
        true
    }

    fn update_inbound_attempt(
        &self,
        attempt: &InboundSnapshotAttempt,
        admit: bool,
        mutate: impl FnOnce(&mut Observation, Instant),
    ) {
        let now = Instant::now();
        let key = ObservationKey {
            raft_group: attempt.raft_group,
            peer_node_id: attempt.peer_node_id,
            direction: SnapshotTransportDirection::Inbound,
        };
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !self.make_room(&mut state, now, &key) {
            return;
        }
        {
            let observation = state
                .observations
                .entry(key.clone())
                .or_insert_with(|| empty_observation(now));
            if observation.attempt_id != attempt.attempt_id {
                if !admit
                    || observation.operation_owns_work
                    || attempt.attempt_id < observation.attempt_id
                {
                    return;
                }
                reset_from_inbound_attempt(observation, attempt, now);
            }
            let previous = observation.phase;
            mutate(observation, now);
            log_transition(self.inner.observing_node_id, &key, previous, observation);
        }

        let current_attempt_id = state
            .observations
            .get(&key)
            .map_or(0, |observation| observation.attempt_id);
        let current_owns_work = state
            .observations
            .get(&key)
            .is_some_and(|observation| observation.operation_owns_work);
        if current_owns_work {
            if admit
                && state
                    .pending_inbound
                    .get(&key)
                    .is_some_and(|pending| pending.attempt.attempt_id <= current_attempt_id)
            {
                state.pending_inbound.remove(&key);
            }
            return;
        }
        let Some(pending) = state.pending_inbound.remove(&key) else {
            return;
        };
        if pending.attempt.attempt_id <= current_attempt_id {
            return;
        }
        let observation = state
            .observations
            .get_mut(&key)
            .expect("inbound observation exists after update");
        let previous = observation.phase;
        reset_from_inbound_attempt(observation, &pending.attempt, now);
        if let Some((next_chunk_deadline, disposition)) = pending.disposition {
            apply_pending_inbound_disposition(
                observation,
                pending.attempt.done,
                next_chunk_deadline,
                disposition,
                now,
            );
        }
        log_transition(self.inner.observing_node_id, &key, previous, observation);
    }

    fn update(
        &self,
        raft_group: &'static str,
        peer_node_id: u64,
        direction: SnapshotTransportDirection,
        mutate: impl FnOnce(&mut Observation, Instant),
    ) {
        let now = Instant::now();
        let key = ObservationKey {
            raft_group,
            peer_node_id,
            direction,
        };
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !self.make_room(&mut state, now, &key) {
            return;
        }
        let observation = state
            .observations
            .entry(key.clone())
            .or_insert_with(|| empty_observation(now));
        let previous = observation.phase;
        mutate(observation, now);
        log_transition(self.inner.observing_node_id, &key, previous, observation);
    }

    fn update_existing(
        &self,
        raft_group: &'static str,
        peer_node_id: u64,
        direction: SnapshotTransportDirection,
        mutate: impl FnOnce(&mut Observation, Instant),
    ) {
        let now = Instant::now();
        let key = ObservationKey {
            raft_group,
            peer_node_id,
            direction,
        };
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(observation) = state.observations.get_mut(&key) else {
            return;
        };
        let previous = observation.phase;
        mutate(observation, now);
        log_transition(self.inner.observing_node_id, &key, previous, observation);
    }

    fn update_outbound_attempt(
        &self,
        attempt: &OutboundSnapshotAttempt,
        mutate: impl FnOnce(&mut Observation, Instant),
    ) {
        self.update_existing(
            attempt.raft_group,
            attempt.peer_node_id,
            SnapshotTransportDirection::Outbound,
            |observation, now| {
                if observation.attempt_id != attempt.attempt_id
                    || observation.outbound_connection_id != Some(attempt.connection_id)
                {
                    return;
                }
                mutate(observation, now);
            },
        );
    }

    fn make_room(&self, state: &mut State, now: Instant, incoming: &ObservationKey) -> bool {
        refresh_current_peers(state);
        let current_peers = state.current_peers.clone();
        state.observations.retain(|key, observation| {
            current_peers.contains(&key.peer_node_id) || !observation_expired(observation, now)
        });
        retain_pending_for_observations(state);
        let capacity = observation_capacity(current_peers.len());
        if state.observations.contains_key(incoming) || state.observations.len() < capacity {
            return true;
        }
        evict_oldest_retired(state, &current_peers)
    }
}

fn evict_oldest_retired(state: &mut State, current_peers: &BTreeSet<u64>) -> bool {
    let oldest_retired = state
        .observations
        .iter()
        .filter(|(key, _)| !current_peers.contains(&key.peer_node_id))
        .min_by_key(|(_, observation)| (observation.operation_owns_work, observation.last_update))
        .map(|(key, _)| key.clone());
    if let Some(key) = oldest_retired {
        state.observations.remove(&key);
        state.pending_inbound.remove(&key);
        true
    } else {
        false
    }
}

fn observation_capacity(peer_count: usize) -> usize {
    peer_count
        .saturating_mul(OBSERVATIONS_PER_PEER)
        .saturating_add(RETIRED_PEER_ALLOWANCE * OBSERVATIONS_PER_PEER)
        .max(OBSERVATIONS_PER_PEER)
}

#[cfg(feature = "sqlite")]
fn refresh_current_peers(state: &mut State) {
    if let Some(membership) = state.sqlite_membership.as_ref() {
        let current_peers = membership
            .borrow()
            .membership_config
            .nodes()
            .map(|(node_id, _)| *node_id)
            .collect();
        state.current_peers = current_peers;
    }
}

#[cfg(not(feature = "sqlite"))]
fn refresh_current_peers(_state: &mut State) {}

fn empty_observation(now: Instant) -> Observation {
    Observation {
        attempt_id: 0,
        snapshot_id: None,
        socket_epoch: 0,
        attempted_offset: None,
        acknowledged_offset: None,
        locally_received_bytes: None,
        total_bytes: None,
        attempt_started: None,
        last_acknowledgement: None,
        last_local_receive: None,
        deadline: None,
        last_update: now,
        phase: SnapshotTransportPhase::Connecting,
        connection_attempt_count: 0,
        retry_count: 0,
        last_error_category: None,
        operation_owns_work: false,
        outbound_connection_id: None,
    }
}

fn reset_from_inbound_attempt(
    observation: &mut Observation,
    attempt: &InboundSnapshotAttempt,
    now: Instant,
) {
    *observation = empty_observation(now);
    observation.attempt_id = attempt.attempt_id;
    observation.snapshot_id = Some(attempt.snapshot_id.clone());
    observation.socket_epoch = attempt.socket_epoch;
    observation.attempted_offset = Some(attempt.attempted_offset);
    observation.total_bytes = attempt.total_bytes;
    observation.attempt_started = Some(attempt.attempt_started);
    observation.deadline = Some(attempt.deadline);
    observation.phase = if attempt.done {
        SnapshotTransportPhase::Installing
    } else {
        SnapshotTransportPhase::Transferring
    };
    observation.connection_attempt_count = 1;
}

fn apply_pending_inbound_disposition(
    observation: &mut Observation,
    done: bool,
    next_chunk_deadline: Option<Instant>,
    disposition: InboundSnapshotDisposition,
    now: Instant,
) {
    observation.operation_owns_work = false;
    observation.last_update = now;
    match disposition {
        InboundSnapshotDisposition::Succeeded => {}
        InboundSnapshotDisposition::Retrying(category) => {
            observation.phase = SnapshotTransportPhase::Retrying;
            observation.deadline = (!done).then_some(next_chunk_deadline).flatten();
            observation.last_error_category = Some(category);
            observation.retry_count = observation.retry_count.saturating_add(1);
        }
        InboundSnapshotDisposition::Failed(category) => {
            observation.phase = SnapshotTransportPhase::Failed;
            observation.deadline = None;
            observation.last_error_category = Some(category);
        }
    }
}

fn retain_pending_for_observations(state: &mut State) {
    let observed = state.observations.keys().cloned().collect::<BTreeSet<_>>();
    state
        .pending_inbound
        .retain(|key, _| observed.contains(key));
}

fn observation_expired(observation: &Observation, now: Instant) -> bool {
    let freshness_horizon = observation
        .deadline
        .map_or(observation.last_update, |deadline| {
            deadline.max(observation.last_update)
        });
    now.saturating_duration_since(freshness_horizon) > EXPIRE_AFTER
}

fn log_transition(
    observing_node_id: u64,
    key: &ObservationKey,
    previous: SnapshotTransportPhase,
    observation: &Observation,
) {
    if previous == observation.phase {
        return;
    }
    tracing::info!(
        observing_node_id,
        peer_node_id = key.peer_node_id,
        raft_group = key.raft_group,
        direction = ?key.direction,
        attempt_id = observation.attempt_id,
        snapshot_id = observation.snapshot_id.as_deref().unwrap_or("unknown"),
        socket_epoch = observation.socket_epoch,
        attempted_offset = observation.attempted_offset,
        acknowledged_offset = observation.acknowledged_offset,
        phase = ?observation.phase,
        "snapshot transport phase transition"
    );
    match observation.phase {
        SnapshotTransportPhase::Complete => tracing::info!(
            observing_node_id,
            peer_node_id = key.peer_node_id,
            raft_group = key.raft_group,
            direction = ?key.direction,
            attempt_id = observation.attempt_id,
            snapshot_id = observation.snapshot_id.as_deref().unwrap_or("unknown"),
            attempted_offset = observation.attempted_offset,
            acknowledged_offset = observation.acknowledged_offset,
            locally_received_bytes = observation.locally_received_bytes,
            "snapshot transport completed"
        ),
        SnapshotTransportPhase::Failed => tracing::warn!(
            observing_node_id,
            peer_node_id = key.peer_node_id,
            raft_group = key.raft_group,
            direction = ?key.direction,
            attempt_id = observation.attempt_id,
            snapshot_id = observation.snapshot_id.as_deref().unwrap_or("unknown"),
            attempted_offset = observation.attempted_offset,
            acknowledged_offset = observation.acknowledged_offset,
            locally_received_bytes = observation.locally_received_bytes,
            error_category = observation.last_error_category.unwrap_or("unknown"),
            "snapshot transport failed"
        ),
        _ => {}
    }
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(duration_ms)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "sqlite")]
    fn membership_metrics(node_ids: &[u64]) -> RaftMetrics<u64, crate::Node> {
        use openraft::{Membership, StoredMembership};

        let nodes: BTreeMap<u64, crate::Node> = node_ids
            .iter()
            .map(|node_id| {
                (
                    *node_id,
                    crate::Node {
                        id: *node_id,
                        addr_raft: format!("127.0.0.1:{}", 8_100 + node_id),
                        addr_api: format!("127.0.0.1:{}", 8_200 + node_id),
                    },
                )
            })
            .collect();
        let voters = BTreeSet::from([*node_ids
            .first()
            .expect("test membership must contain one voter")]);
        let mut metrics = RaftMetrics::new_initial(1);
        metrics.membership_config = Arc::new(StoredMembership::new(
            None,
            Membership::new(vec![voters], nodes),
        ));
        metrics
    }

    #[tokio::test(start_paused = true)]
    async fn local_status_distinguishes_receive_ack_install_retry_and_completion() {
        let status = LocalSnapshotTransportStatus::new(1, BTreeSet::from([2]));
        let deadline = Instant::now() + Duration::from_secs(30);
        // Ordinary connection churn is not snapshot work and must not create
        // a status record that later projects as a stalled snapshot.
        status.connecting("sqlite", 2, 0);
        assert!(status.snapshot().observations.is_empty());
        status.begin_outbound_attempt(
            "sqlite",
            2,
            7,
            "snap-a",
            OutboundSnapshotSocket {
                epoch: 0,
                connected: true,
            },
            deadline,
        );
        status.connecting("sqlite", 2, 1);
        assert_eq!(status.snapshot().observations[0].reconnect_count, 1);
        status.connecting("sqlite", 2, 2);
        assert_eq!(status.snapshot().observations[0].reconnect_count, 2);
        status.outbound_chunk("sqlite", 2, 0, 100, false, deadline);
        let waiting = status.snapshot().observations.remove(0);
        assert_eq!(
            waiting.phase,
            SnapshotTransportPhase::AwaitingAcknowledgement
        );
        assert_eq!(waiting.attempted_offset, Some(100));
        assert_eq!(waiting.acknowledged_offset, None);

        status.outbound_acknowledged("sqlite", 2, 100, false, Some(deadline));
        assert_eq!(
            status.snapshot().observations[0].phase,
            SnapshotTransportPhase::Transferring
        );
        status.outbound_retry("sqlite", 2, "snapshot_mismatch", Some(deadline));
        assert_eq!(
            status.snapshot().observations[0].phase,
            SnapshotTransportPhase::Retrying
        );
        status.outbound_chunk("sqlite", 2, 100, 50, true, deadline);
        let installing = &status.snapshot().observations[0];
        assert_eq!(installing.phase, SnapshotTransportPhase::Installing);
        assert_eq!(installing.total_bytes, Some(150));
        status.outbound_acknowledged("sqlite", 2, 150, true, Some(deadline));
        assert_eq!(
            status.snapshot().observations[0].phase,
            SnapshotTransportPhase::Complete
        );
    }

    #[tokio::test(start_paused = true)]
    async fn receiver_bytes_are_not_reported_as_sender_acknowledgements() {
        let status = LocalSnapshotTransportStatus::new(2, BTreeSet::from([1]));
        status.inbound_received(InboundSnapshotChunk {
            raft_group: "sqlite",
            peer_node_id: 1,
            snapshot_id: "snap-a",
            offset: 0,
            len: 64,
            done: false,
            socket_epoch: 1,
            deadline: Instant::now() + Duration::from_secs(30),
        });
        let received = &status.snapshot().observations[0];
        assert_eq!(received.locally_received_bytes, None);
        assert_eq!(received.acknowledged_offset, None);
        assert!(!received.operation_owns_work);

        let attempt = status
            .inbound_received(InboundSnapshotChunk {
                raft_group: "sqlite",
                peer_node_id: 1,
                snapshot_id: "snap-a",
                offset: 64,
                len: 32,
                done: true,
                socket_epoch: 1,
                deadline: Instant::now() + Duration::from_secs(120),
            })
            .expect("track final inbound chunk");
        assert_eq!(status.snapshot().observations[0].total_bytes, Some(96));

        status.inbound_admitted(&attempt, true, Instant::now() + Duration::from_secs(120));
        assert!(status.snapshot().observations[0].operation_owns_work);

        status.inbound_finished(
            &attempt,
            96,
            true,
            None,
            InboundSnapshotDisposition::Succeeded,
        );
        let installed = &status.snapshot().observations[0];
        assert_eq!(installed.locally_received_bytes, Some(96));
        assert_eq!(installed.acknowledged_offset, None);
        assert_eq!(installed.phase, SnapshotTransportPhase::Complete);
        assert!(!installed.operation_owns_work);
    }

    #[tokio::test(start_paused = true)]
    async fn inbound_progress_is_monotonic_and_new_identity_clears_attempt_state() {
        let status = LocalSnapshotTransportStatus::new(2, BTreeSet::from([1]));
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut attempt = None;
        for (offset, len) in [(0, 64), (64, 32), (32, 16)] {
            attempt = status.inbound_received(InboundSnapshotChunk {
                raft_group: "sqlite",
                peer_node_id: 1,
                snapshot_id: "snap-a",
                offset,
                len,
                done: false,
                socket_epoch: 1,
                deadline,
            });
        }
        let first = &status.snapshot().observations[0];
        assert_eq!(first.attempted_offset, Some(96));
        assert_eq!(first.retry_count, 1);

        status.inbound_finished(
            &attempt.expect("track inbound attempt"),
            96,
            false,
            Some(deadline),
            InboundSnapshotDisposition::Succeeded,
        );
        assert_eq!(
            status.snapshot().observations[0].phase,
            SnapshotTransportPhase::Transferring
        );
        status.inbound_received(InboundSnapshotChunk {
            raft_group: "sqlite",
            peer_node_id: 1,
            snapshot_id: "snap-b",
            offset: 0,
            len: 8,
            done: false,
            socket_epoch: 2,
            deadline,
        });
        let next = &status.snapshot().observations[0];
        assert_eq!(next.snapshot_id.as_deref(), Some("snap-b"));
        assert_eq!(next.attempted_offset, Some(8));
        assert_eq!(next.locally_received_bytes, None);
        assert_eq!(next.total_bytes, None);
        assert_eq!(next.retry_count, 0);
        assert_eq!(next.last_local_receive_age_ms, None);
    }

    #[tokio::test(start_paused = true)]
    async fn same_peer_inbound_install_and_outbound_transfer_do_not_collide() {
        let status = LocalSnapshotTransportStatus::new(1, BTreeSet::from([2]));
        let deadline = Instant::now() + Duration::from_secs(30);
        status.begin_outbound_attempt(
            "sqlite",
            2,
            7,
            "outbound",
            OutboundSnapshotSocket {
                epoch: 1,
                connected: true,
            },
            deadline,
        );
        let inbound_attempt = status
            .inbound_received(InboundSnapshotChunk {
                raft_group: "sqlite",
                peer_node_id: 2,
                snapshot_id: "inbound",
                offset: 0,
                len: 64,
                done: true,
                socket_epoch: 2,
                deadline,
            })
            .expect("track inbound snapshot");
        status.inbound_admitted(&inbound_attempt, true, deadline);

        let snapshot = status.snapshot();
        assert_eq!(snapshot.observations.len(), 2);
        let inbound = snapshot
            .observations
            .iter()
            .find(|observation| observation.direction == SnapshotTransportDirection::Inbound)
            .expect("inbound status");
        let outbound = snapshot
            .observations
            .iter()
            .find(|observation| observation.direction == SnapshotTransportDirection::Outbound)
            .expect("outbound status");
        assert_eq!(inbound.phase, SnapshotTransportPhase::Installing);
        assert_eq!(outbound.phase, SnapshotTransportPhase::Transferring);
        assert!(inbound.operation_owns_work);
        assert!(outbound.operation_owns_work);
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_wrapper_preserves_the_actionable_failure_category() {
        let status = LocalSnapshotTransportStatus::new(1, BTreeSet::from([2]));
        let deadline = Instant::now() + Duration::from_secs(30);
        status.begin_outbound_attempt(
            "sqlite",
            2,
            7,
            "snapshot",
            OutboundSnapshotSocket {
                epoch: 1,
                connected: true,
            },
            deadline,
        );
        status.outbound_retry("sqlite", 2, "snapshot_mismatch", Some(deadline));
        status.outbound_failed("sqlite", 2, "snapshot_attempt_ended");
        let observation = &status.snapshot().observations[0];
        assert_eq!(observation.phase, SnapshotTransportPhase::Failed);
        assert_eq!(
            observation.last_error_category.as_deref(),
            Some("snapshot_mismatch")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn specific_terminal_failure_replaces_prior_transient_category() {
        let status = LocalSnapshotTransportStatus::new(1, BTreeSet::from([2]));
        let deadline = Instant::now() + Duration::from_secs(30);
        status.begin_outbound_attempt(
            "sqlite",
            2,
            7,
            "snapshot",
            OutboundSnapshotSocket {
                epoch: 1,
                connected: true,
            },
            deadline,
        );
        status.outbound_retry("sqlite", 2, "snapshot_mismatch", Some(deadline));
        status.outbound_failed("sqlite", 2, "higher_vote");

        let observation = &status.snapshot().observations[0];
        assert_eq!(observation.phase, SnapshotTransportPhase::Failed);
        assert_eq!(
            observation.last_error_category.as_deref(),
            Some("higher_vote")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn outbound_live_socket_counts_the_first_reconnect() {
        let status = LocalSnapshotTransportStatus::new(1, BTreeSet::from([2]));
        let deadline = Instant::now() + Duration::from_secs(30);
        status.begin_outbound_attempt(
            "sqlite",
            2,
            7,
            "snapshot",
            OutboundSnapshotSocket {
                epoch: 11,
                connected: true,
            },
            deadline,
        );
        assert_eq!(status.snapshot().observations[0].reconnect_count, 0);

        status.connecting("sqlite", 2, 12);
        let reconnected = &status.snapshot().observations[0];
        assert_eq!(reconnected.socket_epoch, 12);
        assert_eq!(reconnected.reconnect_count, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn stale_inbound_completion_cannot_mutate_newer_snapshot_identity() {
        let status = LocalSnapshotTransportStatus::new(2, BTreeSet::from([1]));
        let deadline = Instant::now() + Duration::from_secs(30);
        let old = status
            .inbound_received(InboundSnapshotChunk {
                raft_group: "sqlite",
                peer_node_id: 1,
                snapshot_id: "old",
                offset: 0,
                len: 64,
                done: true,
                socket_epoch: 1,
                deadline,
            })
            .expect("track old attempt");
        status.inbound_admitted(&old, true, deadline);
        let new = status
            .inbound_received(InboundSnapshotChunk {
                raft_group: "sqlite",
                peer_node_id: 1,
                snapshot_id: "new",
                offset: 0,
                len: 8,
                done: true,
                socket_epoch: 2,
                deadline,
            })
            .expect("track queued replacement");

        status.inbound_finished(&old, 64, true, None, InboundSnapshotDisposition::Succeeded);
        status.inbound_admitted(&new, true, deadline);
        status.inbound_finished(
            &old,
            1_024,
            true,
            None,
            InboundSnapshotDisposition::Succeeded,
        );

        let observation = &status.snapshot().observations[0];
        assert_eq!(observation.snapshot_id.as_deref(), Some("new"));
        assert_eq!(observation.attempt_id, new.attempt_id);
        assert_eq!(observation.attempted_offset, Some(8));
        assert_eq!(observation.locally_received_bytes, None);
        assert_eq!(observation.phase, SnapshotTransportPhase::Installing);
        assert!(observation.operation_owns_work);
    }

    #[tokio::test(start_paused = true)]
    async fn abandoned_inbound_admission_records_retry_without_stealing_worker_result() {
        let status = LocalSnapshotTransportStatus::new(2, BTreeSet::from([1]));
        let deadline = Instant::now() + Duration::from_secs(30);
        let owned = status
            .inbound_received(InboundSnapshotChunk {
                raft_group: "sqlite",
                peer_node_id: 1,
                snapshot_id: "worker-owned",
                offset: 0,
                len: 64,
                done: true,
                socket_epoch: 1,
                deadline,
            })
            .expect("track worker-owned attempt");
        status.inbound_admitted(&owned, true, deadline);
        let abandoned = status
            .inbound_received(InboundSnapshotChunk {
                raft_group: "sqlite",
                peer_node_id: 1,
                snapshot_id: "abandoned-admission",
                offset: 0,
                len: 8,
                done: false,
                socket_epoch: 2,
                deadline,
            })
            .expect("track queued admission");
        status.inbound_request_ended(
            &abandoned,
            Some(deadline),
            InboundSnapshotDisposition::Retrying("snapshot_connection_closed"),
        );
        let installing = &status.snapshot().observations[0];
        assert_eq!(installing.snapshot_id.as_deref(), Some("worker-owned"));
        assert_eq!(installing.phase, SnapshotTransportPhase::Installing);
        assert!(installing.operation_owns_work);

        status.inbound_finished(
            &owned,
            64,
            true,
            None,
            InboundSnapshotDisposition::Succeeded,
        );
        let retry = &status.snapshot().observations[0];
        assert_eq!(retry.snapshot_id.as_deref(), Some("abandoned-admission"));
        assert_eq!(retry.attempted_offset, Some(8));
        assert_eq!(retry.locally_received_bytes, None);
        assert_eq!(retry.phase, SnapshotTransportPhase::Retrying);
        assert_eq!(
            retry.last_error_category.as_deref(),
            Some("snapshot_connection_closed")
        );

        status.inbound_finished(
            &owned,
            1_024,
            true,
            None,
            InboundSnapshotDisposition::Succeeded,
        );
        assert_eq!(
            status.snapshot().observations[0].snapshot_id.as_deref(),
            Some("abandoned-admission")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn inbound_socket_epoch_and_reconnect_count_are_attempt_local() {
        let status = LocalSnapshotTransportStatus::new(2, BTreeSet::from([1]));
        let deadline = Instant::now() + Duration::from_secs(30);
        for socket_epoch in [11, 12] {
            status.inbound_received(InboundSnapshotChunk {
                raft_group: "sqlite",
                peer_node_id: 1,
                snapshot_id: "same",
                offset: 0,
                len: 64,
                done: false,
                socket_epoch,
                deadline,
            });
        }
        let reconnected = &status.snapshot().observations[0];
        assert_eq!(reconnected.socket_epoch, 12);
        assert_eq!(reconnected.reconnect_count, 1);

        status.inbound_received(InboundSnapshotChunk {
            raft_group: "sqlite",
            peer_node_id: 1,
            snapshot_id: "new",
            offset: 0,
            len: 8,
            done: false,
            socket_epoch: 13,
            deadline,
        });
        let new_attempt = &status.snapshot().observations[0];
        assert_eq!(new_attempt.socket_epoch, 13);
        assert_eq!(new_attempt.reconnect_count, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn configured_inbound_inter_chunk_deadline_uses_minimum_and_maximum() {
        let status = LocalSnapshotTransportStatus::new(2, BTreeSet::from([1]));
        let minimum = Instant::now() + Duration::from_secs(5);
        let minimum_attempt = status
            .inbound_received(InboundSnapshotChunk {
                raft_group: "sqlite",
                peer_node_id: 1,
                snapshot_id: "minimum",
                offset: 0,
                len: 64,
                done: false,
                socket_epoch: 1,
                deadline: minimum,
            })
            .expect("track minimum attempt");
        status.inbound_admitted(&minimum_attempt, false, minimum);
        status.inbound_finished(
            &minimum_attempt,
            64,
            false,
            Some(minimum),
            InboundSnapshotDisposition::Succeeded,
        );
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(
            status.snapshot().observations[0].phase,
            SnapshotTransportPhase::Stalled
        );

        let maximum = Instant::now() + Duration::from_secs(300);
        let maximum_attempt = status
            .inbound_received(InboundSnapshotChunk {
                raft_group: "sqlite",
                peer_node_id: 1,
                snapshot_id: "maximum",
                offset: 0,
                len: 64,
                done: false,
                socket_epoch: 2,
                deadline: maximum,
            })
            .expect("track maximum attempt");
        status.inbound_admitted(&maximum_attempt, false, maximum);
        status.inbound_finished(
            &maximum_attempt,
            64,
            false,
            Some(maximum),
            InboundSnapshotDisposition::Succeeded,
        );
        tokio::time::advance(STALLED_AFTER).await;
        assert_eq!(
            status.snapshot().observations[0].phase,
            SnapshotTransportPhase::Transferring
        );
        tokio::time::advance(Duration::from_secs(270)).await;
        assert_eq!(
            status.snapshot().observations[0].phase,
            SnapshotTransportPhase::Stalled
        );
    }

    #[tokio::test(start_paused = true)]
    async fn configured_peer_capacity_covers_both_groups_and_directions() {
        let configured = BTreeSet::from([1, 2, 3, 4, 5, 6, 7]);
        let status = LocalSnapshotTransportStatus::new(1, configured);
        let deadline = Instant::now() + Duration::from_secs(30);
        for peer in 2..=7 {
            for group in ["sqlite", "cache"] {
                status.begin_outbound_attempt(
                    group,
                    peer,
                    peer,
                    "outbound",
                    OutboundSnapshotSocket {
                        epoch: 1,
                        connected: true,
                    },
                    deadline,
                );
                status.inbound_received(InboundSnapshotChunk {
                    raft_group: group,
                    peer_node_id: peer,
                    snapshot_id: "inbound",
                    offset: 0,
                    len: 64,
                    done: false,
                    socket_epoch: peer,
                    deadline,
                });
            }
        }

        assert_eq!(status.snapshot().observations.len(), 24);
        assert!(status.current_capacity() >= 24);
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test(start_paused = true)]
    async fn live_membership_add_remove_churn_rekeys_capacity_and_retired_allowance() {
        let status = LocalSnapshotTransportStatus::new(1, BTreeSet::from([1, 2]));
        let (membership_tx, membership_rx) = watch::channel(membership_metrics(&[1, 2]));
        status.bind_sqlite_membership(membership_rx);

        membership_tx.send_replace(membership_metrics(&[1, 2, 3, 4, 5, 6, 7, 8]));
        let deadline = Instant::now() + Duration::from_secs(30);
        for peer in 2..=8 {
            for group in ["sqlite", "cache"] {
                status.begin_outbound_attempt(
                    group,
                    peer,
                    peer,
                    "outbound",
                    OutboundSnapshotSocket {
                        epoch: 1,
                        connected: true,
                    },
                    deadline,
                );
                status.inbound_received(InboundSnapshotChunk {
                    raft_group: group,
                    peer_node_id: peer,
                    snapshot_id: "inbound",
                    offset: 0,
                    len: 64,
                    done: false,
                    socket_epoch: 1,
                    deadline,
                });
            }
        }
        let expanded = status.snapshot();
        assert_eq!(expanded.observations.len(), 28);
        assert!(
            expanded
                .observations
                .iter()
                .any(|item| item.peer_node_id == 8)
        );

        // The observing node and old voters may themselves be removed. The
        // latest committed membership owns the protected slots immediately.
        membership_tx.send_replace(membership_metrics(&[8]));
        for peer in 20..=40 {
            for group in ["sqlite", "cache"] {
                status.begin_outbound_attempt(
                    group,
                    peer,
                    peer,
                    "retired",
                    OutboundSnapshotSocket {
                        epoch: 1,
                        connected: true,
                    },
                    deadline,
                );
                status.inbound_received(InboundSnapshotChunk {
                    raft_group: group,
                    peer_node_id: peer,
                    snapshot_id: "retired",
                    offset: 0,
                    len: 1,
                    done: false,
                    socket_epoch: 1,
                    deadline,
                });
            }
        }

        let contracted = status.snapshot();
        assert!(contracted.observations.len() <= observation_capacity(1));
        assert_eq!(
            contracted
                .observations
                .iter()
                .filter(|item| item.peer_node_id == 8)
                .count(),
            OBSERVATIONS_PER_PEER
        );
        assert!(
            contracted
                .observations
                .iter()
                .filter(|item| item.peer_node_id != 8)
                .count()
                <= RETIRED_PEER_ALLOWANCE * OBSERVATIONS_PER_PEER
        );
    }

    #[tokio::test(start_paused = true)]
    async fn active_observation_remains_visible_through_deadline_then_expires() {
        let status = LocalSnapshotTransportStatus::new(1, BTreeSet::from([2]));
        let deadline = Instant::now() + Duration::from_secs(120);
        status.begin_outbound_attempt(
            "sqlite",
            2,
            7,
            "snap-a",
            OutboundSnapshotSocket {
                epoch: 3,
                connected: true,
            },
            deadline,
        );

        tokio::time::advance(Duration::from_secs(119)).await;
        assert_eq!(
            status.snapshot().observations[0].phase,
            SnapshotTransportPhase::Transferring
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        let stalled = status.snapshot().observations.remove(0);
        assert_eq!(stalled.phase, SnapshotTransportPhase::Stalled);
        assert_eq!(
            stalled.last_error_category.as_deref(),
            Some("snapshot_stalled")
        );
        assert_eq!(stalled.sample_age_ms, duration_ms(Duration::from_secs(120)));

        tokio::time::advance(EXPIRE_AFTER + Duration::from_millis(1)).await;
        assert!(status.snapshot().observations.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn live_install_longer_than_five_minutes_remains_visible_through_its_deadline() {
        let status = LocalSnapshotTransportStatus::new(1, BTreeSet::from([2]));
        let deadline = Instant::now() + Duration::from_secs(3_600);
        status.begin_outbound_attempt(
            "sqlite",
            2,
            7,
            "long-install",
            OutboundSnapshotSocket {
                epoch: 1,
                connected: true,
            },
            deadline,
        );
        status.outbound_chunk("sqlite", 2, 0, 64, true, deadline);

        tokio::time::advance(EXPIRE_AFTER + Duration::from_secs(1)).await;
        let live = &status.snapshot().observations[0];
        assert_eq!(live.phase, SnapshotTransportPhase::Installing);
        assert_eq!(live.active_deadline_remaining_ms, Some(3_299_000));

        tokio::time::advance(Duration::from_secs(3_299)).await;
        assert_eq!(
            status.snapshot().observations[0].phase,
            SnapshotTransportPhase::Stalled
        );
        tokio::time::advance(EXPIRE_AFTER + Duration::from_millis(1)).await;
        assert!(status.snapshot().observations.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn configured_chunk_deadline_controls_awaiting_ack_stall_projection() {
        let status = LocalSnapshotTransportStatus::new(1, BTreeSet::from([2]));
        let minimum = Instant::now() + Duration::from_secs(5);
        status.begin_outbound_attempt(
            "sqlite",
            2,
            1,
            "minimum",
            OutboundSnapshotSocket {
                epoch: 1,
                connected: true,
            },
            minimum,
        );
        status.outbound_chunk("sqlite", 2, 0, 64, false, minimum);
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(
            status.snapshot().observations[0].phase,
            SnapshotTransportPhase::Stalled
        );

        let maximum = Instant::now() + Duration::from_secs(300);
        status.begin_outbound_attempt(
            "sqlite",
            2,
            2,
            "maximum",
            OutboundSnapshotSocket {
                epoch: 2,
                connected: true,
            },
            maximum,
        );
        status.outbound_chunk("sqlite", 2, 0, 64, false, maximum);
        tokio::time::advance(STALLED_AFTER).await;
        assert_eq!(
            status.snapshot().observations[0].phase,
            SnapshotTransportPhase::AwaitingAcknowledgement
        );
        tokio::time::advance(Duration::from_secs(270)).await;
        assert_eq!(
            status.snapshot().observations[0].phase,
            SnapshotTransportPhase::Stalled
        );
    }

    #[tokio::test(start_paused = true)]
    async fn completed_observation_never_projects_as_stalled_and_eventually_expires() {
        let status = LocalSnapshotTransportStatus::new(1, BTreeSet::from([2]));
        let deadline = Instant::now() + Duration::from_secs(120);
        status.begin_outbound_attempt(
            "sqlite",
            2,
            1,
            "snap-a",
            OutboundSnapshotSocket {
                epoch: 1,
                connected: true,
            },
            deadline,
        );
        status.outbound_acknowledged("sqlite", 2, 64, true, Some(deadline));

        tokio::time::advance(STALLED_AFTER + Duration::from_secs(1)).await;
        assert_eq!(
            status.snapshot().observations[0].phase,
            SnapshotTransportPhase::Complete
        );
        tokio::time::advance(EXPIRE_AFTER - STALLED_AFTER).await;
        assert!(status.snapshot().observations.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn valid_install_does_not_stall_at_the_chunk_window() {
        let status = LocalSnapshotTransportStatus::new(1, BTreeSet::from([2]));
        let deadline = Instant::now() + Duration::from_secs(120);
        status.begin_outbound_attempt(
            "sqlite",
            2,
            1,
            "snap-a",
            OutboundSnapshotSocket {
                epoch: 1,
                connected: true,
            },
            deadline,
        );
        status.outbound_chunk("sqlite", 2, 0, 64, true, deadline);

        tokio::time::advance(STALLED_AFTER + Duration::from_secs(1)).await;
        assert_eq!(
            status.snapshot().observations[0].phase,
            SnapshotTransportPhase::Installing
        );
        tokio::time::advance(Duration::from_secs(89)).await;
        assert_eq!(
            status.snapshot().observations[0].phase,
            SnapshotTransportPhase::Stalled
        );
    }

    #[tokio::test(start_paused = true)]
    async fn retired_peers_are_bounded() {
        let status = LocalSnapshotTransportStatus::new(1, BTreeSet::from([2]));
        for peer in 3..40 {
            status.begin_outbound_attempt(
                "sqlite",
                peer,
                peer,
                "retired",
                OutboundSnapshotSocket {
                    epoch: 1,
                    connected: true,
                },
                Instant::now() + Duration::from_secs(30),
            );
        }
        assert!(status.snapshot().observations.len() <= status.current_capacity());

        let saturated = LocalSnapshotTransportStatus::new(1, BTreeSet::new());
        for peer in 2..100 {
            saturated.begin_outbound_attempt(
                "sqlite",
                peer,
                peer,
                "retired",
                OutboundSnapshotSocket {
                    epoch: 1,
                    connected: true,
                },
                Instant::now() + Duration::from_secs(30),
            );
        }
        assert_eq!(
            saturated.snapshot().observations.len(),
            saturated.current_capacity()
        );
    }
}
