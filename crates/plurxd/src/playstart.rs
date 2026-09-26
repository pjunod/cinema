//! Two small caches that keep the click-to-first-frame path off the NAS and
//! out of side effects.
//!
//! Both exist for the same reason: `/decision` is asked one question — how
//! should this file be delivered — and it was answering it by doing remote
//! I/O and announcing to a third party that someone had started watching.
//! Neither belongs on a path a person is waiting behind, and the second is not
//! even true at the time it fires: a decision is not playback, and a stream
//! that fails to start had still told Trakt it was running.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::delivery::{Key, Method};

/// How long a successful stat is trusted.
///
/// Short on purpose. This is not a claim that the file still exists — only
/// that it did a moment ago, which is all `/decision` ever knew either. The
/// authoritative check is the open that follows, and it is the one that
/// reports a share that went away mid-film.
const AVAILABILITY_TTL: Duration = Duration::from_secs(60);

/// Remembers which media files were readable, so repeated plays of the same
/// title don't each pay a cold NAS attribute lookup between the click and the
/// answer.
#[derive(Default)]
pub struct AvailabilityCache {
    seen: Mutex<HashMap<i64, Instant>>,
}

impl AvailabilityCache {
    pub fn new() -> AvailabilityCache {
        AvailabilityCache::default()
    }

    /// Read the recent-success fact without touching the source path. Peer
    /// offer fan-out must never be what wakes a sleeping NAS.
    pub fn recently_present(&self, file_id: i64) -> bool {
        self.seen.lock().is_ok_and(|seen| {
            seen.get(&file_id)
                .is_some_and(|at| at.elapsed() < AVAILABILITY_TTL)
        })
    }

    /// True when the file is on disk. Cached briefly on success and never on
    /// failure — a missing file is the answer that changes a player into an
    /// error message, so it is worth re-asking every time.
    pub async fn is_present(&self, file_id: i64, path: &Path) -> bool {
        if let Ok(seen) = self.seen.lock() {
            if seen
                .get(&file_id)
                .is_some_and(|at| at.elapsed() < AVAILABILITY_TTL)
            {
                return true;
            }
        }
        if tokio::fs::metadata(path).await.is_err() {
            self.forget(file_id);
            return false;
        }
        if let Ok(mut seen) = self.seen.lock() {
            // Bound the map: entries are tiny, but a library-wide sweep should
            // not leave one per file forever.
            if seen.len() > 4096 {
                seen.retain(|_, at| at.elapsed() < AVAILABILITY_TTL);
            }
            seen.insert(file_id, Instant::now());
        }
        true
    }

    /// Drop a file's cached presence — called when an open actually fails, so
    /// an unmounted share stops being remembered as fine.
    pub fn forget(&self, file_id: i64) {
        if let Ok(mut seen) = self.seen.lock() {
            seen.remove(&file_id);
        }
    }
}

/// How long one playback of one item counts as already-announced.
///
/// Long enough that the several media requests a single play makes (a decision
/// followed by a session, or a range request followed by a seek) announce once
/// between them; short enough that genuinely re-watching something announces
/// again.
const START_DEDUP: Duration = Duration::from_secs(300);

/// Announces "watching now" to Trakt when playback actually begins.
///
/// This used to fire from `/decision`, which is wrong twice over: a decision
/// is not playback — a stream that never started had still reported itself
/// running — and it put a third-party call on the click path. It now fires
/// from the points where media is really being delivered, in a detached task,
/// so no viewer ever waits on Trakt being slow.
#[derive(Default)]
pub struct StartNotifier {
    announced: Mutex<HashMap<(i64, i64), Instant>>,
}

impl StartNotifier {
    pub fn new() -> StartNotifier {
        StartNotifier::default()
    }

    /// Claim the right to announce this (viewer, item) pair. False when
    /// another request already did, recently.
    fn claim(&self, user_id: i64, item_id: i64) -> bool {
        let Ok(mut announced) = self.announced.lock() else {
            return false; // a poisoned lock must not double-announce
        };
        let key = (user_id, item_id);
        if announced
            .get(&key)
            .is_some_and(|at| at.elapsed() < START_DEDUP)
        {
            return false;
        }
        if announced.len() > 1024 {
            announced.retain(|_, at| at.elapsed() < START_DEDUP);
        }
        announced.insert(key, Instant::now());
        true
    }
}

