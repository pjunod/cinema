# Apple TV interruptions on a 79.5 GB remux: why they happened and what fixes them

**Status:** findings 3, 4, 5 and 6 fixed (fence grace, resumable attestation,
rolling deferral, the same grace for progressive and Live TV playback);
findings 1 and 2 are deploy-procedure recommendations. **Written:** 2026-10-04 EDT. **Incident build:**
`aa3d77101` (PR #788) on all three voters. **Title:** *Bad Boys: Ride or Die*
(2024), catalog file 5208, a 79.5 GB 2160p HEVC remux, played on the Apple TV
from media1.

Companion to [HEATED-RIVALRY-S1E5-QUORUM-RCA-AND-FIX.md](HEATED-RIVALRY-S1E5-QUORUM-RCA-AND-FIX.md),
which covers the same serving-fence coupling from a different trigger.

## 1. What the viewer saw

Between 04:00 and 04:21 UTC the stream stopped and reopened five times. Each
time the Apple client raised its surface, logged `avplayer_item_failed`
(`-1008`, then `-1004`), and reopened at the last position. After the fifth
interruption the viewer gave up and played something else.

| UTC | What ended the session | Class |
|---|---|---|
| 04:01:17 | media1's `plurxd` restarted by the deploy of `aa3d77101` | deploy |
| 04:06 | lab6 restarted by the same deploy | deploy |
| 04:12:58 | serving authority expired on media1 because the leader (lab4) took 1,419 ms on one apply while it compiled the new build | quorum fence |
| 04:14:32 | the deploy restarted the Raft leader (lab4 started at 04:14:32.83) | quorum fence |
| 04:20:16 | `rolling_window_budget_exhausted` two seconds after the viewer resumed from pause | rolling publication |

Underneath all five: every reopen took the **temporary live-HLS recovery** path
(`vod_index_pending`) because file 5208 had no VOD index, and the index could
not be built (finding 4).

## 2. Findings

### Finding 1 — a deploy restarts every voter while playback runs

The rolling deploy restarted media1, then lab6, then lab4. The node serving
the viewer was restarted first, which ends its sessions by definition. That
part is expected. The two findings below are the parts that are not.

### Finding 2 — the deploy compiles on, then restarts, the Raft leader

- **04:12:58.78.** lab6 logs `slow database state-machine apply … elapsed_ms=1419`
  for raft index 27350916. At that moment lab4, the leader (`leader_id: Some(6)`),
  was building the new image on its own host: its five-minute load average was
  4.94. Followers forward `db_quorum_watermark` to the leader, so one slow apply
  there fenced media1 and lab6 at the same moment.
- **04:14:32.32.** media1's quorum round trip fails with `raft confirmation failed`.
  lab4's container `StartedAt` is 04:14:32.83. The deploy restarted the leader
  without first transferring leadership.

**Recommendation.** In `media/deploy.yml`, build images off the voters, or at
least never on the current leader, and transfer leadership away from a voter
before its restart. Run the leader last. This is ansible work, outside this
repository.

### Finding 3 — a sub-second fence retires every session, even after authority returns (FIXED)

At 04:12:58.828 media1 logs `serving authority expired; mutable media is
self-fenced`. Authority returns at **04:12:59.063**, 234 ms later. The
viewer's session is retired at **04:12:59.111**, 48 ms *after* recovery
(`transcode session self-fenced after quorum loss`).
`stop_all_sessions_for_serving_fence`
(`transcode/manager/control.rs`) projects the fence onto every session and then
retires them. It does not ask again whether authority came back before the
teardown finished.

Why a leader restart reaches other nodes at all: each node's serving
authority is a one-second lease on the quorum watermark, and followers obtain
the watermark through the leader. While the leader restarts there is no leader
until an election finishes (1.7 s here), so every voter loses authority at the
same moment, and the fence loop retired every session on every node.

**Fix.** `serving_fence_loop` still closes the registration gate on any loss,
but retires existing sessions only once authority has been lost for longer than
`SERVING_FENCE_SESSION_GRACE` (5 s) in one outage. Losses that return within a
grace of the last recovery share the same budget, so a flapping quorum still
retires. Keeping a session through a brief loss is safe:

- Nothing it holds is served while fenced. The router answers
  `serving_fenced` for media paths, and every response admits against the
  generation current at the time.
- Another node can claim a session only after its 12 s replicated lease has
  expired. Renewals run every 3 s, so in normal operation more than the grace
  is left when authority is lost. That holds even when only this node lost it
  and the rest of the cluster keeps quorum.
- If a claim did commit, the kept session still could not serve it. After
  recovery, a request is served locally only while the replicated route names
  this node (read through a one-second cache), and the lease loop reaps a
  session whose lease is gone. So the "ownership epoch has not moved"
  condition from the earlier ruling question is enforced by route resolution
  and the lease loop, not re-checked at recovery.
- In the incident window media1 logged no lease-loop reaps; both deaths came
  from the fence loop alone.

During the outage itself, requests on the session's media paths are still
answered 503. Serving reads from existing sessions through the outage is a
possible follow-up.

Regressions in `transcode/tests/chunk_06.rs`:

- `serving_fence_keeps_sessions_through_a_brief_loss`
- `serving_fence_flapping_losses_share_one_grace`
- `serving_fence_retires_sessions_after_a_sustained_loss_and_refuses_late_children`

### Finding 4 — a large HEVC source is never attested, so it never leaves rolling HLS (FIXED)

The VOD path for HEVC copy needs a whole-file SHA-256 attestation before it
will build an index (`attest_copy_source`). At the ~105 MB/s the NAS mount
delivers (measured with `dd` on media1), 79.5 GB takes **12.6 minutes**.

A background attestation is preempted whenever its node starts playback that is
not waiting on it (`foreground_preempted`). It is allowed to continue while a
viewer of *that* file is attached. Each preemption drops the attempt future,
and **the next attempt started again at byte 0**:

- request `a1242166…` (target media1): **53 attempts**, every one ending
  `foreground_preempted`;
- attempt 51 ran 04:08:02 → 04:13:04 and attempt 53 ran 04:15:27 → 04:20:23.
  Each ran for about five minutes, the length of the viewer's session, and was
  dropped when that session ended;
- across the cluster, 5,113 `retry/foreground_preempted` lifecycle events are
  recorded, and 20 requests sit `queued` with that code.

So the file the viewer was watching could never be prepared. Every interruption
also restarted the preparation that would have stopped the interruptions.

**Fix.** `full_source_digest` now records a checkpoint after every 64 MiB: the
offset and a copy of the SHA-256 state. It keeps the checkpoint in a bounded
in-memory table keyed by node, file, full-regime `object_version` and size. The
next attempt resumes from the checkpoint only when its own `object_version`,
taken when it opens the file, is identical. Every byte is still hashed in order
into one state. A rewrite changes `object_version` (inode, size, mtime and
ctime to the nanosecond), so the checkpoint is refused and the read starts over.
That is the same identity check an uninterrupted read relies on between its
first and last byte. Five-minute sessions now add up: three of them cover
79.5 GB. A daemon restart clears the table.

Three details follow from the review:

- A transient read error keeps the checkpoint.
- A finished digest is kept as a checkpoint at end of file. If playback
  preempts the caller between the end of the hash and recording its result,
  the retry re-checks identity and costs no read.
- The activity page's rate and ETA now count only bytes read since the
  attempt's first report. Before, a resumed attempt that reported 60 GB in its
  first second showed an ETA of seconds for minutes of work.

Regressions, in `crates/plurxd/src/fragment_index_cluster.rs`:

- `a_preempted_full_attestation_resumes_and_matches_an_uninterrupted_read`
- `an_attempt_preempted_before_its_first_checkpoint_keeps_the_earlier_one`
- `a_checkpoint_is_refused_once_the_source_identity_moves`
- `the_checkpoint_table_is_bounded_and_keeps_one_entry_per_file`
- `crates/plurxd/src/state.rs`: `a_resumed_attempt_is_timed_from_its_resume_point`

### Finding 5 — any pause longer than about a minute retired a rolling session (FIXED)

- 04:19:49: the producer held at `ahead_seconds=80` (`hold_reason=Demand`).
- 04:20:14.40: the client logged `resume started: buffered-immediate`, with
  62.4 s loaded.
- 04:20:14.57: hold authority changed from Demand to Time.
- 04:20:16.02: `rolling publication clock retired an invalid presentation …
  rolling_window_budget_exhausted: next completed segment exceeds the active
  publication safety floor`.

That message comes from `transcode/rolling/session.rs`. It fires when an
explicit demand is present and no new completed segment ends within
`ROLLING_RESERVE_MAX_MS` (180 − 10 − 16 − 30 = **124 s**) of `consumed_end_ms`.
The deployed build logs no publication budget at INFO, so the values at the
moment of retirement are not recoverable. The likely shape is that
`consumed_end_ms` on resume sat more than 124 s behind the next completed
segment. A 2160p remux running at ~2× while paused could get there.

**Mechanism (reproduced in a test).** While a viewer is paused, the
publication clock keeps publishing one segment per 16 s cycle. That continues
until the lead over the frozen position reaches the 124 s reserve ceiling,
about a minute into the pause. The next segment then fails the safety floor,
and that branch retired the session: deterministically, paused or not. The
resume at 04:20:14 played no part.

**Fix.** Publishing nothing never moves the served window, so the protected
segment stays served. Publication now waits for the viewer instead of retiring
it in three cases:

- the viewer is not consuming (paused, waiting or seeking);
- it is consuming at 1x or faster;
- a slower viewer's wait still fits the hard deadline measured from the last
  publication.

Every deferral episode is bounded by `ROLLING_PAUSE_GRACE` (180 s). Past that,
the session is retired with `deferred_for_ms` in its reason. Only a slow viewer
who would wait past the hard deadline is retired immediately, which is the case
the guard exists for.

**Open, needs devices.** At the ceiling the live playlist stops changing for as
long as the deferral lasts. RFC 8216 expects a live playlist to change within
1.5× target duration. AVPlayer can report -12888, and Media3 raises
`PlaylistStuckException` after 3.5× (56 s). If a client does that while paused,
it reopens, which is what the retirement forced before, so the change is never
worse. Whether pauses longer than a minute now resume cleanly on Apple TV and
Android still has to be checked on physical devices.

Regressions in `transcode/tests/chunk_03.rs`:

- `rolling_publication_budget_pause_then_resume_at_one_x_is_not_retired`
- `rolling_publication_budget_wedged_waiting_viewer_is_retired_after_the_pause_grace`
- `rolling_publication_budget_low_rate_retires_before_the_window_can_skip`
  (unchanged, still retires)

### Finding 6 — progressive `stream.mp4` and Live TV had the same coupling (FIXED)

Finding 3's fix covered the rolling-session registry only. Two other owners
of running playback compared the generation they were admitted under with the
fence's current one, and generations only rise, so any loss ended them for
good, even after authority had returned within milliseconds:

- **Progressive remux** (`http/stream.rs`, since `025c65bb7`, 2026-08-22).
  The child owner broke on `authority_lost_since(admitted_generation)`,
  killed ffmpeg and ended the response body. A progressive remux never
  respawns, so every `stream.mp4` play on every node stopped on a leader
  restart. A unit test pinned the behaviour ("publish recovery before the
  owner gets a scheduling point ... must still make the owner reap the
  child").
- **Live TV on its owner node** (`live_tv.rs`, `live_tv/dvr.rs`,
  `live_tv/resource.rs`). The session observer polled `is_current` every
  25 ms, the tuner fan-out checked it per chunk, and the per-second session
  fence, the transport lease loop and every playlist/segment request
  (answering 410) checked it too. A leader restart drops the owner's fence
  like every voter's, so every Live TV session, shared tuner transport and
  recording on the owner ended. A follower's own blip ended nothing: its
  ingress only checks `is_ready` and answers 503 meanwhile.

`live_tv.rs`'s guide refresh loop also subscribes to the fence; on a loss it
skips one refresh and retries, which is harmless.

**Fix.** One policy for every owner of running work.
`serving_fence::SessionGrace` is #798's outage budget, moved beside the fence
with `SERVING_FENCE_SESSION_GRACE`; the rolling registry's loop now calls it
unchanged.

- The progressive remux owner resolves a loss through it: a loss that
  recovers inside the grace keeps ffmpeg and the response, and the owner
  adopts the current generation; one that outlasts it ends the stream as
  before. The body publishes only while authority is held (ffmpeg blocks on
  its pipe meanwhile) and learns of an expiry through the owner's `fenced`
  token, which a natural exit never fires.
- Live TV holds a `LiveTvAuthority`. Admissions and commits still use the
  exact generation. Running work asks `running(generation)`, which stays true
  until a loss outlasts the grace; then the authority's one loop raises a
  running floor to the loss generation and everything admitted before it stops
  at its next check. While authority is lost, requests answer a retryable
  `serving_fenced` 503 instead of 410, and a joiner can still join a transport
  that survived. Keeping tuner transports and recordings writing through the
  grace is safe because single ownership lives in the replicated resource
  ledger: a replacement owner can claim a capture or an ingest only after this
  owner's 30 s lease (renewed every 5 s) has lapsed, so at least twenty
  seconds of lease remain when the grace runs out.

A node that has truly lost authority still stops serving within one grace on
every path.

Not verified here: whether the Live TV ledger's renewal and snapshot Store
calls ride out an election (the Store's own quorum-recovery budget suggests
they do); the leader-restart device check covers it.

Regressions:

- `http/stream.rs`: `remux_owner_keeps_the_child_through_a_brief_loss`,
  `remux_owner_reaps_the_child_after_a_sustained_loss`,
  `remux_owner_losses_after_a_recovery_share_one_grace`,
  `remux_owner_adopts_a_new_generation_when_authority_is_already_back`,
  `remux_owner_reaps_the_child_when_the_fence_closes_during_a_loss`,
  `remux_body_publishes_nothing_while_authority_is_lost`
- `live_tv.rs`: `live_tv_running_work_survives_a_brief_serving_loss`,
  `live_tv_running_work_ends_after_a_sustained_serving_loss`,
  `live_tv_losses_after_a_recovery_share_one_grace`,
  `live_tv_running_work_survives_a_generation_bump_with_authority_back`,
  `live_tv_running_work_ends_when_the_fence_closes_during_a_loss`,
  `a_loss_inside_the_grace_keeps_the_session_and_refuses_requests_retryably`
- `live_tv/dvr.rs`: `a_transport_keeps_writing_through_a_loss_inside_the_grace`

## 3. Evidence sources

- media1 `docker logs plurxd` from 04:01:17 (container start) to 04:21. lab6
  and lab4 logs cover the same window.
- A read-only copy of media1's replicated `plurx.db`: `analysis_requests`,
  `analysis_attempts` and `analysis_lifecycle_counters`.
- The NAS read rate: `dd bs=4M count=750` from the middle of the source on
  media1 gave 3.1 GB in 30.06 s, 105 MB/s.
