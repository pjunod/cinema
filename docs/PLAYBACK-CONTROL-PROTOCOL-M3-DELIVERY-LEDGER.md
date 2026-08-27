# Playback control M3c1 — attempt-fenced delivery ledger

This slice moves rolling publication and completed-fetch facts into the M3
actor without changing which producer recovery actions are selected. It also
orders the existing pre-publication fallback so actor admission must succeed
before that action can touch its predecessor. It follows the explicit
demand lease in
[`PLAYBACK-CONTROL-PROTOCOL-M3-DEMAND-LEASE.md`](PLAYBACK-CONTROL-PROTOCOL-M3-DEMAND-LEASE.md)
and is a prerequisite for the remaining producer progress/exit events and the
M4 watchdog deletion. Project-wide delivery state remains in
[`PLAYBACK-CONTROL-STATUS.md`](PLAYBACK-CONTROL-STATUS.md).

## Outcome

Each rolling session now has one actor-owned delivery snapshot for one exact
producer attempt:

- the current attempt number;
- whether a usable playlist is ready;
- the highest published segment and its playable end;
- the next media sequence;
- the highest completely fetched segment and its playable end; and
- a fetched segment whose bytes completed before its playlist duration was
  known.

Publication and fetch completion are observations only in this slice. The
actor records and exposes them, while the existing segment index and atomics
remain compatibility projections for pruning, byte accounting, legacy flow
control, and the old watchdogs. No process action moved and no watchdog was
removed here.

## Authority and event flow

The actor allocates every rolling producer attempt. Initial copy/transcode
starts and every allowed pre-publication fallback reset ffmpeg progress to
that same attempt before spawning the child. An attempt cannot be replaced
after its usable playlist has entered actor state.

Attempt admission and final installation return an exact rejection: the
session ended, a playlist was already published, the install attempt was
stale, the attempt counter was exhausted, or the actor mailbox was unavailable.
An admission rejection happens before fallback touches the predecessor and
therefore leaves its process, scratch, admission class, telemetry, and
compatibility frontiers unchanged. A final-install rejection happens after an
accepted transition may already have killed the predecessor and spawned a
candidate. It kills that candidate without publishing it; it does not pretend
to roll the predecessor back, and the terminal actor/ordinary teardown owns
the remaining cleanup.

```text
 producer start/replacement
          |
          v
 BeginProducerAttempt --------------------+
          |                               |
          v                               v
 ffmpeg/progress attempt          actor delivery snapshot
          |                               ^
          +-- playlist refresh -----------| ObservePublication
          |                               |
 HTTP object open captures attempt        |
          |                               |
 successful full response EOF ------------+ CommitMedia
```

`ObservePublication` is monotonic within an attempt. It may advance the
published segment, published end, and next sequence, but cannot move them
backward. A refresh made obsolete by replacement is rejected by attempt.

`CommitMedia` combines media lease renewal and completed-fetch mutation in one
actor command. A full object, a full-span range, or a valid `304` can advance
the fetched segment. A partial range renews demand but cannot claim the whole
segment. Missing, failed, abandoned, short, invalid, retired, superseded, and
stale-attempt responses do neither.

## Response ownership and replacement ordering

A rolling response owner captures both the session identity and the producer
attempt before opening the object. The open is discarded and retried if an
attempt replacement crosses it. Streamed EOF commits revalidate both the
registry identity and the captured attempt; therefore bytes from a predecessor
that finish after replacement cannot renew or advance its successor.

The actor commit and temporary compatibility frontier use a short,
attempt-tagged projection gate. It never waits for process or filesystem I/O.
If successor admission races after a predecessor EOF was accepted, either the
old projection lands first and the successor reset overwrites it, or the reset
lands first and the old projection is skipped. This preserves the M3b contract
that response EOF never waits behind `child_transition`.

The first-playlist compatibility flag uses that same attempt-tagged gate. A
predecessor request that proved its own startup cushion cannot reopen the flag
after successor reset; the successor must independently publish its required
cushion.

Playlist readiness becomes authoritative before a playlist response commits.
A pre-response fallback may win the compatibility child-transition gate after
the old playlist was read but before its actor observation. In that ordering
the observation is rejected and the response's captured attempt cannot renew,
so the old playlist is not returned as a live response. Once a usable playlist
observation is actor-owned, in-place attempt replacement is rejected.

## Fetch-before-publication ordering

A segment can complete while its current playlist has not yet exposed the
segment's `EXTINF`. The actor advances the fetched segment immediately, stores
it as pending, and leaves the fetched end unchanged. A later publication
observation may resolve only that exact pending segment. Newer fetches replace
older pending work because their cumulative end is sufficient; older fetches
cannot move the frontier backward.

This prevents an independently sampled segment-index lock and fetched atomic
from producing an impossible status pair. The actor snapshot is the source for
rolling `published_end_ms`, `fetched_segment`, and `fetched_end_ms` in live
status and joined playback evidence.

## Compatibility kept deliberately