/// How long a start attempt may go without a first frame, a failure report
/// or a refusal before it counts as `cancelled`: above the top
/// `plurx_ttff_ms` bucket (120 s), so a start slow enough to land in that
/// bucket still resolves as `ok` rather than being given up on first.
pub(crate) const START_DEADLINE: Duration = Duration::from_secs(180);

/// How long a play that reached its first frame stays one play with no
/// sign of the viewer. Every later request for the same file, every client
/// beacon about it and every progress beat for its item refresh it; a player
/// open on that title beats every 5-10 s, paused or not, so this is only
/// reached once the player is gone. The same window [`START_DEDUP`] gives a
/// Trakt announcement, for the same reason.
pub(crate) const PLAY_QUIET: Duration = START_DEDUP;

/// Bound on tracked (viewer, file) pairs. A viewer arriving at a full ledger
/// is not tracked, rather than evicting an attempt that is still in flight:
/// an evicted attempt could only be miscounted.
const START_LEDGER_CAP: usize = 4_096;

/// The client events that report a start that never showed a picture, as
/// the three first-party players name them: the web's `playback_failed`,
/// `stream_rejected`, `hls_fatal` and `stream_refused`, Android's
/// `playback_error`, Apple's `avplayer_item_failed`. Each is a failure of a
/// *start* only while the attempt it belongs to is still pending; the same
/// event after a first frame is a mid-play failure and is not counted here.
pub(crate) const START_FAILURE_EVENTS: [&str; 6] = [
    "playback_failed",
    "stream_rejected",
    "hls_fatal",
    "stream_refused",
    "playback_error",
    "avplayer_item_failed",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartPhase {
    /// Media was requested and nothing has settled the attempt yet.
    Pending { opened: Instant },
    /// The attempt reached a first frame; later requests are the same play.
    Playing,
    /// The attempt failed or was refused. Kept briefly so the client's own
    /// report of a refusal the server already counted is not counted twice;
    /// the next media request opens a new attempt.
    Closed { at: Instant },
}

#[derive(Debug)]
struct StartEntry {
    item_id: Option<i64>,
    method: &'static str,
    phase: StartPhase,
    last_seen: Instant,
}

/// C-08 M5 row 4, failed starts per attempt: one entry per (viewer, file)
/// on this node, opened where media is really requested and resolved exactly
/// once, into `plurx_start_outcomes_total{method,outcome}`.
///
/// **An attempt is opened by a media request**, at the seam that already
/// hears every delivery begin ([`note_playback_started`]: an HLS create, a
/// direct-play GET, a progressive `stream.mp4`), never by `/decision` —
/// which is also asked by detail pages that only want markers or a subtitle
/// cost and would open attempts nobody made. Requests for the same file while
/// the attempt is pending (a range storm, a create after a fallback) join it.
///
/// **It resolves once**, on the first of: the client's `ttff` beacon (`ok`);
/// one of [`START_FAILURE_EVENTS`] (`failed`); the server answering a start
/// request for that file with an error (`refused`); or [`START_DEADLINE`]
/// passing with none of them (`cancelled` — the viewer left, or the player
/// hung without saying so). The beacons are paired by the authenticated user
/// and the `file_id` every first-party beacon already carries, so no client
/// change is needed.
///
/// **Known bound.** The ledger is node-local, like the direct-play registry
/// it sits beside. HLS is held to one node and the web client's cookie pins
/// the rest, but a request and the beacon about it may reach different nodes
/// for a client that keeps no cookie (Android) behind a cluster address:
/// the opening node then resolves the attempt `cancelled` and the beacon's
/// node cannot pair it. That second half is counted, in
/// `plurx_start_outcomes_unpaired_total{outcome}`, so the size of the error is
/// on the same page as the number it distorts.
pub struct StartAttempts {
    entries: Mutex<HashMap<(i64, i64), StartEntry>>,
    /// This ledger's own resolutions, for tests that must not read the
    /// process-wide counters every other test is also moving.
    #[cfg(test)]
    local: Mutex<HashMap<(&'static str, &'static str), u64>>,
}

impl Default for StartAttempts {
    fn default() -> Self {
        StartAttempts::new()
    }
}

fn start_method(method: Option<&str>) -> &'static str {
    match method {
        Some("direct_play") => "direct_play",
        Some("remux") => "remux",
        Some("transcode") => "transcode",
        _ => "unknown",
    }
}

