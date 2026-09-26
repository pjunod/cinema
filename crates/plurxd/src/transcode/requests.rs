use super::*;

/// One creation request's lifecycle, keyed by the client's `request_id`.
pub(super) enum RequestState {
    /// A create with this id is running and its outcome isn't known yet.
    /// A concurrent create with the same id waits for it rather than pass
    /// the same check and start a second encoder.
    InFlight,
    /// The create finished, and this is the session it made.
    Ready(String),
}

/// One idempotency-key record. `target_height` is written while the request is
/// still claimed and before session creation can supersede its predecessor.
/// Keeping the normalized answer in this existing map is the N4 contract; a
/// second retry store would let the two lifecycles disagree.
pub(super) struct RequestEntry {
    pub(super) intent_fingerprint: String,
    pub(super) target_height: Option<i64>,
    pub(super) state: RequestState,
}

/// How long a create will wait on an identical in-flight one before calling
/// it lost. An honest peer resolves within the slot queue's patience plus
/// spawn overhead; a reservation still standing past this belongs to a
/// process that died without unwinding, and erroring beats hanging the
/// player behind it.
pub(super) const INFLIGHT_WAIT: Duration = Duration::from_secs(QUEUE_WAIT.as_secs() + 10);
pub(super) const INFLIGHT_POLL: Duration = Duration::from_millis(100);

/// The reservation a creating call holds while it works.
///
/// Exists so a create that never completes — an error, or a caller that
/// vanished mid-await (a closed tab drops its request future wherever it
/// happens to be) — cannot leave its `request_id` parked `InFlight` forever,
/// wedging every honest retry behind [`INFLIGHT_WAIT`]. `complete` records
/// the session and defuses the guard; `Drop` covers every other exit by
/// clearing the reservation so the next attempt starts fresh.
pub(super) struct RequestClaim<'a> {
    pub(super) requests: &'a std::sync::Mutex<HashMap<String, RequestEntry>>,
    pub(super) key: Option<String>,
}

impl RequestClaim<'_> {
    /// Record the created session under this reservation's key, pruning
    /// entries whose sessions have ended so the map cannot grow for the life
    /// of the process. In-flight reservations are never pruned — their
    /// sessions aren't in the map *yet*.
    pub(super) fn complete(mut self, session_id: &str, live: &std::collections::HashSet<String>) {
        let Some(key) = self.key.take() else { return };
        let mut requests = self.requests.lock().expect("requests mutex");
        requests.retain(|_, entry| match &entry.state {
            RequestState::InFlight => true,
            RequestState::Ready(sid) => live.contains(sid),
        });
        if let Some(entry) = requests.get_mut(&key) {
            entry.state = RequestState::Ready(session_id.to_owned());
        } else {
            // Unreachable as written: our own entry is `InFlight`, so the
            // retain above keeps it. If it ever is missing, the create
            // persists no `Ready` record and the next replay of this
            // `request_id` spawns a duplicate encoder — too quiet a failure
            // to leave unsaid.
            debug_assert!(false, "completed claim lost its own reservation");
            tracing::warn!(
                target: "plurxd::transcode",
                session = %session_log_id(session_id),
                "request claim vanished before completion; a replay may duplicate this session"
            );
        }
    }
}

impl Drop for RequestClaim<'_> {
    fn drop(&mut self) {
        let Some(key) = self.key.take() else { return };
        if let Ok(mut requests) = self.requests.lock() {
            // Only this claim's own reservation. Nothing else removes an
            // `InFlight` entry, so finding one under our key means it is
            // ours; a `Ready` entry is a completed create and belongs to
            // the map.
            if matches!(
                requests.get(&key),
                Some(RequestEntry {
                    state: RequestState::InFlight,
                    ..
                })
            ) {
                requests.remove(&key);
            }
        }
    }
}

/// What claiming a `request_id` resolved to.
pub(super) enum Claimed<'a> {
    /// This call owns the create; the claim must be completed or dropped.
    Mine(RequestClaim<'a>, SessionRequest),
    /// An identical create already made this session.
    Recovered(StartInfo),
}
