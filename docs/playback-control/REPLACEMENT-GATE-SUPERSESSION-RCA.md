# Abandoned replacements held their player's key — RCA and fix

**Status:** built — the supersedable gate landed on `main` in PR #437 · **Written:** 2026-09-21

**Reported** 2026-09-21 ~18:50 ET, Android client on the TCL tablet, playing
*Bad Boys: Ride or Die* (file 5208). The player showed

> transcode capacity is temporarily unavailable: another replacement for this
> player is still being committed

over Retry / Back, and Retry did not clear it.

**Verdict: orphaned.** Not a commit in flight, not a `PendingCandidateGuard`,
not a publication fence held until reconcile. The replacement gate for that
player was held by a *detached cleanup task belonging to an HTTP request that
had already answered the client six seconds earlier*, and nothing in the tree
ages, expires, or force-releases that registry.

---

## 1. What emits the string

`crates/plurxd/src/transcode.rs`, inside `acquire_cluster_replacement_gate`:

```rust
let gate_deadline = std::cmp::min(deadline, now() + CLUSTER_REPLACEMENT_GATE_WAIT);
let permit = tokio::time::timeout_at(gate_deadline, gate.lock_owned())
    .await
    .map_err(|_| capacity_error("another replacement for this player is still being committed"))?;
```

The gate was a plain `tokio::sync::Mutex<()>` in a process-local registry keyed
**only** on `(user_id, playback_id)` — `["[\"user_id\",1]","<playback_id>"]`.
`file_id`, height, track selection and `request_id` are not in the key, so an
ordinary first open, a stall reopen, an M6 prepared commit and a quality change
all contend for the same lock. `CLUSTER_REPLACEMENT_GATE_WAIT` is 3 s.

What it waits on is therefore not a *state* at all — it is whoever holds
`ClusterReplacementGuard` for that key. Every production holder is a detached
task:

| holder | anchor | reachable from the request? |
|---|---|---|
| `start_task` (local create) | `hls.rs:2401`, re-detached at `hls.rs:2452` | no |
| activation task | `hls.rs:2739`/`2755`, re-detached at `hls.rs:2826` | no — *"The HTTP deadline never cancels either ownership decision."* |
| armed handoff | `hls.rs:3612` | no; the request already returned `media_session_handoff_pending` |
| takeover supervisor | `media_sessions.rs:4938`, spawned at `5197` | there is no request |
| `Drop for StartedSessionGuard` cleanup | `hls.rs:481-521` | no |

`cluster_replacement_gates` appeared at exactly four lines in the workspace: the
field, its init, the acquire, and the guard's back-reference. **There was no
sweeper, reaper, TTL, generation or reconcile pass over it.**
`entries.retain(… strong_count() > 0)` prunes only *unheld* map slots. The
release path bottoms out in `retire_session_until_with_cause(…, None, …)` →
`ticket.wait()` — an unbounded await inside an unbounded retry loop
(`transcode.rs:23640-23675`). A single wedged retirement converted that player's
key into a 3-second refusal for the lifetime of the process.

## 2. What actually happened, from the nodes

Owner was **m6** (`192.168.4.14`), build `v0.3.0-3135-g9deb58a2e`.
`docker logs plurxd`, times UTC:

```
22:54:56.892  decision: file 5208, user pjunod, method=Transcode,
              subtitle=2, subtitle_requires_burn_in=true, DV P8 → SDR
22:54:58.672  admitted a held source on its media facts   file_id=5208   ← inside create, gate HELD
22:54:58.850  extracting embedded text subtitle to the sidecar cache  file_id=5208 index=2
22:55:48.069  503 on POST /api/v1/files/5208/hls/…   latency=50679 ms     ← attempt 1 gives up
22:55:48.209  client playback_error … attempt=a1/resume
22:55:52.716  surface_cleared … by=user action=retry                      ← Paul presses Retry
22:55:58.209  session create failed: transcode capacity is temporarily unavailable:
              another replacement for this player is still being committed   file=5208
22:55:58.209  503 … latency=3883 ms                                       ← exactly the 3 s gate wait
22:56:28.137  surface_cleared … by=user action=close
22:57:47.894  503 … latency=50723 ms                       ← attempt 3 got the gate, timed out the same way
23:01:41.490  text subtitle sidecar cached  file_id=5208 index=2  elapsed_ms=402639
23:01:41.749  vod session attached …                                      ← everything works again
```

Three facts settle it.

1. **The gate outlived its request by ≥ 6 s.** Attempt 1 answered 503 at
   22:55:48. Attempt 2's POST landed at ~22:55:54.3 and timed out 3.88 s later
   — so at 22:55:57 the key was still held by attempt 1's *cleanup*, which is
   the `Drop for StartedSessionGuard` spawn, not by anything the viewer was
   waiting for. That is the reported overlay, and it is the orphan.
2. **What held it inside the create was an unbounded await under the gate.**
   `transcode.rs` awaits `subtitles::ensure_burn_file(…)` while building the
   encoder options, and that joins the single-flight sidecar extraction with no
   timeout. The extraction ran **402,639 ms**. Both 50.7 s attempts died on
   their own start budget while parked there; the third only succeeded once the
   extraction finally published at 23:01:41.
