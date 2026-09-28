use super::*;

/// Media requests parked in [`TranscodeManager::segment_for_publication_before`]
/// waiting for the producer to publish what they asked for — segments, and
/// the init object a playlist request resolves through the same loop (those
/// carry no segment index).
///
/// The rolling twin of the VOD wait pool's reading. The rolling status used to
/// report a constant zero here, on the claim that rolling delivery never parks
/// a response — but the publication loop does park, for up to [`SEGMENT_WAIT`],
/// and that wait is exactly what a client's "Buffering…" detail needs in order
/// to tell a starved producer from a player sitting on media it already has.
#[derive(Default)]
pub(super) struct HttpWaitLedger {
    pub(super) entries: std::sync::Mutex<HttpWaitEntries>,
}

#[derive(Default)]
pub(super) struct HttpWaitEntries {
    pub(super) next_id: u64,
    pub(super) open: HashMap<u64, (Instant, Option<i64>)>,
}

/// One status sample of a [`HttpWaitLedger`]: how many requests are parked,
/// how long the oldest has been, and which segment it asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct HttpWaitSnapshot {
    pub(super) count: usize,
    pub(super) oldest_ms: Option<i64>,
    pub(super) oldest_segment: Option<i64>,
}

impl HttpWaitLedger {
    pub(super) fn enter(&self, segment: Option<i64>) -> u64 {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = entries.next_id;
        entries.next_id = entries.next_id.wrapping_add(1);
        entries.open.insert(id, (Instant::now(), segment));
        id
    }

    pub(super) fn leave(&self, id: u64) {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .open
            .remove(&id);
    }

    pub(super) fn snapshot(&self) -> HttpWaitSnapshot {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let oldest = entries.open.values().min_by_key(|(since, _)| *since);
        HttpWaitSnapshot {
            count: entries.open.len(),
            oldest_ms: oldest
                .map(|(since, _)| since.elapsed().as_millis().min(i64::MAX as u128) as i64),
            oldest_segment: oldest.and_then(|(_, segment)| *segment),
        }
    }
}

/// Holds one entry in a session's [`HttpWaitLedger`] for as long as a segment
/// request is parked. Dropping it is the only way the entry leaves — on a
/// served segment, on every early return, and when the client disconnects and
/// the request future is dropped mid-sleep — so the count cannot leak.
pub(super) struct HttpWaitGuard {
    pub(super) session: Arc<Session>,
    pub(super) id: u64,
}

impl HttpWaitGuard {
    pub(super) fn enter(session: &Arc<Session>, segment: Option<i64>) -> Self {
        Self {
            id: session.http_waits.enter(segment),
            session: Arc::clone(session),
        }
    }
}

impl Drop for HttpWaitGuard {
    fn drop(&mut self) {
        self.session.http_waits.leave(self.id);
    }
}

pub(super) fn storage_read_is_slow(bytes: u64, elapsed: Duration) -> bool {
    elapsed >= SEGMENT_WAIT_EVENT_MIN
        && bytes as f64 / elapsed.as_secs_f64() < SEGMENT_STALL_BYTES_PER_SECOND
}

pub(super) async fn wait_for_playlist_poll_before(deadline: Instant) {
    let now = tokio::time::Instant::now().into_std();
    if now >= deadline {
        return;
    }
    tokio::time::sleep_until(tokio::time::Instant::from_std(
        (now + PLAYLIST_WAIT_POLL).min(deadline),
    ))
    .await;
}