impl StartAttempts {
    pub fn new() -> StartAttempts {
        StartAttempts {
            entries: Mutex::new(HashMap::new()),
            #[cfg(test)]
            local: Mutex::new(HashMap::new()),
        }
    }

    fn resolve(&self, method: &'static str, outcome: &'static str) {
        crate::telemetry::record_start_outcome(method, outcome);
        #[cfg(test)]
        if let Ok(mut local) = self.local.lock() {
            *local.entry((method, outcome)).or_default() += 1;
        }
    }

    fn unpaired(&self, outcome: &'static str) {
        crate::telemetry::record_start_unpaired(outcome);
        #[cfg(test)]
        if let Ok(mut local) = self.local.lock() {
            *local.entry(("unpaired", outcome)).or_default() += 1;
        }
    }

    /// Resolve every pending attempt past its deadline as `cancelled` and
    /// forget plays and tombstones nobody has touched. Runs inside every
    /// other operation and before each `/metrics` render, so an idle node
    /// still reports the attempt that was abandoned last.
    fn sweep_locked(&self, entries: &mut HashMap<(i64, i64), StartEntry>, now: Instant) {
        entries.retain(|_, entry| match entry.phase {
            StartPhase::Pending { opened } => {
                if now.saturating_duration_since(opened) >= START_DEADLINE {
                    self.resolve(entry.method, "cancelled");
                    false
                } else {
                    true
                }
            }
            StartPhase::Playing => now.saturating_duration_since(entry.last_seen) < PLAY_QUIET,
            StartPhase::Closed { at } => now.saturating_duration_since(at) < START_DEADLINE,
        });
    }

    pub fn sweep(&self, now: Instant) {
        if let Ok(mut entries) = self.entries.lock() {
            self.sweep_locked(&mut entries, now);
        }
    }

