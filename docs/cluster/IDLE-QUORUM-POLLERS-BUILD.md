# Idle quorum pollers — loops that ask the leader whether nothing has changed

**Status:** open — ready to build · **Executes:** A4 of the cluster layer audit
(`CLUSTER-LAYER-AUDIT-2026-11-14.md` in this folder once PR #1604 lands) ·
**Design:** Claude (Fable 5.2), session `session_01BCWLA3JD1HCNMEid3AZrc3` ·
**Verified against:** `main @ 8785492be` · **Written:** 2026-11-15

> **Who reads this.** The build agent (Claude, GPT or OpenRouter). Work §4 one
> loop at a time, one commit each, and record each in §6. If a loop turns out
> to need its quorum read for correctness, leave it, and say why in §6.

## 1. Objective

Several background loops issue a linearizable read on every tick to learn that
nothing changed. A linearizable read is a confirmation round to every voter,
forwarded to the leader from a follower. Read from source, these loops are the
whole idle floor of the cluster: about 22 quorum reads per second across four
nodes with no work to do.

After this change an idle loop tick costs local reads only. A quorum read
happens when the local replica shows there is something to act on, and the
action itself still goes through the authority it goes through today.

Acceptance: on an idle four-node cluster, the loops in §2 together issue
fewer than two quorum reads per second (the fence sampler is not in scope).

## 2. The loops

Re-verify each at build time; line numbers are for `8785492be`.

| Loop | Where | Period | Quorum read per tick |
|---|---|---|---|
| Offline source probe | `plurx-core/src/cluster/membership.rs` `offline_source_probe_loop` → `answer_offline_source_probes` → `pending_offline_source_probes` (`store/hiqlite_durable.rs:2987`) | 500 ms, every node | 1 |
| Playback cache ceiling | `plurxd/src/playback_cache.rs` settings task (`SAMPLE_AGE` 1 s) → `get_setting(CACHE_MAX_GB)` | 1 s while the cache is in use | 1 |
| Media session lease loop | `plurxd/src/media_sessions.rs` `lease_loop` → `owned_media_sessions` (`store/hiqlite_sessions.rs`) | 3 s, every node | 1, even with no sessions |
| Sharing run and claim loops | `plurxd/src/sharing.rs` (`enabled()`, `LISTENER_IDLE_POLL` 5 s) | 5 s, every node, sharing on or off | 2 |
| Heartbeat's sharing switch | `membership.rs` heartbeat → `get_setting("sharing_enabled")` | 10 s, every node | 1 |
| Background work lanes | `plurxd/src/state.rs` `background_work_loop` and the lanes it drives | 5–25 s idle | 2–5 settings reads |
| Subtitle worker passes | `plurxd/src/subtitle_transcription.rs` `worker_pass` | 15 s | about 3 settings reads |
| Live TV loops | `plurxd/src/live_tv/dvr.rs` `dvr_loop`, `reminder_loop`, `dvr_library_loop`; `http/live_tv.rs` guide replica loop | 15–60 s, every node | 1 `settings_snapshot` each |

## 3. Design

Two existing mechanisms cover every row. Use them; add no third.

1. **Settings come from the applied settings view.** `AppliedView::setting`,
   `settings` and `settings_snapshot` (`plurx-core/src/store/applied_view.rs`)
   answer from this node's replica while the serving proof holds and the
   settings floor is met, and from the authority otherwise. The floor is
   raised by every settings writer on this node, so a node reads its own save
   back correctly. A loop that reads a switch or a tunable to decide what to
   do next reads it here. Every call site added must be registered in
   `plurxd/src/http/applied_settings_inventory.rs`, which pins the callers.
   The reason this is safe for these loops and not for everything: each of
   them already tolerates acting one period late, and a view answer is stale
   by at most the proof lease plus apply lag, which is shorter than any of
   their periods.
2. **"Is there work?" comes from a local hint, and the authority is asked
   only on a hit.** The store already has this shape: a `local_` twin of the
   read, used to decide whether the authoritative read is owed
   (`offline_claim.rs`, `watched_drain.rs`, `classification_schedule.rs`;
   `clock_offset.rs` `enforcement_hint`). For the offline probe loop and the
   lease loop: add a local read of the same rows; when it is empty, the tick
   is done; when it is not, run today's authoritative read and today's action
   unchanged. Unlike the older hint users, do not add a forced periodic
   authoritative read: a replica that never applies a row cannot act on it
   either, and the lease loop's renewal of sessions this process started is
   driven by the process's own registry (verify this; if the lease loop is
   the only thing that discovers a session this node owns, stop and report).

What does not change: periods, the actions, what is written, and every read
that precedes a write or decides a fence.

## 4. Milestones

One commit per loop, in the table's order. For each:

- Convert the read as §3 says.
- Add or extend a test that drives one idle tick and asserts zero authority
  reads, and one tick with work present and asserts the action still happens.
  The store's class counters or `HttpStoreOperationCounts` give the count; a
  recording test double is fine where the loop already takes a trait.
- Show the test fails with the old read restored.

Then: `cargo fmt --check`, `cargo clippy -p plurx-core -p plurxd --all-targets
-- -D warnings`, the touched modules' tests, the consistent-read and
placeholder census tests (a removed consistent-read site lowers a pinned
count; lower it), and one adversarial review of the whole diff by the owner.

## 5. Non-goals

- **No new timers, wakes or change-feed plumbing.** The general change feed is
  the audit's B4. This is the cheap half: stop paying a quorum round to hear
  "nothing".
- **Do not touch the fence sampler, the peer authorizer, or any read before a
  write.** They belong to other items.
- **Do not change the sharing listener's failure handling.** PR #1598 owns
  it. Only the idle `enabled()` reads move to the settings view; if #1598 has
  landed and already did that, skip the row and say so.
- **No settings or flags.**

## 6. Execution log

| Date | Loop | Agent-Model | Agent-Session | Result |
|---|---|---|---|---|
| 2026-11-15 | Design | claude-fable-5-2 | session_01BCWLA3JD1HCNMEid3AZrc3 | this document |
