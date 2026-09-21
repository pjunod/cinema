# Abandoned replacements held their player's key — RCA and fix

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

`crates/plurxd/src/transcode.rs:19047` (pre-fix), inside
`acquire_cluster_replacement_gate`:

```rust
let gate_deadline = std::cmp::min(deadline, now() + CLUSTER_REPLACEMENT_GATE_WAIT);
let permit = tokio::time::timeout_at(gate_deadline, gate.lock_owned())
    .await
    .map_err(|_| capacity_error("another replacement for this player is still being committed"))?;
```

The gate is a plain `tokio::sync::Mutex<()>` in a process-local registry keyed
**only** on `(user_id, playback_id)` — `["[\"user_id\",1]","<playback_id>"]`.
`file_id`, height, track selection and `request_id` are not in the key, so an
ordinary first open, a stall reopen, an M6 prepared commit and a quality change
all contend for the same lock. `CLUSTER_REPLACEMENT_GATE_WAIT` is 3 s.

What it is waiting on is therefore not a *state* at all — it is whoever holds
`ClusterReplacementGuard` for that key. Every production holder is a detached
task:

| holder | anchor | reachable from the request? |
|---|---|---|
| `start_task` (local create) | `hls.rs:2401`, re-detached at `hls.rs:2452` | no |
| activation task | `hls.rs:2739`/`2755`, re-detached at `hls.rs:2826` | no — *"The HTTP deadline never cancels either ownership decision."* |
| armed handoff | `hls.rs:3612` | no; the request already returned `media_session_handoff_pending` |
| takeover supervisor | `media_sessions.rs:4938`, spawned at `5197` | there is no request |
| `Drop for StartedSessionGuard` cleanup | `hls.rs:481-521` | no |

`cluster_replacement_gates` appears at exactly four lines in the workspace: the
field, its init, the acquire, and the guard's back-reference. **There is no
sweeper, reaper, TTL, generation or reconcile pass over it.** `entries.retain(…
strong_count() > 0)` prunes only *unheld* map slots. The release path bottoms
out in `retire_session_until_with_cause(…, None, …)` →
`ticket.wait()` — an unbounded await inside an unbounded retry loop
(`transcode.rs:23640-23675`). A single wedged retirement converts that player's
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
   `transcode.rs:19472` awaits `subtitles::ensure_burn_file(…)`, which joins the
   single-flight sidecar extraction with no timeout. That extraction ran
   **402,639 ms**. Both 50.7 s attempts died on their own start budget while
   parked there; the third only succeeded once the extraction finally published
   at 23:01:41.
3. **This is not the M6 prepared-replacement path.** `/metrics` on all four
   nodes: `plurx_playback_preparation_staged_total{outcome="staged"} 0`,
   `plurx_playback_preparation_cancelled_total{reason=…} 0` for every reason,
   `observations_total{seam="replacement"} 0`. Nothing was being prepared or
   committed anywhere in the fleet. The word "committed" in the message is the
   gate's own vocabulary, not a preparation commit.

## 3. The fix

### 3.1 The gate is now supersedable

`ReplacementGate` replaces the bare mutex and carries an arrival ticket, the
newest arrival's ticket (`wanted`), and the session ids the current holder has
published.

- An arriving open **announces itself before waiting**, so a holder learns it
  lost the player while it still has work to abandon. `ClusterReplacementGuard::
  is_superseded()` is checked at the start checkpoint in
  `create_cluster_session_with_priority`, which refuses cheaply rather than
  spending an encoder slot on a start no viewer is waiting for.
- If the holder does not yield inside the cooperative 3 s window, the arrival
  **fences everything the holder published and takes the key**, installing a
  fresh gate under the same registry entry. The abandoned holder keeps its own
  `Arc` and its own lock; releasing it later touches nothing the key now uses.
- A start whose own budget expires before the cooperative window does neither
  announces nor evicts: it is out of time, not stuck behind a wedged holder, and
  telling a healthy replacement it lost the player on its behalf would end a
  replacement that is doing nothing wrong. It keeps getting the capacity
  refusal, which is what its own deadline earned.

**Why taking the key early is safe.** The gate serializes one player's
replacements; it is not what decides which worker wins. That is the durable
activation CAS, and the loser of it already tears itself down through
`StartedSessionGuard`. So the cost of moving the key is bounded to "two
provisional workers exist for a moment" — the state cluster make-before-break is
built for — while fencing the holder's published ids first means a registration
it completes afterwards is reaped rather than served. The cost of *not* moving
it is what the tablet saw.

Ids are published as soon as they exist: `creation.info.session_id` when
`create_session_inner` returns, and the predetermined
`start.provisional_session_id` at takeover, from the moment the gate is held.

`plurx_playback_replacement_superseded_total{outcome}` counts every handover,
split by whether anything had to be fenced. A non-zero `fenced` bucket is the
signal that a holder is wedging often enough to go find.

### 3.2 The overlay

The wait *is* bounded — at most the cooperative window, because the arrival now
takes the key rather than queueing. So the honest answer to "say how long and
auto-retry" is to make the refusal legible to the retry ladder every client
already has.

`session_start_error` stopped answering the retryable-capacity class with a bare
`{"error": "<sentence>"}`. It now returns

```
HTTP/1.1 503 Service Unavailable
Retry-After: 3
{"code":"transcode_capacity_pending","message":"transcode capacity is temporarily unavailable: …"}
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

The case that is **not** bounded gets its own class rather than borrowing
"temporarily": a start that lost its player to a newer open answers
`409 media_player_superseded`, deliberately outside the retry ladder. The
viewer is already looking at the player that won, and retrying would take the
key back from it.

## 4. Left open

- **`ensure_burn_file` is still awaited under the gate** (`transcode.rs:19472`)
  with no timeout, so a slow sidecar extraction still burns a whole start
  budget — it just can no longer wedge the player. The 402 s extraction for
  file 5208 is its own problem: a per-file, already single-flighted, shared
  artifact being waited on inside a per-player exclusive lock. Worth moving
  ahead of the gate or bounding against the start deadline.
- **`Drop for StartedSessionGuard` still releases the guard only after a full
  retirement**, not after the fence. The supersession makes that survivable
  rather than fatal; releasing at the fence would make the handover free.
- The `media_player_superseded` 409 falls through to each client's terminal row
  with the server's sentence. A dedicated surface row (row 12a in the contract)
  would let it be silent, which is what it should be.
