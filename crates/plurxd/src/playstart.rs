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
use std::sync::{Arc, Mutex};
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

/// How long a play that reached its first frame may go with no sign of its
/// viewer and still have a media request for it join it outright. Every
/// later request for the file, every client beacon about it and every live
/// progress beat for its item refresh it. Only the web beats while paused (a
/// 60 s floor); Apple and Android beat only while playing, so a native pause
/// is silent however long it lasts. A request after this much silence is
/// therefore either that pause ending or a new open of the same title, and
/// the ledger does not guess: it opens a *possible resume*
/// ([`StartPhase::Pending`] with `after_play`), which only an explicit start
/// signal turns into an attempt. The same window [`START_DEDUP`] gives a
/// Trakt announcement.
pub(crate) const PLAY_QUIET: Duration = START_DEDUP;

/// How long a play is remembered at all with no sign of its viewer: long
/// enough that a native player left paused overnight still resumes into the
/// play it was, rather than into a start nobody reports a first frame for.
/// Forgetting it at [`PLAY_QUIET`] made every native pause past five minutes
/// a phantom start (#559 review, finding 2).
pub(crate) const PLAY_FORGET: Duration = Duration::from_secs(24 * 60 * 60);

/// The `code`s a start request is refused with that mean "not yet, ask
/// again": exactly the `create_503_not_yet` row of
/// `tests/playback/playback-surface-contract.json`, which all three clients
/// retry on their 1 s / 2 s / 4 s ladder with the same request, and which
/// the web answers for `vod_index_pending` by falling back to a progressive
/// remux. Such a refusal does not end the attempt: it keeps it pending, so
/// the retry or the fallback that follows joins it (#559 review, finding 1).
pub(crate) const START_NOT_YET_CODES: [&str; 7] = [
    "startup_timeout",
    "media_owner_transition",
    "vod_index_pending",
    "vod_engine_unattested",
    "transcode_capacity_pending",
    "media_session_overloaded",
    "quality_catalog_unavailable",
];

/// The `reason`s a `ttff` beacon gives for a player that was just opened, as
/// all three clients name them: `cold-start` from the beginning, `resume`
/// from a saved position. Every other reason (`seek`, `quality`, `audio`,
/// `selection`, `fallback`, `stall-*`) is a picture inside a play already
/// running. A beacon with no reason is an older client's open.
const START_REASONS: [&str; 2] = ["cold-start", "resume"];

fn starts_a_play(reason: Option<&str>) -> bool {
    reason.is_none_or(|reason| START_REASONS.contains(&reason))
}

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
    ///
    /// `not_yet`: the server's last answer to it was one of
    /// [`START_NOT_YET_CODES`] and no media has been served since, so the
    /// client is expected to ask again. Reaching the deadline like that is a
    /// `refused` start, not a `cancelled` one: the server never served it.
    ///
    /// `after_play`: it was opened by a request for a play that had gone
    /// quiet for [`PLAY_QUIET`], so it may be nothing but that play's resume.
    /// Only a first frame reported as a start, a failure report or a refusal
    /// makes it an attempt; a first frame for any other reason, or the
    /// deadline, returns it to the play uncounted.
    Pending {
        opened: Instant,
        not_yet: bool,
        after_play: bool,
    },
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
    /// The create (`request_claim_id`) that most recently opened or joined
    /// this entry. The entry is keyed by (viewer, file), so two devices of one
    /// viewer on one title share it and this names only the newer; the
    /// per-create record is [`StartAttempts::settle`]'s session index.
    attempt: Option<String>,
}

/// One create that is waiting for its client's first-frame or failure report.
#[derive(Debug)]
struct PendingStart {
    user_id: i64,
    file_id: i64,
    attempt: String,
    at: Instant,
}

/// The create's identity as the ledger learns it: the attempt id (the
/// create's `request_claim_id`) and the session the create answered with,
/// which is the one thing the client's beacons name that tells two devices of
/// one viewer on one title apart.
#[derive(Clone, Debug)]
pub struct AttemptTag {
    pub attempt: String,
    pub session_id: String,
}

/// How a client's beacon settled a create.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettledOutcome {
    Presented,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettledStart {
    pub attempt: String,
    pub outcome: SettledOutcome,
    /// The session the settled create answered with; `None` for a direct or
    /// progressive start, which has none. The first-frame row names this,
    /// so it pairs with the attempt row whatever the beacon carried.
    pub session: Option<String>,
}