    /// Media was requested for `file_id`: open an attempt, or join the one in
    /// flight, or refresh the play it belongs to.
    pub fn opened(
        &self,
        user_id: i64,
        file_id: i64,
        item_id: Option<i64>,
        method: &str,
        now: Instant,
    ) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        self.sweep_locked(&mut entries, now);
        let method = start_method(Some(method));
        let key = (user_id, file_id);
        if let Some(entry) = entries.get_mut(&key) {
            match entry.phase {
                StartPhase::Pending { .. } => {
                    entry.method = method;
                    entry.last_seen = now;
                    entry.item_id = item_id.or(entry.item_id);
                    return;
                }
                StartPhase::Playing => {
                    entry.last_seen = now;
                    return;
                }
                StartPhase::Closed { .. } => {}
            }
        } else if entries.len() >= START_LEDGER_CAP {
            return;
        }
        entries.insert(
            key,
            StartEntry {
                item_id,
                method,
                phase: StartPhase::Pending { opened: now },
                last_seen: now,
            },
        );
    }

    /// A client beacon about `file_id`. Only `ttff` and
    /// [`START_FAILURE_EVENTS`] can resolve an attempt; any beacon about a
    /// play in progress keeps that play alive.
    pub fn client_event(
        &self,
        user_id: i64,
        file_id: i64,
        event: &str,
        method: Option<&str>,
        now: Instant,
    ) {
        let first_frame = event == "ttff";
        let failure = START_FAILURE_EVENTS.contains(&event);
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        self.sweep_locked(&mut entries, now);
        let Some(entry) = entries.get_mut(&(user_id, file_id)) else {
            if first_frame {
                self.unpaired("ok");
            } else if failure {
                self.unpaired("failed");
            }
            return;
        };
        match entry.phase {
            StartPhase::Pending { .. } if first_frame => {
                // The method the client says it is playing through: a
                // fallback may have moved it since the request that opened
                // the attempt.
                let method = match start_method(method) {
                    "unknown" => entry.method,
                    named => named,
                };
                self.resolve(method, "ok");
                entry.phase = StartPhase::Playing;
                entry.last_seen = now;
            }
            StartPhase::Pending { .. } if failure => {
                self.resolve(entry.method, "failed");
                entry.phase = StartPhase::Closed { at: now };
            }
            StartPhase::Pending { .. } => {}
            StartPhase::Playing => entry.last_seen = now,
            // Already resolved; a first frame here is one no request opened.
            StartPhase::Closed { .. } => {
                if first_frame {
                    self.unpaired("ok");
                }
            }
        }
    }

    /// The server answered a start request for `file_id` with an error. A
    /// pending attempt is refused; with none in flight the request was an
    /// attempt of its own, refused at once. A play already showing a picture
    /// is not a start, so a refused replacement for it is not counted here.
    pub fn refused(&self, user_id: i64, file_id: i64, method: Option<&str>, now: Instant) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        self.sweep_locked(&mut entries, now);
        let key = (user_id, file_id);
        match entries.get_mut(&key) {
            Some(entry) if matches!(entry.phase, StartPhase::Playing) => {}
            Some(entry) if matches!(entry.phase, StartPhase::Pending { .. }) => {
                self.resolve(entry.method, "refused");
                entry.phase = StartPhase::Closed { at: now };
            }
            Some(entry) => {
                self.resolve(start_method(method), "refused");
                entry.phase = StartPhase::Closed { at: now };
            }
            None => {
                self.resolve(start_method(method), "refused");
                if entries.len() < START_LEDGER_CAP {
                    entries.insert(
                        key,
                        StartEntry {
                            item_id: None,
                            method: start_method(method),
                            phase: StartPhase::Closed { at: now },
                            last_seen: now,
                        },
                    );
                }
            }
        }
    }

    /// A live progress beat for `item_id`: the viewer is still in front of
    /// that title, so its play stays one play however long it is paused.
    pub fn progress_beat(&self, user_id: i64, item_id: i64, now: Instant) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        for ((user, _), entry) in entries.iter_mut() {
            if *user == user_id
                && entry.item_id == Some(item_id)
                && matches!(entry.phase, StartPhase::Playing)
            {
                entry.last_seen = now;
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn count(&self, method: &str, outcome: &str) -> u64 {
        self.local
            .lock()
            .ok()
            .and_then(|local| {
                local
                    .iter()
                    .find(|((m, o), _)| *m == method && *o == outcome)
                    .map(|(_, n)| *n)
            })
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub(crate) fn phase(&self, user_id: i64, file_id: i64) -> Option<StartPhase> {
        self.entries
            .lock()
            .ok()
            .and_then(|entries| entries.get(&(user_id, file_id)).map(|entry| entry.phase))
    }

    #[cfg(test)]
    pub(crate) fn last_seen(&self, user_id: i64, file_id: i64) -> Option<Instant> {
        self.entries.lock().ok().and_then(|entries| {
            entries
                .get(&(user_id, file_id))
                .map(|entry| entry.last_seen)
        })
    }
}

/// Note that a viewer has actually started receiving media for `file_id`.
///
/// Fire-and-forget: the work (resolving the item, reading resume position,
/// calling Trakt) happens in a detached task, because the caller is in the
/// middle of serving a stream and must not wait for any of it.
/// `method` is taken from every caller, not only from the one that gets
/// recorded: which routes need a registry entry of their own is a decision
/// [`crate::delivery`] makes once, and passing the method here is what stops a
/// fifth delivery route from being added without answering it.
///
/// `playback_id` is the client's own id for its player, where it sends one. It
/// is not a capability and is trusted for nothing but grouping — the worst a
/// guessed id does is merge two of the guesser's own rows.
pub fn note_playback_started(
    state: &crate::state::AppState,
    user_id: i64,
    user_name: &str,
    file_id: i64,
    method: Method,
    playback_id: Option<&str>,
) {
    let registry_key = method
        .needs_registry()
        .then(|| Key::new(user_id, file_id, playback_id));
    let start_method = method.metric_label();
    let user_name = user_name.to_owned();
    let state = state.clone();
    tokio::spawn(async move {
        let Ok(Some(file)) = state.store.get_file(file_id).await else {
            return;
        };
        // Ahead of the dedup claim, not behind it. The claim exists to
        // announce one start per play; the registry wants to hear about every
        // request, because that repetition is the heartbeat keeping a live
        // viewer on the activity page. Recorded behind the claim, an entry
        // would be written once and then expire under somebody still watching.
        if let Some(key) = registry_key {
            state
                .direct_plays
                .record(key, &user_name, file_id, file.item_id);
        }
        // Every media request, like the registry above: the first opens a
        // start attempt (C-08 M5 row 4) and the rest join or refresh it.
        state.start_attempts.opened(
            user_id,
            file_id,
            Some(file.item_id),
            start_method,
            Instant::now(),
        );
        if !state.starts.claim(user_id, file.item_id) {
            return;
        }
        // Resume position as a percentage, which is what a scrobble start
        // wants — Trakt shows "watching, 34% in" rather than restarting it.
        let percent = state
            .store
            .watch_state(user_id, file.item_id)
            .await
            .ok()
            .flatten()
            .and_then(|w| {
                w.duration_ms
                    .filter(|d| *d > 0)
                    .map(|d| (w.position_ms as f64 / d as f64 * 100.0).clamp(0.0, 100.0))
            })
            .unwrap_or(0.0);
        state.trakt.on_start(user_id, file.item_id, percent);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn availability_caches_presence_but_never_absence() {
        let dir = crate::test_tempdir().expect("tempdir");
        let present = dir.path().join("here.mkv");
        tokio::fs::write(&present, b"x").await.expect("write");
        let missing = dir.path().join("gone.mkv");

        let cache = AvailabilityCache::new();
        assert!(cache.is_present(1, &present).await);
        assert!(!cache.is_present(2, &missing).await);

        // A file that vanishes after being cached still reads as present for
        // the TTL — the cache's honest limit, and why the open that follows is
        // the authority.
        tokio::fs::remove_file(&present).await.expect("remove");
        assert!(cache.is_present(1, &present).await);
        // Until something reports the failure.
        cache.forget(1);
        assert!(!cache.is_present(1, &present).await);

        // Absence is never cached: the second ask re-checks, so a share that
        // comes back is playable immediately rather than a minute later.
        tokio::fs::write(&missing, b"x").await.expect("write");
        assert!(cache.is_present(2, &missing).await);
    }

    /// C-08 M5 row 4: each way an attempt ends is counted once, under the
    /// method of the attempt, and a range storm is one attempt.
    #[test]
    fn a_start_attempt_resolves_once_by_its_first_terminal() {
        let ledger = StartAttempts::new();
        let t0 = Instant::now();
        // ok: three range requests, then the first frame, then a second
        // first-frame report and a failure after the picture.
        for offset in 0..3 {
            ledger.opened(
                1,
                10,
                Some(100),
                "direct_play",
                t0 + Duration::from_millis(offset),
            );
        }
        ledger.client_event(
            1,
            10,
            "ttff",
            Some("direct_play"),
            t0 + Duration::from_secs(2),
        );
        ledger.client_event(
            1,
            10,
            "ttff",
            Some("direct_play"),
            t0 + Duration::from_secs(3),
        );
        ledger.client_event(1, 10, "playback_error", None, t0 + Duration::from_secs(4));
        assert_eq!(ledger.count("direct_play", "ok"), 1);
        assert_eq!(
            ledger.count("direct_play", "failed"),
            0,
            "a mid-play failure is not a failed start"
        );
        // failed, and the fallback that follows is a new attempt.
        ledger.opened(1, 11, Some(101), "direct_play", t0);
        ledger.client_event(
            1,
            11,
            "stream_rejected",
            Some("direct_play"),
            t0 + Duration::from_secs(1),
        );
        ledger.opened(1, 11, Some(101), "transcode", t0 + Duration::from_secs(2));
        ledger.client_event(
            1,
            11,
            "ttff",
            Some("transcode"),
            t0 + Duration::from_secs(5),
        );
        assert_eq!(ledger.count("direct_play", "failed"), 1);
        assert_eq!(ledger.count("transcode", "ok"), 1);
        // refused while pending, and the client's own report of it is not a
        // second outcome or an unpaired one.
        ledger.opened(1, 12, Some(102), "remux", t0);
        ledger.refused(1, 12, Some("remux"), t0 + Duration::from_secs(1));
        ledger.client_event(
            1,
            12,
            "stream_refused",
            Some("remux"),
            t0 + Duration::from_secs(1),
        );
        assert_eq!(ledger.count("remux", "refused"), 1);
        assert_eq!(ledger.count("remux", "failed"), 0);
        assert_eq!(ledger.count("unpaired", "failed"), 0);
        // The request that retries it is a new attempt, not part of a play.
        ledger.opened(1, 12, Some(102), "remux", t0 + Duration::from_secs(2));
        assert!(matches!(
            ledger.phase(1, 12),
            Some(StartPhase::Pending { .. })
        ));
        // refused with nothing in flight: an attempt refused at its request.
        ledger.refused(1, 13, None, t0);
        assert_eq!(ledger.count("unknown", "refused"), 1);
        // cancelled: nothing arrives before the deadline.
        ledger.opened(2, 10, Some(100), "transcode", t0);
        ledger.sweep(t0 + START_DEADLINE - Duration::from_millis(1));
        assert_eq!(ledger.count("transcode", "cancelled"), 0);
        ledger.sweep(t0 + START_DEADLINE);
        assert_eq!(ledger.count("transcode", "cancelled"), 1);
        assert_eq!(ledger.phase(2, 10), None);
        // A first frame the deadline already gave up on is not a second outcome.
        ledger.client_event(2, 10, "ttff", Some("transcode"), t0 + START_DEADLINE);
        assert_eq!(ledger.count("transcode", "ok"), 1);
        assert_eq!(ledger.count("unpaired", "ok"), 1);
    }

    /// A play stays one play while its viewer is in front of it, paused or
    /// not, and a request after the viewer has gone is a new attempt.
    #[test]
    fn a_play_is_kept_by_its_progress_beats_and_forgotten_when_they_stop() {
        let ledger = StartAttempts::new();
        let t0 = Instant::now();
        ledger.opened(1, 10, Some(100), "direct_play", t0);
        ledger.client_event(1, 10, "ttff", None, t0 + Duration::from_secs(1));
        assert_eq!(ledger.phase(1, 10), Some(StartPhase::Playing));
        // Paused far longer than the quiet window, beating every 10 s.
        let mut now = t0 + Duration::from_secs(1);
        while now < t0 + PLAY_QUIET * 3 {
            now += Duration::from_secs(10);
            ledger.progress_beat(1, 100, now);
            ledger.sweep(now);
        }
        // The resume's range request joins the play instead of opening a
        // start nobody will report a first frame for.
        ledger.opened(1, 10, Some(100), "direct_play", now);
        assert_eq!(ledger.phase(1, 10), Some(StartPhase::Playing));
        ledger.sweep(now + START_DEADLINE);
        assert_eq!(ledger.count("direct_play", "cancelled"), 0);
        // A beat for another item, or another viewer, keeps nothing alive.
        let later = now + PLAY_QUIET;
        ledger.progress_beat(1, 999, later);
        ledger.progress_beat(2, 100, later);
        ledger.sweep(later);
        assert_eq!(ledger.phase(1, 10), None);
        ledger.opened(1, 10, Some(100), "direct_play", later);
        assert!(matches!(
            ledger.phase(1, 10),
            Some(StartPhase::Pending { .. })
        ));
    }

    #[test]
    fn a_full_ledger_tracks_no_new_attempt_rather_than_evicting_one() {
        let ledger = StartAttempts::new();
        let t0 = Instant::now();
        for file in 0..START_LEDGER_CAP as i64 {
            ledger.opened(1, file, None, "remux", t0);
        }
        ledger.opened(2, 1, None, "remux", t0);
        assert_eq!(ledger.phase(2, 1), None);
        assert!(ledger.phase(1, 0).is_some());
        ledger.sweep(t0 + START_DEADLINE);
        assert_eq!(ledger.count("remux", "cancelled"), START_LEDGER_CAP as u64);
    }

    #[test]
    fn one_playback_announces_once() {
        let notifier = StartNotifier::new();
        assert!(notifier.claim(1, 10), "first request announces");
        assert!(!notifier.claim(1, 10), "the rest of the same play do not");
        assert!(
            notifier.claim(1, 11),
            "a different item is a different play"
        );
        assert!(notifier.claim(2, 10), "so is a different viewer");
    }
}