3. **This is not the M6 prepared-replacement path.** `/metrics` on all four
   nodes: `plurx_playback_preparation_staged_total{outcome="staged"} 0`,
   `plurx_playback_preparation_cancelled_total{reason=…} 0` for every reason,
   `observations_total{seam="replacement"} 0`. Nothing was being prepared or
   committed anywhere in the fleet. The word "committed" in the message is the
   gate's own vocabulary, not a preparation commit.

## 3. The fix

### 3.1 A key is reclaimed on evidence, never on a clock

The first version of this fix let an arrival take the key whenever the holder
had not yielded inside the 3 s cooperative window. Adversarial review killed it,
correctly: **the legitimate work under this gate takes tens of seconds** — this
very incident had two 50-second starts — so a timer short enough to unwedge a
player is short enough to destroy every healthy one. Worse, the client-side half
of this change makes the retry ladder re-post into that window, so a start that
was nearly built when attempt *n* was refused would have been destroyed by
attempt *n+1* three seconds later. It also fenced nothing in the case it existed
for, because session ids were published only after `create_session_inner`
returned and the wedge is *inside* that call.

What replaced it reclaims only what it can prove is reclaimable.

- **Abandoned.** A hold says so itself. `ClusterReplacementGuard::
  mark_abandoned()` is called by `Drop for StartedSessionGuard` — before it
  spawns, together with `publish_fenceable(&session_id)` — and by
  `TakeoverWorkerGuard::spawn_teardown`. Both are the shape that wedged this
  player: a guard that has outlived the request it belonged to and now lives
  inside an unbounded teardown. This is a fact the holder knows and a waiter
  cannot infer, which is exactly why the timer was the wrong instrument.
- **Past its ceiling.** `CLUSTER_REPLACEMENT_HOLD_CEILING` is 120 s — longer
  than the longest declared start budget plus the activation and settlement
  windows the key is legitimately held across. A hold past it has outlived every
  budget it asked for.
- **Anything else keeps its player.** A start still inside its budget is refused
  to the waiter, which waits the bounded refusal out and re-posts.

Reclaiming fences everything the hold has published, then installs a fresh gate
under the same registry key and retires the old one. Retirement and ownership
live under one lock (`GateState`), so "judge this hold and retire it" and "claim
this hold" are mutually exclusive: a reclaim that loses the race finds a fresh,
healthy holder and refuses, and a claimant that loses sees the retirement and
re-enters the registry. Without that, a start that won the lock a moment before
a reclaim became a second live owner of one player.

`plurx_playback_replacement_reclaimed_total{reason}` counts every handover.
`abandoned` is the design working; `hold_ceiling` is not — it means something
under the gate ran past every budget it declared, and it is the signal to go
find what.

### 3.2 The overlay

The wait *is* bounded now, so the honest answer to "say how long and auto-retry"
is to make the refusal legible to the retry ladder the clients already have.

`session_start_error` stopped answering this refusal with a bare
`{"error": "<sentence>"}`. It now returns

```
HTTP/1.1 503 Service Unavailable
Retry-After: 3
{"code":"transcode_capacity_pending","message":"transcode capacity is temporarily unavailable: waiting for this player's previous start: …"}
```

and `transcode_capacity_pending` joins the fixture's `create_503_not_yet` row.
That is the whole client change: Android, Apple and web each already carry a
1 s · 2 s · 4 s ladder inside an absolute 60 s deadline, gated on the server's
`code`. They refused to use it here for a correct reason, written down in three
places — *"a bodiless 503 is not a 'still building' answer — it is a 503 nobody
explained — and retrying one would be guessing."* Now it is explained.

What the viewer sees instead of the raw sentence and a dead Retry button:
Android and web raise `preparing` ("Still preparing this stream…") and re-post
on the ladder; Apple, whose parser requires `code` and had been showing
`"Server returned 503"`, gets both fields for the first time.

**The new code is deliberately narrower than the capacity class.** Review found
two siblings in that class where auto-retrying is actively wrong: *"the proved
4K HDR10 QuickSync slot is busy; retry at 1080p"* instructs the client to change
the request, and a scratch ceiling above the configured budget is a
misconfiguration no retry can satisfy — and each re-post re-enters the admission
poll loop, so retrying would multiply load against the exact resource that is
exhausted. Those keep the codeless 503 that nothing retries.
`is_replacement_wait_error` is a strict subset of `is_retryable_capacity_error`,
so every existing server-side consumer of the capacity class is unchanged.

## 4. Left open

- **`ensure_burn_file` is still awaited under the gate** with no timeout, so a
  slow sidecar extraction still burns a whole start budget — it just can no
  longer wedge the player past the ceiling. The 402 s extraction for file 5208
  is its own defect: a per-file, already single-flighted, shared artifact being
  waited on inside a per-player exclusive lock. Worth moving ahead of the gate
  or bounding against the start deadline.
- **`Drop for StartedSessionGuard` still releases the guard only after a full
  retirement**, not after the fence. Marking the hold abandoned makes that
  survivable — the next open reclaims rather than queues — but releasing at the
  fence would make the handover free rather than merely bounded.
- **A hold wedged before it registers anything has nothing to fence.** The
  ceiling reclaim then moves the key with an empty fence list, and if that hold
  later registers, only the durable activation CAS separates the two workers.
  The robust answer is a fenced-id set on the registry that `register_session`
  refuses against, rather than `fence_sessions`' snapshot of a map the worker
  has not been inserted into yet.
- **Not deployed**, and neither client half has run on hardware.
