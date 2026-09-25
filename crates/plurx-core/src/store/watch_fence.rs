//! The per-process read-your-write fence for watch state (K-04 M2).
//!
//! Catalogue facts may be served from the local replica under the bounded
//! proof alone, because a catalogue read that is a few entries behind shows
//! a library as it was a moment ago. Watch state is different: the Home rails
//! read what the same person has just written, and a replica one entry
//! behind shows the episode they finished a second ago as unwatched. So a
//! local watch read needs a second, per-client condition on top of the
//! bounded permit: this node must have applied the Raft entry that carried
//! the latest watch write the *client* was told about.
//!
//! Only the client can name that entry. It echoes, as `X-Plurx-Read-After`,
//! the newest `X-Plurx-Commit-Index` an earlier response gave it, whichever
//! node answered that write. Without the header the read goes to Authority,
//! even when this process holds a write record for the user: a node cannot
//! tell whether the user has written through a peer since its own record,
//! any more than it can tell a user who wrote nothing from one who wrote
//! through a peer a second ago. (A TV writes progress through node B, the
//! phone marks the next episode watched through node A at a later index, and
//! the TV's next Home on B would otherwise be proved fresh by B's older
//! record.) So the local record never stands in for the client's floor; it
//! can only raise one:
//!
//! - the log index of every watch mutation this process committed for the
//!   user, taken from the write response ([`hiqlite::WriteAck`]) and kept
//!   here for [`WATCH_WRITE_FENCE_TTL`], lifts a client's floor to this
//!   node's latest write for the user when the client's echo is older;
//! - a write whose index could not be learned (an older leader, or a proxy,
//!   answered it; or it failed and may still commit) marks the user
//!   unprovable for the same window, which means Authority even with a
//!   header.
//!
//! The header gives **per-client session consistency, not per-user
//! consistency**: a second device's write through another node is covered by
//! neither the header (that client never saw its index) nor the local record
//! (this node never saw the write). Nothing here reads wall-clock time: the
//! window is monotonic and the fence itself is an index comparison.
//!
//! The map is bounded. If more users write within one window than it can
//! hold, it does not evict a live fence — forgetting one would let a local
//! read skip a write the fence exists to see — but closes local watch reads
//! for every user until one full window has passed.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long one acknowledged watch write keeps its fence. Matches the
/// window the web client echoes `X-Plurx-Commit-Index` for; a longer window
/// only costs Authority reads.
pub(crate) const WATCH_WRITE_FENCE_TTL: Duration = Duration::from_secs(60);

/// Distinct users one process fences at once before it closes local watch
/// reads for a window instead of forgetting a live fence.
pub(crate) const WATCH_WRITE_FENCE_MAX_USERS: usize = 4_096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fence {
    Index(u64),
    Unprovable,
}

impl Fence {
    fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Index(left), Self::Index(right)) => Self::Index(left.max(right)),
            _ => Self::Unprovable,
        }
    }
}

#[derive(Default)]
struct FenceState {
    entries: HashMap<i64, (Fence, Instant)>,
    saturated_until: Option<Instant>,
}

#[derive(Default)]
pub(crate) struct WatchWriteFences {
    state: Mutex<FenceState>,
}

impl WatchWriteFences {
    /// Record one acknowledged watch mutation for `user_id`. `None` means the
    /// write committed but its log position is unknown here.
    pub(crate) fn record(&self, user_id: i64, log_index: Option<u64>) {
        self.record_at(user_id, log_index, Instant::now());
    }

    /// The applied index a local watch read for `user_id` must reach, or
    /// `None` when no local read may be served and Authority must answer:
    /// always `None` without the client's `read_after`, which this process's
    /// own write record may raise but never replace.
    pub(crate) fn required_applied_index(
        &self,
        user_id: i64,
        read_after: Option<u64>,
    ) -> Option<u64> {
        self.required_at(user_id, read_after, Instant::now())
    }

    fn record_at(&self, user_id: i64, log_index: Option<u64>, now: Instant) {
        let fence = log_index.map_or(Fence::Unprovable, Fence::Index);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let merged = match state.entries.get(&user_id) {
            Some((existing, expires_at)) if *expires_at > now => existing.merge(fence),
            _ => fence,
        };
        if !state.entries.contains_key(&user_id)
            && state.entries.len() >= WATCH_WRITE_FENCE_MAX_USERS
        {
            state.entries.retain(|_, (_, expires_at)| *expires_at > now);
            if state.entries.len() >= WATCH_WRITE_FENCE_MAX_USERS {
                // Every held fence is live, and dropping any of them would
                // let its user's next local read skip that write. Close local
                // watch reads for everyone for one window instead; the new
                // write is still covered, because nothing is served locally.
                state.saturated_until = Some(now + WATCH_WRITE_FENCE_TTL);
                return;
            }
        }
        state
            .entries
            .insert(user_id, (merged, now + WATCH_WRITE_FENCE_TTL));
    }

