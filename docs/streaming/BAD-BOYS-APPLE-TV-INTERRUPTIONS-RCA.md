# Apple TV interruptions on a 79.5 GB remux: why they happened and what fixes them

**Status:** finding 4 fixed (resumable whole-file attestation). Findings 1–3 are
open, with recommendations. **Written:** 2026-10-04 EDT. **Incident build:**
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

### Finding 3 — a sub-second fence retires every session, even after authority returns

At 04:12:58.828 media1 logs `serving authority expired; mutable media is
self-fenced`. Authority returns at **04:12:59.063**, 234 ms later. The
viewer's session is retired at **04:12:59.111**, 48 ms *after* recovery
(`transcode session self-fenced after quorum loss`).
`stop_all_sessions_for_serving_fence`
(`transcode/manager/control.rs`) projects the fence onto every session and then
retires them. It does not ask again whether authority came back before the
teardown finished.

This is deliberate: a node that lost authority may have lost ownership of its
sessions. It is also why any replicated write slower than ~500 ms on the leader
interrupts every viewer in the cluster. **Ruling for Paul:** keep the current
rule, or let a session survive when authority returns within a short grace and
its ownership epoch has not moved. The Heated Rivalry review warns that
removing the apply wait from the serving proof is the wrong way to do this; a
grace on retirement is a different change.

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

### Finding 5 — resuming from pause retired a rolling session (open, mechanism unconfirmed)

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

**Recommendation.** Make "no segment fits the floor" mean "nothing to publish
yet" while the client's runway is still healthy, and retire only when the
client is actually starving. Log `consumed_end_ms`, `desired_end_ms`,
`allowed_end_ms` and the first new segment's end on every retirement. Once
finding 4 lets the VOD index exist, this path no longer serves the title, but
it still serves every title that is waiting for one.

## 3. Evidence sources

- media1 `docker logs plurxd` from 04:01:17 (container start) to 04:21. lab6
  and lab4 logs cover the same window.
- A read-only copy of media1's replicated `plurx.db`: `analysis_requests`,
  `analysis_attempts` and `analysis_lifecycle_counters`.
- The NAS read rate: `dd bs=4M count=750` from the middle of the source on
  media1 gave 3.1 GB in 30.06 s, 105 MB/s.