| Existing mechanism | M3c1 treatment | Why it remains |
|---|---|---|
| `SegmentIndex` | Still updated, sized, and pruned | It owns the compatibility playlist catalog, byte totals, and retained files until actor catalog ownership lands. |
| `playlist_published` | Still gates the first usable response | The existing HTTP startup contract remains unchanged; its value is also projected into the actor. |
| `high_segment`, `fetched_end_ms` | Still projected after an accepted actor commit, behind an attempt-tagged synchronous gate | Legacy pacing, pruning, and compatibility tests still consume them. Successor admission resets them without blocking EOF on process I/O; status no longer samples them when the actor is available. |
| `child_transition` | Still orders old fallback/retirement paths | M4 removes it only after child start, exit, progress, signal, and retirement are actor actions. |
| first-segment/software grace and progress watcher | Unchanged | Progress and child exit become actor observations in M3c2; M4 then replaces competing recovery with one producer progress deadline. |
| playlist/segment wait budgets | Unchanged | These are bounded HTTP waits, not recovery owners. Their wakeup state moves to the actor later. |
| 15-second repair/flow tick | Unchanged | It still schedules index refresh, pruning, compatibility flow, metrics, and cleanup. |

Playlist parsing and file metadata reads happen outside `child_transition`.
Only the prepared in-memory merge crosses that gate after revalidating the
attempt. A slow NAS metadata operation can delay one observation, but cannot
block control, stop, replacement, or resume signaling.

Actor admission intentionally precedes predecessor teardown, so the current
attempt may already name the successor while predecessor files still exist.
The replacement marker fences playlist reads, segment opens, and index refresh
preparation throughout that interval. No observation can attribute those old
files to the new attempt and merge them after the transition reopens. Each
reader samples the compatibility attempt before the actor attempt and checks
the marker on both sides; a complete false→true→false replacement therefore
cannot masquerade as an unchanged path owner. If a replacement task is
cancelled after admission, its phase-aware guard leaves paths fenced, records
a terminal failure, and retires the actor for ordinary cleanup.

Concurrent refreshes within one attempt also capture the in-memory index
revision before reading the playlist bytes. If another refresh or retention
mutation lands first, the older preparation is discarded instead of borrowing
a newer token and overwriting the newer timeline.

Retention owns `child_transition` for at most 32 same-filesystem metadata
renames from served segment names to unique attempt/sweep garbage names, plus
the matching catalog mutation. Payload unlink runs detached after releasing
the transition and cannot stall another session's repair pass. Segment paths
are reused by a successor, so this bounded handoff is required: a replacement
that won first makes the retention sample stale, while one that arrives later
cannot seed successor bytes until no predecessor cleanup still names a served
path. Rename failures leave the source path and accounting intact; unlink
failures remain confined to unservable garbage names. M4 replaces these shared
paths with attempt-owned actor state.

A replacement is authorized twice: actor admission before the predecessor is
touched, and exact-attempt authorization after the candidate process is spawned
but before it is installed. The actor's exact producer/deadline fence then stays
held through the synchronous child assignment. Expiry, retirement, or a newer
attempt that wins before that fence kills the candidate and leaves it outside
session ownership.

Initial sessions use the equivalent publication rule. They take the potentially
delayed registry lock first, reauthorize the exact attempt, and hold the actor's
producer/deadline fence through the synchronous registry insertion. Serving
authority and actor ownership are both rechecked after every asynchronous wait;
an unregistered rejected child is killed and its admission resources returned.

Composite subtitle playlists carry the response owner resolved with their
exact video-playlist bytes. They never reconstruct ownership from a reusable
session id, whether the source was a rolling attempt or a same-id VOD
attachment.

Playlist readiness is committed to the actor from the exact validated bytes
being returned, before the response can renew. Index refresh may still reread
for byte accounting and pruning, but failure of that second read cannot leave
client-visible media outside actor ownership or admit a later in-place
fallback.

## Instrumentation

The actor snapshot now supplies rolling delivery frontiers to session status,
Activity detail, control responses, and joined server/client evidence. Those
surfaces expose the producer attempt, playlist-ready verdict, published
segment/end, next sequence, fetched segment/end, and any fetch whose timing is
still pending. Existing delivery byte/rate/error metrics remain response-path
measurements. Attempt fencing is covered by structured warnings when an old
fallback is rejected and by regression tests that hold an old response open
across replacement.

M3c2 will add coalesced producer-progress observations and exact-attempt child
exit observations. That slice must not block ffmpeg's stdout/stderr drain on
the actor mailbox. It still will not move recovery actions; M4 performs that
cutover only after the actor has all required facts.

## Verification contract

The slice is mergeable only after adversarial review precedes tests and the
final diff proves:

1. producer attempts are monotonic and actor allocated;
2. publication, fetch, and response ownership reject stale attempts;
3. a predecessor body completing after replacement cannot renew or mutate the
   successor;
4. publication and fetch frontiers never move backward;
5. fetch-before-`EXTINF` resolves only against its exact pending segment, and
   a later equal-segment commit with known timing resolves it too;
6. complete objects, full-span ranges, and `304` advance the frontier while
   partial, dropped, failed, and invalid responses do not;
7. retirement rejects every late publication and fetch mutation;
8. rolling status reads actor frontiers while compatibility flow/pruning stay
   behaviorally unchanged; and
9. the focused suites, full local gate, and every required hosted job pass.

Rollback removes the actor delivery fields and attempt fence while leaving the
merged M3b lease/demand actor intact. Because this slice does not remove or
change recovery actions, rollback does not require restoring a watchdog.