    fn required_at(&self, user_id: i64, read_after: Option<u64>, now: Instant) -> Option<u64> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.saturated_until.is_some_and(|until| until > now) {
            return None;
        }
        let local = state
            .entries
            .get(&user_id)
            .filter(|(_, expires_at)| *expires_at > now)
            .map(|(fence, _)| *fence);
        // The client's floor is required; a local record only raises it.
        let read_after = read_after?;
        match local {
            Some(Fence::Unprovable) => None,
            Some(Fence::Index(index)) => Some(index.max(read_after)),
            None => Some(read_after),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_with_no_write_record_and_no_header_is_answered_by_authority() {
        let fences = WatchWriteFences::default();
        let now = Instant::now();
        assert_eq!(fences.required_at(7, None, now), None);
        assert_eq!(fences.required_at(7, Some(40), now), Some(40));
    }

    #[test]
    fn the_larger_of_the_local_write_and_the_echoed_header_is_required() {
        let fences = WatchWriteFences::default();
        let now = Instant::now();
        fences.record_at(7, Some(50), now);
        assert_eq!(fences.required_at(7, Some(40), now), Some(50));
        assert_eq!(fences.required_at(7, Some(60), now), Some(60));
        // Another user's write raises nothing for this one.
        assert_eq!(fences.required_at(8, Some(40), now), Some(40));
        // A later write raises the fence; an earlier one never lowers it.
        fences.record_at(7, Some(70), now);
        fences.record_at(7, Some(55), now);
        assert_eq!(fences.required_at(7, Some(40), now), Some(70));
    }

    /// Review of #504, finding 1: this node's own older write record is no
    /// proof of the user's latest write, which may have gone through a peer.
    /// Without the client's floor the read is Authority's.
    #[test]
    fn a_local_write_record_never_stands_in_for_the_clients_floor() {
        let fences = WatchWriteFences::default();
        let now = Instant::now();
        // Node B acknowledged the user's progress at 100; a later write at
        // 140 went through node A, which B knows nothing of.
        fences.record_at(7, Some(100), now);
        assert_eq!(fences.required_at(7, None, now), None);
        assert_eq!(
            fences.required_at(7, None, now + Duration::from_secs(10)),
            None
        );
        // The client that saw 140 names it, and B must reach it.
        assert_eq!(fences.required_at(7, Some(140), now), Some(140));
    }

    #[test]
    fn an_unprovable_write_closes_local_reads_for_its_window_even_with_a_header() {
        let fences = WatchWriteFences::default();
        let now = Instant::now();
        fences.record_at(7, Some(50), now);
        fences.record_at(7, None, now);
        assert_eq!(fences.required_at(7, Some(1_000), now), None);
        // A provable write inside the same window cannot reopen it.
        fences.record_at(7, Some(90), now + Duration::from_secs(1));
        assert_eq!(
            fences.required_at(7, Some(1_000), now + Duration::from_secs(1)),
            None
        );
    }

    #[test]
    fn a_fence_lasts_exactly_one_window_from_its_latest_write() {
        let fences = WatchWriteFences::default();
        let now = Instant::now();
        fences.record_at(7, Some(50), now);
        let just_inside = now + WATCH_WRITE_FENCE_TTL - Duration::from_millis(1);
        assert_eq!(fences.required_at(7, Some(1), just_inside), Some(50));
        assert_eq!(
            fences.required_at(7, Some(1), now + WATCH_WRITE_FENCE_TTL),
            Some(1)
        );
        // After expiry an unprovable record no longer poisons a new write.
        fences.record_at(8, None, now);
        let later = now + WATCH_WRITE_FENCE_TTL;
        fences.record_at(8, Some(80), later);
        assert_eq!(fences.required_at(8, Some(1), later), Some(80));
    }

    #[test]
    fn a_full_map_closes_local_reads_instead_of_forgetting_a_live_fence() {
        let fences = WatchWriteFences::default();
        let now = Instant::now();
        for user in 0..WATCH_WRITE_FENCE_MAX_USERS as i64 {
            fences.record_at(user, Some(10), now);
        }
        assert_eq!(fences.required_at(0, Some(1), now), Some(10));
        let overflow = WATCH_WRITE_FENCE_MAX_USERS as i64;
        fences.record_at(overflow, Some(20), now);
        assert_eq!(fences.required_at(0, Some(1), now), None);
        assert_eq!(fences.required_at(overflow, Some(20), now), None);
        // Once every held fence and the closure have expired, the map
        // accepts writes and local reads reopen.
        let later = now + WATCH_WRITE_FENCE_TTL;
        fences.record_at(overflow, Some(30), later);
        assert_eq!(fences.required_at(overflow, Some(1), later), Some(30));
    }
}
