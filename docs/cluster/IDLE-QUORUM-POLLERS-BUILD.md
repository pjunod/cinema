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
| 2026-11-15 | Offline source probe | claude-opus-5-5 | session_01BCWLA3JD1HCNMEid3AZrc3 | Converted (mechanism 2): `answer_offline_source_probes` reads `local_pending_offline_source_probes` (new `OfflinePackageStore` twin, the authoritative read's own statement, inert on SQLite) and returns on empty. Embedded voter: idle pass 0 authority reads; a probe addressed here takes `pending_offline_source_probes` once and is answered. Fails with the hint removed. |
| 2026-11-15 | Playback cache ceiling | claude-opus-5-5 | session_01BCWLA3JD1HCNMEid3AZrc3 | Converted (mechanism 1): `AppliedCeilingSource::read_max_gb` reads `AppliedView::setting`; `StoreCeilingSource` is gone. `playback_cache.rs` is a stays file, so the inventory gains `IDLE_LOOP_READS`, the only view reads a stays file may hold (owner to confirm: E1 §12-6 kept worker reads on quorum "until a measurement asks"; the audit is that measurement). |
| 2026-11-15 | Media session lease loop | claude-opus-5-5 | session_01BCWLA3JD1HCNMEid3AZrc3 | Converted (mechanism 2). Verified: renewal of what this process started is driven by its registry (`renewable_session_ids`, settlement-protected ids) and lease seeds; rows an earlier process left are found by the inventory, so the hint must cover them, and does. `lease_tick_inventory` skips only with no remembered lease, no live worker, no seed and `owned_media_sessions_hint` false. Differs from §3: the hint is a bool over a superset predicate (active, owned here, unexpired), not the inventory's rows. Embedded voter: idle 0 authority reads; remembered lease or owned row takes `owned_media_sessions`. |
| 2026-11-15 | Sharing run and claim loops | claude-opus-5-5 | session_01BCWLA3JD1HCNMEid3AZrc3 | Converted (mechanism 1): `loop_enabled` (same bound, parse and error as `enabled`) in `run` and `claim_loop`; #1598 has not landed, failure arms unchanged. The listener's 1 s observe loop (sharing on) and every per-import `enabled` re-check stay Store reads. |
| 2026-11-15 | Heartbeat's sharing switch | claude-opus-5-5 | session_01BCWLA3JD1HCNMEid3AZrc3 | Converted (local hint, as `enforcement_hint`): the heartbeat calls `heartbeat_purpose_keys`, which returns when `local_setting_hint("sharing_enabled")` is off; startup and the Source schema step still call `coordinate_purpose_keys`. Embedded voter: off/absent 0 authority reads; on takes `get_setting`. `coordinate_source_schema_live` returns on its persisted receipt before its own read, so it was left. |
| 2026-11-15 | Background work lanes | claude-opus-5-5 | session_01BCWLA3JD1HCNMEid3AZrc3 | Converted (mechanism 1) for the lanes' looks: `read_preparation_lanes` (cadence and output switch in one keyed read), `lane_cluster_fragment_index_enabled`, `lane_analysis_retry_policy`, `lane_subtitle_source_queue_enabled`; `JobManager` holds the view. Left on the Store: the output drain's switch (it cancels rows; `output_preparation_for_drain`, once a minute), the shared helpers' other callers, and the lanes' non-settings reads (queue listings, claims). |
| 2026-11-15 | Subtitle worker passes | claude-opus-5-5 | session_01BCWLA3JD1HCNMEid3AZrc3 | Converted (mechanism 1): `pass_settings` in the transcription pass and `config::read_applied` in the managed models pass. Differs from §2: the per-pass reads that remain are not settings (`job_candidates`, `subtitle_model_receipts`, translation operation listings) and are out of this plan's scope. |
| 2026-11-15 | Live TV loops | claude-opus-5-5 | session_01BCWLA3JD1HCNMEid3AZrc3 | Converted (view gate, then today's Store read): `loop_configs`, `recordings_library_viewed_ours`, `guide_viewed_on`. Differs from §2: `LiveTvConfig::owner_node_id` is always the reading node, so the reminder loop's read decided only readability and moved wholly; the reminder tick's own guide check was a second snapshot and is gated too. A finish while recording is off re-reads the Store for its fence generation, only when a row exists. Idle ticks no longer observe the configuration into the projection metric; the guide cache and fence observer still do from their own reads. |
