//! Bounded, process-local snapshot transport observations.
//!
//! This state is owned by one embedded node. It is intentionally independent
//! of SQL, Raft logs, and Prometheus labels: recording a chunk must never
//! create replicated work or retain an unbounded snapshot/request identity.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::Instant;

const RETIRED_PEER_ALLOWANCE: usize = 4;
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
    pub(crate) deadline: Instant,
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
}

struct State {
    next_inbound_attempt: u64,
    observations: BTreeMap<ObservationKey, Observation>,
}

struct Inner {
    observing_node_id: u64,
    boot_id: String,
    configured_peers: BTreeSet<u64>,
    capacity: usize,
    state: Mutex<State>,
}

/// Cloneable handle for one node's in-memory snapshot transport state.
#[derive(Clone)]
pub struct LocalSnapshotTransportStatus {
    inner: Arc<Inner>,
}

impl LocalSnapshotTransportStatus {
    pub(crate) fn new(observing_node_id: u64, configured_peers: BTreeSet<u64>) -> Self {
        let capacity = configured_peers
            .len()
            .saturating_mul(2)
            .saturating_add(RETIRED_PEER_ALLOWANCE * 2)
            .max(2);
        let boot_nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map_or(0, |duration| duration.as_nanos());
        Self {
            inner: Arc::new(Inner {
                observing_node_id,
                boot_id: format!("{observing_node_id}-{boot_nanos}"),
                configured_peers,
                capacity,
                state: Mutex::new(State {
                    next_inbound_attempt: 0,
                    observations: BTreeMap::new(),
                }),
            }),
        }
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
        state.observations.retain(|key, observation| {
            self.inner.configured_peers.contains(&key.peer_node_id)
                || now.saturating_duration_since(observation.last_update) <= EXPIRE_AFTER
        });
        let observations = state
            .observations
            .iter_mut()
            .filter_map(|(key, observation)| {
                let sample_age = now.saturating_duration_since(observation.last_update);
                if sample_age > EXPIRE_AFTER {
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

    pub(crate) fn connecting(
        &self,
        raft_group: &'static str,
        peer_node_id: u64,
        socket_epoch: u64,
    ) {
        self.update_existing(
            raft_group,
            peer_node_id,
            SnapshotTransportDirection::Outbound,
            |observation, now| {
                if !observation.operation_owns_work {
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

    pub(crate) fn begin_outbound_attempt(
        &self,
        raft_group: &'static str,
        peer_node_id: u64,
        attempt_id: u64,
        snapshot_id: &str,
        socket_epoch: u64,
        deadline: Instant,
    ) {
        self.update(
            raft_group,
            peer_node_id,
            SnapshotTransportDirection::Outbound,
            |observation, now| {
                observation.attempt_id = attempt_id;
                observation.snapshot_id = Some(snapshot_id.to_owned());
                observation.socket_epoch = socket_epoch;
                observation.attempted_offset = None;
                observation.acknowledged_offset = None;
                observation.locally_received_bytes = None;
                observation.total_bytes = None;
                observation.attempt_started = Some(now);
                observation.last_acknowledgement = None;
                observation.last_local_receive = None;
                observation.deadline = Some(deadline);
                observation.phase = SnapshotTransportPhase::Transferring;
                observation.connection_attempt_count = 0;
                observation.retry_count = 0;
                observation.last_error_category = None;
                observation.operation_owns_work = true;
                observation.last_update = now;
            },
        );
    }

    pub(crate) fn outbound_chunk(
        &self,
        raft_group: &'static str,
        peer_node_id: u64,
        offset: u64,
        len: usize,
        done: bool,
        deadline: Instant,
    ) {
        self.update(
            raft_group,
            peer_node_id,
            SnapshotTransportDirection::Outbound,
            |observation, now| {
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
            },
        );
    }

    pub(crate) fn outbound_acknowledged(
        &self,
        raft_group: &'static str,
        peer_node_id: u64,
        acknowledged_offset: u64,
        done: bool,
        transfer_deadline: Option<Instant>,
    ) {
        self.update(
            raft_group,
            peer_node_id,
            SnapshotTransportDirection::Outbound,
            |observation, now| {
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
            },
        );
    }

    pub(crate) fn outbound_retry(
        &self,
        raft_group: &'static str,
        peer_node_id: u64,
        category: &'static str,
        deadline: Option<Instant>,
    ) {
        self.update(
            raft_group,
            peer_node_id,
            SnapshotTransportDirection::Outbound,
            |observation, now| {
                observation.retry_count = observation.retry_count.saturating_add(1);
                observation.phase = SnapshotTransportPhase::Retrying;
                observation.deadline = deadline;
                observation.last_error_category = Some(category);
                observation.operation_owns_work = true;
                observation.last_update = now;
            },
        );
    }

    pub(crate) fn outbound_failed(
        &self,
        raft_group: &'static str,
        peer_node_id: u64,
        category: &'static str,
    ) {
        self.update(
            raft_group,
            peer_node_id,
            SnapshotTransportDirection::Outbound,
            |observation, now| {
                observation.phase = SnapshotTransportPhase::Failed;
                observation.deadline = None;
                if observation.last_error_category.is_none() {
                    observation.last_error_category = Some(category);
                }
                observation.operation_owns_work = false;
                observation.last_update = now;
            },
        );
    }

    pub(crate) fn inbound_received(&self, chunk: InboundSnapshotChunk<'_>) {
        let InboundSnapshotChunk {
            raft_group,
            peer_node_id,
            snapshot_id,
            offset,
            len,
            done,
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
            return;
        }
        let changed_snapshot = state
            .observations
            .get(&key)
            .and_then(|observation| observation.snapshot_id.as_deref())
            != Some(snapshot_id);
        if changed_snapshot {
            state.next_inbound_attempt = state.next_inbound_attempt.saturating_add(1);
        }
        let attempt_id = state.next_inbound_attempt;
        let observation = state
            .observations
            .entry(key.clone())
            .or_insert_with(|| empty_observation(now));
        let previous = observation.phase;
        if changed_snapshot {
            *observation = empty_observation(now);
            observation.attempt_id = attempt_id;
            observation.snapshot_id = Some(snapshot_id.to_owned());
            observation.attempt_started = Some(now);
        }
        let end_offset = offset.saturating_add(len as u64);
        let previous_end = observation.attempted_offset.unwrap_or_default();
        if !changed_snapshot && end_offset <= previous_end {
            observation.retry_count = observation.retry_count.saturating_add(1);
        }
        observation.attempted_offset = Some(previous_end.max(end_offset));
        if done {
            observation.total_bytes = Some(previous_end.max(end_offset));
        }
        observation.deadline = Some(deadline);
        observation.phase = if done {
            SnapshotTransportPhase::Installing
        } else {
            SnapshotTransportPhase::Transferring
        };
        observation.operation_owns_work = false;
        observation.last_error_category = None;
        observation.last_update = now;
        log_transition(self.inner.observing_node_id, &key, previous, observation);
    }

    pub(crate) fn inbound_admitted(
        &self,
        raft_group: &'static str,
        peer_node_id: u64,
        done: bool,
        deadline: Instant,
    ) {
        self.update(
            raft_group,
            peer_node_id,
            SnapshotTransportDirection::Inbound,
            |observation, now| {
                observation.deadline = Some(deadline);
                observation.phase = if done {
                    SnapshotTransportPhase::Installing
                } else {
                    SnapshotTransportPhase::Transferring
                };
                observation.operation_owns_work = true;
                observation.last_update = now;
            },
        );
    }

    pub(crate) fn inbound_finished(
        &self,
        raft_group: &'static str,
        peer_node_id: u64,
        locally_received_offset: u64,
        done: bool,
        error_category: Option<&'static str>,
    ) {
        self.update(
            raft_group,
            peer_node_id,
            SnapshotTransportDirection::Inbound,
            |observation, now| {
                observation.operation_owns_work = false;
                observation.last_update = now;
                if let Some(category) = error_category {
                    observation.phase = SnapshotTransportPhase::Retrying;
                    observation.deadline = None;
                    observation.last_error_category = Some(category);
                    observation.retry_count = observation.retry_count.saturating_add(1);
                } else {
                    // Finishing the node-owned install only means bytes are
                    // durable locally. The response still has to cross and flush
                    // the WebSocket before the sender can call it acknowledged.
                    observation.deadline = None;
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
            },
        );
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

    fn make_room(&self, state: &mut State, now: Instant, incoming: &ObservationKey) -> bool {
        state.observations.retain(|key, observation| {
            self.inner.configured_peers.contains(&key.peer_node_id)
                || now.saturating_duration_since(observation.last_update) <= EXPIRE_AFTER
        });
        if state.observations.contains_key(incoming)
            || state.observations.len() < self.inner.capacity
        {
            return true;
        }
        let oldest_retired = state
            .observations
            .iter()
            .filter(|(key, observation)| {
                !self.inner.configured_peers.contains(&key.peer_node_id)
                    && !observation.operation_owns_work
            })
            .min_by_key(|(_, observation)| observation.last_update)
            .map(|(key, _)| key.clone());
        if let Some(key) = oldest_retired {
            state.observations.remove(&key);
            true
        } else {
            false
        }
    }
}

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
    }
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

    #[tokio::test(start_paused = true)]
    async fn local_status_distinguishes_receive_ack_install_retry_and_completion() {
        let status = LocalSnapshotTransportStatus::new(1, BTreeSet::from([2]));
        let deadline = Instant::now() + Duration::from_secs(30);
        // Ordinary connection churn is not snapshot work and must not create
        // a status record that later projects as a stalled snapshot.
        status.connecting("sqlite", 2, 0);
        assert!(status.snapshot().observations.is_empty());
        status.begin_outbound_attempt("sqlite", 2, 7, "snap-a", 0, deadline);
        status.connecting("sqlite", 2, 1);
        assert_eq!(status.snapshot().observations[0].reconnect_count, 0);
        status.connecting("sqlite", 2, 2);
        assert_eq!(status.snapshot().observations[0].reconnect_count, 1);
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
            deadline: Instant::now() + Duration::from_secs(30),
        });
        let received = &status.snapshot().observations[0];
        assert_eq!(received.locally_received_bytes, None);
        assert_eq!(received.acknowledged_offset, None);
        assert!(!received.operation_owns_work);

        status.inbound_received(InboundSnapshotChunk {
            raft_group: "sqlite",
            peer_node_id: 1,
            snapshot_id: "snap-a",
            offset: 64,
            len: 32,
            done: true,
            deadline: Instant::now() + Duration::from_secs(120),
        });
        assert_eq!(status.snapshot().observations[0].total_bytes, Some(96));

        status.inbound_admitted("sqlite", 1, true, Instant::now() + Duration::from_secs(120));
        assert!(status.snapshot().observations[0].operation_owns_work);

        status.inbound_finished("sqlite", 1, 96, true, None);
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
        for (offset, len) in [(0, 64), (64, 32), (32, 16)] {
            status.inbound_received(InboundSnapshotChunk {
                raft_group: "sqlite",
                peer_node_id: 1,
                snapshot_id: "snap-a",
                offset,
                len,
                done: false,
                deadline,
            });
        }
        let first = &status.snapshot().observations[0];
        assert_eq!(first.attempted_offset, Some(96));
        assert_eq!(first.retry_count, 1);

        status.inbound_finished("sqlite", 1, 96, false, None);
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
        status.begin_outbound_attempt("sqlite", 2, 7, "outbound", 1, deadline);
        status.inbound_received(InboundSnapshotChunk {
            raft_group: "sqlite",
            peer_node_id: 2,
            snapshot_id: "inbound",
            offset: 0,
            len: 64,
            done: true,
            deadline,
        });
        status.inbound_admitted("sqlite", 2, true, deadline);

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
        status.begin_outbound_attempt("sqlite", 2, 7, "snapshot", 1, deadline);
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
    async fn active_observation_stalls_then_expires_without_renewing_sample_age() {
        let status = LocalSnapshotTransportStatus::new(1, BTreeSet::from([2]));
        let deadline = Instant::now() + Duration::from_secs(120);
        status.begin_outbound_attempt("sqlite", 2, 7, "snap-a", 3, deadline);

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

        tokio::time::advance(EXPIRE_AFTER - Duration::from_secs(120) + Duration::from_millis(1))
            .await;
        assert!(status.snapshot().observations.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn configured_chunk_deadline_controls_awaiting_ack_stall_projection() {
        let status = LocalSnapshotTransportStatus::new(1, BTreeSet::from([2]));
        let minimum = Instant::now() + Duration::from_secs(5);
        status.begin_outbound_attempt("sqlite", 2, 1, "minimum", 1, minimum);
        status.outbound_chunk("sqlite", 2, 0, 64, false, minimum);
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(
            status.snapshot().observations[0].phase,
            SnapshotTransportPhase::Stalled
        );

        let maximum = Instant::now() + Duration::from_secs(300);
        status.begin_outbound_attempt("sqlite", 2, 2, "maximum", 2, maximum);
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
        status.begin_outbound_attempt("sqlite", 2, 1, "snap-a", 1, deadline);
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
        status.begin_outbound_attempt("sqlite", 2, 1, "snap-a", 1, deadline);
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
            status.connecting("sqlite", peer, 1);
        }
        assert!(status.snapshot().observations.len() <= status.inner.capacity);

        let saturated = LocalSnapshotTransportStatus::new(1, BTreeSet::new());
        for peer in 2..100 {
            saturated.connecting("sqlite", peer, 1);
            saturated.inbound_admitted(
                "sqlite",
                peer,
                false,
                Instant::now() + Duration::from_secs(30),
            );
        }
        assert_eq!(
            saturated.snapshot().observations.len(),
            saturated.inner.capacity
        );
    }
}