/// The sessions index key of a direct or progressive start (no session of
/// its own): its attempt id behind a prefix no session id carries.
const REQUEST_START_PREFIX: &str = "request-start:";

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
/// hung without saying so). A refusal the clients retry
/// ([`START_NOT_YET_CODES`]) keeps the attempt pending instead. The beacons
/// are paired by the authenticated user and the `file_id` every first-party
/// beacon already carries, so no client change is needed.
///
/// **After a first frame it is one play** until [`PLAY_FORGET`] with no sign
/// of the viewer. A request after [`PLAY_QUIET`] of silence (a native pause
/// ending, or the title opened again) becomes an attempt only on an explicit
/// start signal, and otherwise rejoins the play.
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
    /// Creates awaiting a client report, by the session each answered with.
    /// Independent of `entries`' phase machine on purpose: that machine
    /// counts one outcome per (viewer, file) and so cannot say which of two
    /// devices a first frame belongs to; this index can.
    sessions: Mutex<HashMap<String, PendingStart>>,
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
            sessions: Mutex::new(HashMap::new()),
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
            StartPhase::Pending {
                opened,
                not_yet,
                after_play,
            } => {
                if now.saturating_duration_since(opened) < START_DEADLINE {
                    true
                } else if not_yet {
                    // The client stopped asking after being told "not yet":
                    // the server never served this attempt.
                    self.resolve(entry.method, "refused");
                    false
                } else if after_play {
                    // A quiet play's request and no start signal after it:
                    // the pause ended, silently, as a native player's does.
                    entry.phase = StartPhase::Playing;
                    true
                } else {
                    self.resolve(entry.method, "cancelled");
                    false
                }
            }
            StartPhase::Playing => now.saturating_duration_since(entry.last_seen) < PLAY_FORGET,
            StartPhase::Closed { at } => now.saturating_duration_since(at) < START_DEADLINE,
        });
    }

    pub fn sweep(&self, now: Instant) {
        if let Ok(mut entries) = self.entries.lock() {
            self.sweep_locked(&mut entries, now);
        }
    }

    /// Media was requested for `file_id`: open an attempt, or join the one in
    /// flight, or refresh the play it belongs to, or (for a play quiet for
    /// [`PLAY_QUIET`]) hold a possible resume of it.
    #[cfg(test)]
    pub fn opened(
        &self,
        user_id: i64,
        file_id: i64,
        item_id: Option<i64>,
        method: &str,
        now: Instant,
    ) {
        self.opened_tagged(user_id, file_id, item_id, method, None, now);
    }

    /// [`Self::opened`] for a request that is a create with an identity of
    /// its own: the attempt is remembered on the entry. Matching the client's
    /// report back to the create is [`Self::tag_session`]'s, taken at the
    /// create itself rather than in the detached task that opens the entry,
    /// so no report can beat it.
    ///
    /// True when this request opened a new attempt (not a join, a refresh
    /// or a possible resume).
    pub fn opened_tagged(
        &self,
        user_id: i64,
        file_id: i64,
        item_id: Option<i64>,
        method: &str,
        tag: Option<&AttemptTag>,
        now: Instant,
    ) -> bool {
        let attempt = tag.map(|tag| tag.attempt.clone());
        let Ok(mut entries) = self.entries.lock() else {
            return false;
        };
        self.sweep_locked(&mut entries, now);
        let method = start_method(Some(method));
        let key = (user_id, file_id);
        if let Some(entry) = entries.get_mut(&key) {
            match &mut entry.phase {
                StartPhase::Pending { not_yet, .. } => {
                    // The joining request names the method now in use: a
                    // fallback's, or the one a retried create has decided.
                    // Media is being served, so no longer "not yet".
                    *not_yet = false;
                    entry.method = method;
                    entry.last_seen = now;
                    entry.item_id = item_id.or(entry.item_id);
                    entry.attempt = attempt.or(entry.attempt.take());
                    return false;
                }
                StartPhase::Playing
                    if now.saturating_duration_since(entry.last_seen) < PLAY_QUIET =>
                {
                    entry.last_seen = now;
                    return false;
                }
                StartPhase::Playing => {
                    entry.phase = StartPhase::Pending {
                        opened: now,
                        not_yet: false,
                        after_play: true,
                    };
                    entry.method = method;
                    entry.last_seen = now;
                    entry.item_id = item_id.or(entry.item_id);
                    entry.attempt = attempt.or(entry.attempt.take());
                    return false;
                }
                StartPhase::Closed { .. } => {}
            }
        } else if entries.len() >= START_LEDGER_CAP {
            return false;
        }
        entries.insert(
            key,
            StartEntry {
                item_id,
                method,
                phase: StartPhase::Pending {
                    opened: now,
                    not_yet: false,
                    after_play: false,
                },
                last_seen: now,
                attempt,
            },
        );
        true
    }

    /// A direct or progressive request opened attempt `attempt`: name it on
    /// the (viewer, file) entry and wait for its client's report, which can
    /// name no session of this server's (there is none) and is matched by
    /// that newest attempt instead.
    pub fn tag_request(&self, user_id: i64, file_id: i64, attempt: &str, now: Instant) {
        if let Ok(mut entries) = self.entries.lock() {
            if let Some(entry) = entries.get_mut(&(user_id, file_id)) {
                entry.attempt = Some(attempt.to_owned());
            }
        }
        self.tag_session(
            user_id,
            file_id,
            &AttemptTag {
                attempt: attempt.to_owned(),
                session_id: format!("{REQUEST_START_PREFIX}{attempt}"),
            },
            now,
        );
    }

    /// A create answered with `tag.session_id`: remember which attempt that
    /// session belongs to until its client reports.
    pub fn tag_session(&self, user_id: i64, file_id: i64, tag: &AttemptTag, now: Instant) {
        let Ok(mut sessions) = self.sessions.lock() else {
            return;
        };
        sessions.retain(|_, pending| now.saturating_duration_since(pending.at) < START_DEADLINE);
        if sessions.len() < START_LEDGER_CAP || sessions.contains_key(&tag.session_id) {
            sessions.insert(
                tag.session_id.clone(),
                PendingStart {
                    user_id,
                    file_id,
                    attempt: tag.attempt.clone(),
                    at: now,
                },
            );
        }
    }

    /// A client report that settles a create: the first frame (`ttff`, for
    /// any reason — a seek's or a quality change's new session is a create
    /// too) or one of [`START_FAILURE_EVENTS`]. Returns the attempt it
    /// belongs to, once: the create is no longer waiting afterwards.
    ///
    /// The report is matched by the session it names, so two devices of one
    /// viewer on one title settle their own creates. A report that names no
    /// session (an older client) falls back to the newest create for the
    /// (viewer, file), which is the old ledger key's answer and is the only
    /// one such a report can have. `None` when nothing waits for it: the
    /// create was handled by another node, or has already been settled.
    pub fn settle(
        &self,
        user_id: i64,
        file_id: i64,
        session_id: Option<&str>,
        event: &str,
        now: Instant,
    ) -> Option<SettledStart> {
        let outcome = if event == "ttff" {
            SettledOutcome::Presented
        } else if START_FAILURE_EVENTS.contains(&event) {
            SettledOutcome::Failed
        } else {
            return None;
        };
        // The entry's newest attempt, read before the session index is taken
        // so the two locks are never held together.
        let newest = self
            .entries
            .lock()
            .ok()?
            .get(&(user_id, file_id))
            .and_then(|entry| entry.attempt.clone());
        let mut sessions = self.sessions.lock().ok()?;
        sessions.retain(|_, pending| now.saturating_duration_since(pending.at) < START_DEADLINE);
        let mine =
            |pending: &PendingStart| pending.user_id == user_id && pending.file_id == file_id;
        let by_session = session_id.and_then(|session| {
            sessions
                .get(session)
                .filter(|pending| mine(pending))
                .map(|_| session.to_owned())
        });
        let key = match by_session {
            Some(key) => key,
            // A report naming a session no create of this node answered with
            // may still be about a direct or progressive start, which has no
            // session; one naming none gets the newest create, whatever it is.
            None => {
                let attempt = newest?;
                sessions
                    .iter()
                    .find(|(key, pending)| {
                        pending.attempt == attempt
                            && mine(pending)
                            && (session_id.is_none() || key.starts_with(REQUEST_START_PREFIX))
                    })
                    .map(|(key, _)| key.clone())?
            }
        };
        let pending = sessions.remove(&key)?;
        Some(SettledStart {
            attempt: pending.attempt,
            outcome,
            session: (!key.starts_with(REQUEST_START_PREFIX)).then_some(key),
        })
    }

    /// A client beacon about `file_id`. Only `ttff` and
    /// [`START_FAILURE_EVENTS`] can resolve an attempt; any beacon about a
    /// play in progress keeps that play alive. `reason` is the beacon's own
    /// (why its playback attempt began), which tells a possible resume that
    /// became a new open from one that was a resume after all.
    pub fn client_event(
        &self,
        user_id: i64,
        file_id: i64,
        event: &str,
        method: Option<&str>,
        reason: Option<&str>,
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
            StartPhase::Pending {
                after_play: true, ..
            } if first_frame && !starts_a_play(reason) => {
                // A seek's or a stall's picture after a quiet play's request:
                // that play resumed, and it is not a start.
                entry.phase = StartPhase::Playing;
                entry.last_seen = now;
            }
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

    /// The server answered a start request for `file_id` with an error,
    /// carrying the typed `code` when it has one.
    ///
    /// A code in [`START_NOT_YET_CODES`] tells the client to ask again, so it
    /// keeps the attempt pending (or opens one) for that retry, or the
    /// fallback it takes instead, to join. Any other refusal ends a pending
    /// attempt `refused`; with none in flight the request was an attempt of
    /// its own, refused at once. A play already showing a picture is not a
    /// start, so a refused replacement for it is not counted here.
    pub fn refused(
        &self,
        user_id: i64,
        file_id: i64,
        method: Option<&str>,
        code: Option<&str>,
        now: Instant,
    ) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        self.sweep_locked(&mut entries, now);
        let key = (user_id, file_id);
        if code.is_some_and(|code| START_NOT_YET_CODES.contains(&code)) {
            let waiting = StartPhase::Pending {
                opened: now,
                not_yet: true,
                after_play: false,
            };
            let room = entries.len() < START_LEDGER_CAP;
            match entries.get_mut(&key) {
                Some(entry) if matches!(entry.phase, StartPhase::Playing) => {}
                Some(entry) => {
                    match &mut entry.phase {
                        StartPhase::Pending { not_yet, .. } => *not_yet = true,
                        // After a failed or refused attempt: a new one, waiting.
                        phase => {
                            *phase = waiting;
                            entry.method = start_method(method);
                        }
                    }
                    entry.last_seen = now;
                }
                None if room => {
                    entries.insert(
                        key,
                        StartEntry {
                            item_id: None,
                            method: start_method(method),
                            phase: waiting,
                            last_seen: now,
                            attempt: None,
                        },
                    );
                }
                None => {}
            }
            return;
        }
        match entries.get_mut(&key) {
            Some(entry) if matches!(entry.phase, StartPhase::Playing) => {}
            Some(entry) if matches!(entry.phase, StartPhase::Pending { .. }) => {
                // An attempt only a refused request has touched has the
                // refusing request's method, not `unknown`.
                let method = match entry.method {
                    "unknown" => start_method(method),
                    known => known,
                };
                self.resolve(method, "refused");
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
                            attempt: None,
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

/// What a create reports about itself to the handler boundary that persists
/// it: the attempt id and the owner are only known deep inside the create, and
/// the boundary that logs the create's read budget is two calls up. A
/// task-local carries them out without widening the create's signature.
#[derive(Debug, Default)]
pub(crate) struct CreateNote {
    attempt: Option<String>,
    owner_node: Option<String>,
}

impl CreateNote {
    /// The note a create fills and the boundary that spawned it reads; shared
    /// because the create runs as its own task that outlives a viewer.
    pub(crate) fn shared() -> Arc<Mutex<CreateNote>> {
        Arc::default()
    }
    /// What the create has said so far.
    pub(crate) fn take(note: &Arc<Mutex<CreateNote>>) -> CreateNote {
        std::mem::take(&mut *note.lock().unwrap_or_else(|e| e.into_inner()))
    }
    pub(crate) fn attempt(&self) -> Option<&str> {
        self.attempt.as_deref()
    }
    pub(crate) fn owner_node(&self) -> Option<&str> {
        self.owner_node.as_deref()
    }
}

tokio::task_local! {
    static CREATE_NOTE: Arc<Mutex<CreateNote>>;
}

/// Run one create with the [`CreateNote`] it can fill. The caller keeps the
/// handle, so a create that times out, or whose viewer goes away, still
/// reports the attempt it had reached.
pub(crate) async fn scope_create_note<F: std::future::Future>(
    note: Arc<Mutex<CreateNote>>,
    create: F,
) -> F::Output {
    CREATE_NOTE.scope(note, create).await
}

/// [`scope_create_note`] with a fresh note, returned with the output.
#[cfg(test)]
pub(crate) async fn with_create_note<F: std::future::Future>(create: F) -> (F::Output, CreateNote) {
    let note = CreateNote::shared();
    let output = scope_create_note(Arc::clone(&note), create).await;
    (output, CreateNote::take(&note))
}

/// The create claimed its durable admission row under `attempt`.
pub(crate) fn note_create_attempt(attempt: &str) {
    let _ = CREATE_NOTE.try_with(|note| {
        note.lock().unwrap_or_else(|e| e.into_inner()).attempt = Some(attempt.to_owned());
    });
}

/// The node that owns the session this create published.
pub(crate) fn note_create_owner(owner_node: &str) {
    let _ = CREATE_NOTE.try_with(|note| {
        note.lock().unwrap_or_else(|e| e.into_inner()).owner_node = Some(owner_node.to_owned());
    });
}

/// One create, as the node-local telemetry sidecar keeps it (G1).
pub(crate) struct StartAttemptRecord {
    pub at_unix_ms: i64,
    pub user_id: i64,
    pub file_id: i64,
    pub attempt: String,
    /// The session the create answered with, when it answered.
    pub session_id: Option<String>,
    /// `vod` or `rolling` once the create got far enough to know.
    pub route: Option<&'static str>,
    /// The refusal's `code`, or `http_<status>` for a refusal that carries
    /// none, which is itself the finding.
    pub refusal: Option<String>,
    pub refusal_message: Option<String>,
    pub client_build: String,
    pub owner_node: Option<String>,
    pub phases: String,
    pub authority_reads: u64,
    pub local_reads: u64,
    pub mutations: u64,
    pub store_ms: u64,
    pub elapsed_ms: u64,
}

/// `name:12ms/1a/2l/0w,…` (the create's read-budget phases) as rows.
pub(crate) fn parse_phases(rendered: &str) -> Vec<serde_json::Value> {
    rendered
        .split(',')
        .filter_map(|phase| {
            let (name, counts) = phase.split_once(':')?;
            let mut parts = counts.split('/');
            let mut number = |suffix: &str| parts.next()?.strip_suffix(suffix)?.parse::<u64>().ok();
            Some(serde_json::json!({
                "name": name,
                "ms": number("ms")?,
                "authority_reads": number("a")?,
                "local_reads": number("l")?,
                "mutations": number("w")?,
            }))
        })
        .collect()
}

fn clip_text(value: &str, max: usize) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect()
}

impl StartAttemptRecord {
    pub(crate) fn into_event(self) -> plurx_core::domain::PlaybackEvent {
        let extra = serde_json::json!({
            "phases": parse_phases(&self.phases),
            "authority_reads": self.authority_reads,
            "local_reads": self.local_reads,
            "mutations": self.mutations,
            "store_ms": self.store_ms,
            "elapsed_ms": self.elapsed_ms,
            "owner_node": self.owner_node,
            "client_build": clip_text(&self.client_build, 48),
        });
        plurx_core::domain::PlaybackEvent {
            at_unix_ms: self.at_unix_ms,
            user_id: Some(self.user_id),
            session_id: self
                .session_id
                .as_deref()
                .map(crate::transcode::session_log_id),
            file_id: Some(self.file_id),
            event: "start_attempt".to_owned(),
            method: self.route.map(str::to_owned),
            detail: self.refusal_message.as_deref().map(|m| clip_text(m, 200)),
            attempt: Some(clip_text(&self.attempt, 64)),
            reason: self.refusal.as_deref().map(|r| clip_text(r, 48)),
            extra: Some(extra.to_string()),
            ..plurx_core::domain::PlaybackEvent::default()
        }
    }
}

/// The first-frame row for a create the client's `ttff` report settled; `ms`
/// is the report's own time to first frame.
pub(crate) fn presented_event(
    attempt: &str,
    (user_id, file_id): (i64, i64),
    raw_session_id: Option<&str>,
    method: Option<&str>,
    ttff_ms: Option<i64>,
    reason: Option<&str>,
    now_unix_ms: i64,
) -> plurx_core::domain::PlaybackEvent {
    plurx_core::domain::PlaybackEvent {
        at_unix_ms: now_unix_ms,
        user_id: Some(user_id),
        session_id: raw_session_id.map(crate::transcode::session_log_id),
        file_id: Some(file_id),
        event: "start_presented".to_owned(),
        method: method.map(|m| clip_text(m, 16)),
        ms: ttff_ms.filter(|ms| *ms >= 0),
        attempt: Some(clip_text(attempt, 64)),
        reason: reason.map(|r| clip_text(r, 24)),
        ..plurx_core::domain::PlaybackEvent::default()
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
    tag: Option<AttemptTag>,
) {
    note_started(
        state,
        (user_id, user_name),
        file_id,
        method,
        playback_id,
        tag.map_or(StartIdentity::None, StartIdentity::Create),
    );
}

/// [`note_playback_started`] for a direct or progressive request, which has
/// no create of its own: the request that opens a new attempt also writes
/// the `start_attempt` row (route `direct` or `progressive`), so a start
/// that fell back from HLS (`vod_index_pending` → progressive remux) shows
/// as the second start it is. `client_build` is the request's own label.
pub fn note_request_started(
    state: &crate::state::AppState,
    user_id: i64,
    user_name: &str,
    file_id: i64,
    method: Method,
    playback_id: Option<&str>,
    client_build: String,
) {
    note_started(
        state,
        (user_id, user_name),
        file_id,
        method,
        playback_id,
        StartIdentity::Request { client_build },
    );
}

/// A create that answered by replaying the session another create of the
/// same request published -- a retry that joined its owner, on this ingress
/// or another. Tagged as the publishing create is, so the client's first
/// frame, which names that session, settles this create here.
pub(crate) fn note_replayed_create(
    state: &crate::state::AppState,
    user: &plurx_core::domain::User,
    file_id: i64,
    attempt: &str,
    (session_id, copy): (&str, bool),
    library_channel: bool,
) {
    if library_channel {
        return;
    }
    let tag = AttemptTag {
        attempt: attempt.to_owned(),
        session_id: session_id.to_owned(),
    };
    state
        .start_attempts
        .tag_session(user.id, file_id, &tag, Instant::now());
    note_playback_started(
        state,
        user.id,
        &user.username,
        file_id,
        if copy {
            Method::HlsCopy
        } else {
            Method::Transcode
        },
        None,
        Some(tag),
    );
}

/// A request's client label for its start row: the `User-Agent` (clipped
/// where the row is written), or `unknown`.
pub(crate) fn request_client_build(headers: &axum::http::HeaderMap) -> String {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|agent| !agent.is_empty())
        .map_or_else(|| "unknown".to_owned(), str::to_owned)
}

/// What a media request knows about the start it may open.
enum StartIdentity {
    None,
    /// A create, tagged with its claim id and the session it answered with.
    Create(AttemptTag),
    /// A direct or progressive request: no create, so the attempt it opens
    /// is recorded here.
    Request {
        client_build: String,
    },
}

fn note_started(
    state: &crate::state::AppState,
    (user_id, user_name): (i64, &str),
    file_id: i64,
    method: Method,
    playback_id: Option<&str>,
    identity: StartIdentity,
) {
    let (tag, request_build) = match identity {
        StartIdentity::None => (None, None),
        StartIdentity::Create(tag) => (Some(tag), None),
        StartIdentity::Request { client_build } => (None, Some(client_build)),
    };
    let route = match method {
        Method::Direct => "direct",
        Method::Remux => "progressive",
        Method::HlsCopy | Method::Transcode => "rolling",
    };
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
        let opened = state.start_attempts.opened_tagged(
            user_id,
            file_id,
            Some(file.item_id),
            start_method,
            tag.as_ref(),
            Instant::now(),
        );
        if let (true, Some(client_build)) = (opened, request_build) {
            let attempt = uuid::Uuid::new_v4().to_string();
            state
                .start_attempts
                .tag_request(user_id, file_id, &attempt, Instant::now());
            crate::telemetry::emit(
                Arc::clone(&state.store),
                StartAttemptRecord {
                    at_unix_ms: crate::state::clock_ms(),
                    user_id,
                    file_id,
                    attempt,
                    session_id: None,
                    route: Some(route),
                    refusal: None,
                    refusal_message: None,
                    client_build,
                    owner_node: Some(state.node_id.clone()),
                    phases: String::new(),
                    authority_reads: 0,
                    local_reads: 0,
                    mutations: 0,
                    store_ms: 0,
                    elapsed_ms: 0,
                }
                .into_event(),
            );
        }
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
            None,
            t0 + Duration::from_secs(2),
        );
        ledger.client_event(
            1,
            10,
            "ttff",
            Some("direct_play"),
            None,
            t0 + Duration::from_secs(3),
        );
        ledger.client_event(
            1,
            10,
            "playback_error",
            None,
            None,
            t0 + Duration::from_secs(4),
        );
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
            None,
            t0 + Duration::from_secs(1),
        );
        ledger.opened(1, 11, Some(101), "transcode", t0 + Duration::from_secs(2));
        ledger.client_event(
            1,
            11,
            "ttff",
            Some("transcode"),
            None,
            t0 + Duration::from_secs(5),
        );
        assert_eq!(ledger.count("direct_play", "failed"), 1);
        assert_eq!(ledger.count("transcode", "ok"), 1);
        // refused while pending, and the client's own report of it is not a
        // second outcome or an unpaired one.
        ledger.opened(1, 12, Some(102), "remux", t0);
        ledger.refused(1, 12, Some("remux"), None, t0 + Duration::from_secs(1));
        ledger.client_event(
            1,
            12,
            "stream_refused",
            Some("remux"),
            None,
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
        ledger.refused(1, 13, None, None, t0);
        assert_eq!(ledger.count("unknown", "refused"), 1);
        // cancelled: nothing arrives before the deadline.
        ledger.opened(2, 10, Some(100), "transcode", t0);
        ledger.sweep(t0 + START_DEADLINE - Duration::from_millis(1));
        assert_eq!(ledger.count("transcode", "cancelled"), 0);
        ledger.sweep(t0 + START_DEADLINE);
        assert_eq!(ledger.count("transcode", "cancelled"), 1);
        assert_eq!(ledger.phase(2, 10), None);
        // A first frame the deadline already gave up on is not a second outcome.
        ledger.client_event(2, 10, "ttff", Some("transcode"), None, t0 + START_DEADLINE);
        assert_eq!(ledger.count("transcode", "ok"), 1);
        assert_eq!(ledger.count("unpaired", "ok"), 1);
    }

    /// A play stays one play while its viewer is in front of it, whether or
    /// not the player beats while paused: the web beats (a 60 s floor),
    /// Apple and Android do not. Silence alone does not forget it; only a day
    /// with no sign of the viewer does.
    #[test]
    fn a_play_is_kept_by_its_progress_beats_and_held_through_a_silent_pause() {
        let ledger = StartAttempts::new();
        let t0 = Instant::now();
        ledger.opened(1, 10, Some(100), "direct_play", t0);
        ledger.client_event(1, 10, "ttff", None, None, t0 + Duration::from_secs(1));
        assert_eq!(ledger.phase(1, 10), Some(StartPhase::Playing));
        // Web: paused far longer than the quiet window, beating every 60 s.
        let mut now = t0 + Duration::from_secs(1);
        while now < t0 + PLAY_QUIET * 3 {
            now += Duration::from_secs(60);
            ledger.progress_beat(1, 100, now);
            ledger.sweep(now);
        }
        // The resume's range request joins the play outright.
        ledger.opened(1, 10, Some(100), "direct_play", now);
        assert_eq!(ledger.phase(1, 10), Some(StartPhase::Playing));
        ledger.sweep(now + START_DEADLINE);
        assert_eq!(ledger.count("direct_play", "cancelled"), 0);
        // A beat for another item, or another viewer, keeps nothing alive,
        // but going quiet does not end a play: a native pause is quiet.
        let later = now + START_DEADLINE + PLAY_QUIET;
        ledger.progress_beat(1, 999, later);
        ledger.progress_beat(2, 100, later);
        ledger.sweep(later);
        assert_eq!(ledger.phase(1, 10), Some(StartPhase::Playing));
        // A day of silence does, and the next request is an attempt of its own.
        let gone = now + PLAY_FORGET;
        ledger.sweep(gone);
        assert_eq!(ledger.phase(1, 10), None);
        ledger.opened(1, 10, Some(100), "direct_play", gone);
        assert!(matches!(
            ledger.phase(1, 10),
            Some(StartPhase::Pending {
                after_play: false,
                ..
            })
        ));
    }

    /// #559 review, finding 2: Apple and Android send no progress beat while
    /// paused, and no `ttff` when the viewer unpauses. A native pause of any
    /// length must resume into the same play, not into a start that resolves
    /// `cancelled` (or, after an Android seek, a second `ok`).
    #[test]
    fn a_native_pause_with_no_beats_resumes_into_the_same_play() {
        let ledger = StartAttempts::new();
        let t0 = Instant::now();
        ledger.opened(1, 10, Some(100), "direct_play", t0);
        ledger.client_event(
            1,
            10,
            "ttff",
            Some("direct_play"),
            Some("cold-start"),
            t0 + Duration::from_secs(1),
        );
        // Paused for ten minutes: no beat, no request.
        let resume = t0 + PLAY_QUIET * 2;
        ledger.sweep(resume);
        assert_eq!(ledger.phase(1, 10), Some(StartPhase::Playing));
        // The unpause's range request, and nothing after it.
        ledger.opened(1, 10, Some(100), "direct_play", resume);
        ledger.sweep(resume + START_DEADLINE);
        assert_eq!(
            ledger.count("direct_play", "cancelled"),
            0,
            "a resume is not an abandoned start"
        );
        assert_eq!(ledger.count("direct_play", "ok"), 1);
        assert_eq!(ledger.phase(1, 10), Some(StartPhase::Playing));
        // Android: a seek right after the resume reports a `ttff` for the
        // seek. That picture is inside the play, not a second start.
        let again = resume + START_DEADLINE + PLAY_QUIET * 2;
        ledger.opened(1, 10, Some(100), "direct_play", again);
        ledger.client_event(
            1,
            10,
            "ttff",
            Some("direct_play"),
            Some("seek"),
            again + Duration::from_secs(20),
        );
        assert_eq!(
            ledger.count("direct_play", "ok"),
            1,
            "a seek's picture is not a start"
        );
        assert_eq!(ledger.phase(1, 10), Some(StartPhase::Playing));
        // The same title opened again later is a start, and is counted...
        let reopened = again + PLAY_QUIET * 2;
        ledger.opened(1, 10, Some(100), "remux", reopened);
        ledger.client_event(
            1,
            10,
            "ttff",
            Some("remux"),
            Some("resume"),
            reopened + Duration::from_secs(2),
        );
        assert_eq!(ledger.count("remux", "ok"), 1);
        // ...and so is one that fails before its picture.
        let failing = reopened + PLAY_QUIET * 2;
        ledger.opened(1, 10, Some(100), "direct_play", failing);
        ledger.client_event(
            1,
            10,
            "playback_error",
            None,
            None,
            failing + Duration::from_secs(3),
        );
        assert_eq!(ledger.count("direct_play", "failed"), 1);
    }

    /// #559 review, finding 1: the server's own "not yet, ask again" answers
    /// are not refused starts. Three `transcode_capacity_pending` 503s on the
    /// clients' 1 s / 2 s / 4 s ladder, then the create that plays, are one
    /// `ok` and no `refused`.
    #[test]
    fn a_not_yet_refusal_keeps_the_start_open_for_the_retry_that_plays() {
        let ledger = StartAttempts::new();
        let t0 = Instant::now();
        for (at, code) in [
            (0, "transcode_capacity_pending"),
            (1, "quality_catalog_unavailable"),
            (3, "transcode_capacity_pending"),
        ] {
            ledger.refused(1, 10, None, Some(code), t0 + Duration::from_secs(at));
            assert!(matches!(
                ledger.phase(1, 10),
                Some(StartPhase::Pending { not_yet: true, .. })
            ));
        }
        ledger.opened(1, 10, Some(100), "transcode", t0 + Duration::from_secs(7));
        ledger.client_event(
            1,
            10,
            "ttff",
            Some("transcode"),
            Some("cold-start"),
            t0 + Duration::from_secs(9),
        );
        assert_eq!(ledger.count("transcode", "ok"), 1);
        for method in ["unknown", "transcode", "remux", "direct_play"] {
            assert_eq!(ledger.count(method, "refused"), 0, "{method}");
        }
        // The web answers `vod_index_pending` on a copy create with a
        // progressive remux: one start, `ok`, under the method that played.
        ledger.refused(1, 11, None, Some("vod_index_pending"), t0);
        ledger.opened(1, 11, Some(101), "remux", t0 + Duration::from_secs(1));
        ledger.client_event(
            1,
            11,
            "ttff",
            None,
            Some("cold-start"),
            t0 + Duration::from_secs(3),
        );
        assert_eq!(ledger.count("remux", "ok"), 1);
        assert_eq!(ledger.count("unknown", "refused"), 0);
        // A client that stops asking after "not yet" was never served: one
        // `refused` at the deadline, not `cancelled`.
        ledger.refused(1, 12, None, Some("startup_timeout"), t0);
        ledger.refused(
            1,
            12,
            None,
            Some("startup_timeout"),
            t0 + Duration::from_secs(1),
        );
        // Served after a "not yet" and then abandoned before a picture: the
        // server did serve it, so it is `cancelled`, not `refused`.
        ledger.refused(1, 14, None, Some("transcode_capacity_pending"), t0);
        ledger.opened(1, 14, Some(104), "transcode", t0 + Duration::from_secs(2));
        ledger.sweep(t0 + START_DEADLINE);
        assert_eq!(ledger.count("unknown", "refused"), 1);
        assert_eq!(ledger.count("unknown", "cancelled"), 0);
        assert_eq!(ledger.count("transcode", "cancelled"), 1);
        assert_eq!(ledger.count("transcode", "refused"), 0);
        // A refusal that is not a "not yet" still ends its attempt at once.
        ledger.refused(1, 13, None, Some("vod_disabled"), t0);
        assert!(matches!(
            ledger.phase(1, 13),
            Some(StartPhase::Closed { .. })
        ));
        assert_eq!(ledger.count("unknown", "refused"), 2);
    }

    /// The not-yet codes are the contract's `create_503_not_yet` row, the
    /// codes every client retries, and nothing else.
    #[test]
    fn start_not_yet_codes_are_the_contract_row_the_clients_retry() {
        let contract: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/playback/playback-surface-contract.json"
        ))
        .expect("the surface contract parses");
        let row = contract["sources"]
            .as_array()
            .expect("sources")
            .iter()
            .find(|source| source["id"] == "create_503_not_yet")
            .expect("the create_503_not_yet row");
        let mut contract_codes: Vec<&str> = row["codes"]
            .as_array()
            .expect("codes")
            .iter()
            .map(|code| code.as_str().expect("a code is a string"))
            .collect();
        let mut ours = START_NOT_YET_CODES.to_vec();
        contract_codes.sort_unstable();
        ours.sort_unstable();
        assert_eq!(ours, contract_codes);
    }

    /// §7.8.5, the `method` label (#559 review, finding 3): the method a
    /// first frame names wins over the opening request's, which is the
    /// fallback case the override exists for.
    #[test]
    fn a_first_frame_names_the_method_its_start_is_counted_under() {
        let ledger = StartAttempts::new();
        let t0 = Instant::now();
        ledger.opened(1, 10, Some(100), "direct_play", t0);
        ledger.client_event(
            1,
            10,
            "ttff",
            Some("transcode"),
            None,
            t0 + Duration::from_secs(2),
        );
        assert_eq!(ledger.count("transcode", "ok"), 1);
        assert_eq!(ledger.count("direct_play", "ok"), 0);
    }

    /// §7.8.5 (#559 review, finding 3): a request joining a pending attempt
    /// re-labels it, so a first frame that names no method, and a refusal
    /// that ends it, are counted under the method now in use.
    #[test]
    fn a_request_joining_a_pending_start_relabels_it() {
        let ledger = StartAttempts::new();
        let t0 = Instant::now();
        ledger.opened(1, 11, Some(101), "direct_play", t0);
        ledger.opened(1, 11, Some(101), "remux", t0 + Duration::from_secs(1));
        ledger.client_event(1, 11, "ttff", None, None, t0 + Duration::from_secs(2));
        assert_eq!(ledger.count("remux", "ok"), 1);
        assert_eq!(ledger.count("direct_play", "ok"), 0);
        ledger.opened(1, 12, Some(102), "direct_play", t0);
        ledger.opened(1, 12, Some(102), "transcode", t0 + Duration::from_secs(1));
        ledger.refused(1, 12, None, None, t0 + Duration::from_secs(2));
        assert_eq!(ledger.count("transcode", "refused"), 1);
        assert_eq!(ledger.count("direct_play", "refused"), 0);
    }

    /// §7.8.5 (#559 review, finding 3): a first frame on a refusal's
    /// tombstone pairs with no attempt. It is counted unpaired, never `ok`.
    #[test]
    fn a_first_frame_after_a_refusal_is_unpaired() {
        let ledger = StartAttempts::new();
        let t0 = Instant::now();
        ledger.opened(1, 12, Some(102), "transcode", t0);
        ledger.refused(1, 12, None, None, t0 + Duration::from_secs(1));
        ledger.client_event(
            1,
            12,
            "ttff",
            Some("transcode"),
            None,
            t0 + Duration::from_secs(3),
        );
        assert_eq!(ledger.count("transcode", "refused"), 1);
        assert_eq!(ledger.count("transcode", "ok"), 0);
        assert_eq!(ledger.count("unpaired", "ok"), 1);
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

/// G1: the per-create identity and record.
#[cfg(test)]
mod start_record_tests {
    use super::*;
    use serde_json::json;

    fn tag(attempt: &str, session: &str) -> AttemptTag {
        AttemptTag {
            attempt: attempt.to_owned(),
            session_id: session.to_owned(),
        }
    }

    /// Two devices of one viewer on one title are two creates. The ledger is
    /// keyed by (viewer, file) and so holds one entry for both; the reports
    /// are matched by the session each names, and each settles its own.
    #[test]
    fn two_devices_on_one_title_settle_their_own_creates() {
        let ledger = StartAttempts::new();
        let t0 = Instant::now();
        for create in [tag("claim-a", "sess-a"), tag("claim-b", "sess-b")] {
            ledger.tag_session(1, 10, &create, t0);
            ledger.opened_tagged(1, 10, Some(100), "transcode", Some(&create), t0);
        }
        let later = t0 + Duration::from_secs(2);
        assert_eq!(
            ledger.settle(1, 10, Some("sess-b"), "ttff", later),
            Some(SettledStart {
                attempt: "claim-b".to_owned(),
                outcome: SettledOutcome::Presented,
                session: Some("sess-b".to_owned()),
            }),
            "the second device's picture settles the second device's create"
        );
        assert_eq!(
            ledger.settle(1, 10, Some("sess-a"), "hls_fatal", later),
            Some(SettledStart {
                attempt: "claim-a".to_owned(),
                outcome: SettledOutcome::Failed,
                session: Some("sess-a".to_owned()),
            }),
            "and the first device's failure settles the first"
        );
        assert_eq!(
            ledger.settle(1, 10, Some("sess-a"), "ttff", later),
            None,
            "a create is settled once"
        );
        // Not a first frame or a start failure: nothing to settle.
        ledger.tag_session(1, 10, &tag("claim-c", "sess-c"), t0);
        assert_eq!(ledger.settle(1, 10, Some("sess-c"), "stall", later), None);
        // Another viewer or title cannot settle it by naming its session.
        assert_eq!(ledger.settle(2, 10, Some("sess-c"), "ttff", later), None);
        assert_eq!(ledger.settle(1, 11, Some("sess-c"), "ttff", later), None);
        assert!(ledger
            .settle(1, 10, Some("sess-c"), "ttff", later)
            .is_some());
    }

    /// The counterfactual, as the code answers it: a report that names no
    /// session can only be given the (viewer, file) entry's newest attempt,
    /// so with the old key two devices merge into one and the older create
    /// is never attributed. Naming the session is what separates them.
    #[test]
    fn a_report_that_names_no_session_gets_the_newest_create_only() {
        let ledger = StartAttempts::new();
        let t0 = Instant::now();
        for create in [tag("claim-a", "sess-a"), tag("claim-b", "sess-b")] {
            ledger.tag_session(1, 10, &create, t0);
            ledger.opened_tagged(1, 10, Some(100), "transcode", Some(&create), t0);
        }
        let settled = ledger.settle(1, 10, None, "ttff", t0).expect("newest");
        assert_eq!(settled.attempt, "claim-b");
        assert_eq!(ledger.settle(1, 10, None, "ttff", t0), None);
        assert_eq!(
            ledger
                .settle(1, 10, Some("sess-a"), "ttff", t0)
                .map(|s| s.attempt),
            Some("claim-a".to_owned()),
            "the older create was only reachable by its session"
        );
    }

    #[test]
    fn a_create_nobody_reports_on_is_forgotten_after_the_start_deadline() {
        let ledger = StartAttempts::new();
        let t0 = Instant::now();
        ledger.tag_session(1, 10, &tag("claim-a", "sess-a"), t0);
        assert_eq!(
            ledger.settle(1, 10, Some("sess-a"), "ttff", t0 + START_DEADLINE),
            None
        );
    }

    #[test]
    fn a_create_reports_its_attempt_and_owner_out_of_a_timed_out_future() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        let (result, note) = runtime.block_on(with_create_note(async {
            note_create_attempt("claim-a");
            note_create_owner("node-b");
            // A future the caller's timeout drops mid-flight has still said
            // what it reached.
            tokio::time::timeout(Duration::from_millis(5), std::future::pending::<()>()).await
        }));
        assert!(result.is_err());
        assert_eq!(note.attempt(), Some("claim-a"));
        assert_eq!(note.owner_node(), Some("node-b"));
        // Outside a scope the notes are no-ops, not panics.
        note_create_attempt("ignored");
    }

    #[test]
    fn phases_parse_into_rows_and_an_unreadable_one_is_skipped() {
        assert_eq!(
            parse_phases("plan:12ms/1a/2l/0w,claim:340ms/0a/1l/3w,garbled,tail:1ms/x/0l/0w"),
            vec![
                json!({"name": "plan", "ms": 12, "authority_reads": 1, "local_reads": 2, "mutations": 0}),
                json!({"name": "claim", "ms": 340, "authority_reads": 0, "local_reads": 1, "mutations": 3}),
            ]
        );
        assert!(parse_phases("").is_empty());
    }

    #[test]
    fn a_record_is_one_sidecar_row_with_the_cost_in_extra() {
        let event = StartAttemptRecord {
            at_unix_ms: 1_700_000_000_000,
            user_id: 7,
            file_id: 42,
            attempt: "claim-a".to_owned(),
            session_id: Some("session-a".to_owned()),
            route: Some("rolling"),
            refusal: Some("startup_timeout".to_owned()),
            refusal_message: Some("exceeded".to_owned()),
            client_build: "web/Chrome\nforged".to_owned(),
            owner_node: Some("node-b".to_owned()),
            phases: "plan:12ms/1a/2l/0w".to_owned(),
            authority_reads: 1,
            local_reads: 2,
            mutations: 3,
            store_ms: 4,
            elapsed_ms: 5,
        }
        .into_event();
        assert_eq!(event.event, "start_attempt");
        assert_eq!(event.attempt.as_deref(), Some("claim-a"));
        assert_eq!(event.reason.as_deref(), Some("startup_timeout"));
        assert_eq!(event.method.as_deref(), Some("rolling"));
        assert_eq!(
            event.session_id,
            Some(crate::transcode::session_log_id("session-a"))
        );
        let extra: serde_json::Value =
            serde_json::from_str(event.extra.as_deref().expect("extra")).expect("JSON");
        assert_eq!(extra["owner_node"], "node-b");
        assert_eq!(
            extra["client_build"], "web/Chromeforged",
            "control characters dropped"
        );
        assert_eq!(extra["phases"][0]["ms"], 12);
        assert_eq!(
            [
                extra["authority_reads"].as_u64(),
                extra["local_reads"].as_u64(),
                extra["mutations"].as_u64(),
                extra["store_ms"].as_u64(),
                extra["elapsed_ms"].as_u64()
            ],
            [Some(1), Some(2), Some(3), Some(4), Some(5)]
        );
    }
}
