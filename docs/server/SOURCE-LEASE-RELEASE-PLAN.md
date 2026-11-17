# Source lease release — lease expiry frees source capacity, nothing else does

**Status:** step 1 merged (#1597); steps 2–4 built on
`claude/source-lease-steps-2-4`, [PR #1728](http://forge.lan:3000/noirr/plurx/pulls/1728) records qualification and landing ([as built](SOURCE-LEASE-AS-BUILT.md)) · **Owner:** GPT-6 Codex,
session `01a1e5c0-d4b1-7740-93b4-a0e0123a79cf`, continuing Claude Opus 5.5 ·
**Written:** 2026-11-14 · **Updated:** 2026-11-16

Companion to [SOURCE-CUSTODY-RECOVERY.md](../archive/SOURCE-CUSTODY-RECOVERY.md), which
records the physical-proof model this plan retires, and
[JOB-QUEUE-ROOT-CAUSE-AND-FIX.md](JOB-QUEUE-ROOT-CAUSE-AND-FIX.md), which owns
the wider queue work. Read §1 for why, §2 for the rule, and §4 for the steps.
If a step seems to need a new proof, witness or host helper, stop and flag
it instead: that is the design this plan replaces.

## 1. Why — one unprovable reader stopped the whole cluster

Paul, 2026-11-14: *"Simple run of the mill jobs along with daemon restarts
cannot break all job processing across the cluster … These aren't nuclear
launch codes. They are media server media jobs."*

Under the custody model, a source owner whose lease had lapsed kept its
`source_io` charge until it proved, physically, that its reader had stopped.
The claim SQL in
[`background_jobs.rs`](../../crates/plurx-core/src/store/background_jobs.rs)
refuses **every** source-reading claim while any such expired custody
exists. With no storage domains configured, that is every scan, analysis,
probe and preparation in the cluster.

The 2026-11-14 incident shows how easily the proof fails:

| Attempt | Why it could not release custody `7c92c813` (job `4fc08d71`) |
|---|---|
| The job's own completion ledger | The job **succeeded** at 15:56 UTC; the ledger waited on in-process holder counts that never reached zero, silently |
| Daemon restart (namespace teardown replay) | The kernel reused the PID-namespace inode `4026532506`, so the replay saw "same namespace" |
| Host census helper | Needs the `/run/plurx-physical-stop` bind that only #1561 adds |
| What worked | Stop plurxd, park a throwaway container on the freed namespace number, start plurxd |

Earlier incidents (2026-11-09 → 11-13: three playback owners on one voter, a
cancelled `transcode_prepare` on another) needed a host reboot or a new helper.
Each fix added another proof path. Every one of them can fail closed, and a
closed failure is a cluster-wide outage.

## 2. The rule — lease expiry is the release

A source owner (foreground or background) holds capacity while its
authority is live. When its authority has lapsed for
`SOURCE_AUTHORITY_REAP_GRACE_MS` (60 s), the store sweep deletes it. No
settlement proof, witness, census or reboot is asked for.

**Why that is safe enough:** the worst case is an orphaned child of a dead
daemon still reading a media file while a new reader starts on the same
disk. That is one extra concurrent read: a throughput cost, never a
correctness one. plurxd runs as its container's init, so its children die
with it anyway.

**Why 60 s:** the grace absorbs a renewal that races the sweep, plus clock
skew between nodes. It is not a proof window.

**When authority lapsed.** For a live owner, authority is its job, request,
lease or package expiry. Once the owner stops running (finished,
`cancelling`, re-fenced), authority reads 0, which carries no time. The
sweep stamps such a row `cleanup_pending` with the time it first saw it, and
the grace runs from that stamp. A cancelling job therefore gets its full
grace to kill its child. A paused row holds no capacity and goes at once.
The sweep, the recovery metric and the Activity panel share one expression
for this, `custody_lapsed_at_sql`, so they cannot disagree. Playback renews every few
seconds and loses its authority on any failed renewal. So reaping a row 60 s
after its lease ended can never take capacity from a healthy playback.

```
 owner authority live? ── yes ──▶ holds source capacity
        │ no
 lapsed ≥ 60 s? ───────── no ───▶ still charged (renewal may be in flight)
        │ yes
        ▼
 sweep deletes the row ──▶ capacity free; any late settle is a no-op
```

## 3. Non-goals

- **No new proof paths.** No witness, census, physical-stop or reboot logic
  is ever again a condition for releasing capacity, because each one adds a
  way for one job to wedge every node.
- **No per-symptom watchdogs.** The sweep is the single mechanism. If an
  owner outlives it, the bug is in the sweep.
- **Not a capacity redesign.** How many concurrent source readers a node or
  domain allows is §4 step 4, a separate decision.

## 4. Steps

### 4.1 Step 1 — expiry releases (PR #1597)

- `reap_settled_source_custody` deletes owners lapsed past the grace,
  stamps newly ownerless rows, and clears analysis coordination roles still
  pointing at released custody.
- The claim paths (`background_jobs.rs` and the analysis capacity terms in
  `fragment_index_cluster.rs`) no longer refuse every source claim while
  any custody has lapsed. A lapsed owner counts as one ordinary reader until
  the sweep releases it.
- Every completion-ledger path that cannot publish a joined proof waits for
  the sweep, then removes its local witness. That covers a dropped owner, a
  vetoed capability, a refused join, and children that can't be proven
  exited. The ledger logs its open holders after 60 s.
- Replay collects v2 witnesses whose owner the store no longer holds.
- The recovery metric, its warning and the Activity panel flag only owners
  still charged 3 minutes after their lease lapsed, meaning the sweep itself
  isn't running.

**Acceptance:** the two `Regression-Test` lines in #1597 pass. On the fleet,
`plurx_background_source_recovery_pending` stays 0 through a
`docker restart plurxd` on a node with a running source job.

### 4.2 Step 2 — delete the proof machinery

Remove what step 1 made dead weight:

- **Code:** `source_custody/recovery/{reader,source_census,physical_stop}.rs`
  and their tests; the completion ledger in
  `background_jobs/source/completion.rs` (finish releases directly); the
  launch-observation hooks that poison witnesses.
- **Scripts and their tests:** `scripts/source-custody-{recovery,source-census,reader-stop}`.
- **Compose:** the `/run/plurx-physical-stop` bind.
- **Formal pins:** the matching entries in `verification/*/mapping.json`.

Keep the store tables until step 3. The source-arbitration schema is pinned
to its exact SQL, so changing it needs a versioned extension migration.
Two schema texts still describe the old model and change with step 3:

- the `dispatch_turn` view in `background_job_turns.sql` still lets an
  unreaped lapsed custody stop a ready scan from displacing other work (it
  clears when the sweep releases it);
- the `background_source_custody` comments in
  `background_jobs_foreground.sql` still say only a settlement receipt
  deletes a row.

**Acceptance:** `cargo clippy --all-targets -D warnings` is clean; the fast
lane is green; Activity shows no recovery panel after a restart.

### 4.3 Step 3 — one lease table (built)

Replace `background_source_custody`, `foreground_source_owners` and the
`source_io_occupancy` union with one table:

```
source_leases(owner, node_id, resource_key, expires_at_ms)
```

The claim checks it, and the holder's heartbeat renews it. This needs a
source-arbitration extension v2 with an explicit upgrade from v1.

### 4.4 Step 4 — capacity per node, not one cluster slot (built; Paul decided 2026-11-15)

Today, with no storage domains, the whole cluster shares **one**
`source_io` slot. A per-node default (for example two background readers per
node, with storage domains as an optional cap) would stop one slow reader on
one node from serialising the cluster.

## 5. Superseded and affected work

- [PLAYBACK-READER-WITNESS-PLAN.md](PLAYBACK-READER-WITNESS-PLAN.md) phases
  B0–B3 strengthen the witness model that step 2 deletes. **Paused pending
  Paul's call.**
- **#1561** (census recovery for playback owners) is on a merge train owned
  by another session. It becomes unnecessary after step 1 and is removed in
  step 2.
- [SOURCE-CUSTODY-RECOVERY.md](../archive/SOURCE-CUSTODY-RECOVERY.md) is retired to
  `archive/` in step 2.

## Execution log

| Date | Session | Step | Result |
|---|---|---|---|
| 2026-11-14 | Claude Opus 5.5 · session_01BCWLA1BAXKSuq2TLMK9Zna | Incident | Custody `7c92c813` released on its voter via the fresh-namespace restart |
| 2026-11-14 | Claude Opus 5.5 · session_01BCWLA1BAXKSuq2TLMK9Zna | Step 1 | PR #1597 opened as a draft |
| 2026-11-14 | Claude Opus 5.5 · session_01BCWLA1BAXKSuq2TLMK9Zna | Step 1 review | Fable review: 5 should-fix, 3 nits, all addressed (stamped grace, veto removed, witness retirement, role unpinning, shared lapse expression) |
| 2026-11-15 | Claude Opus 5.5 · session_01BCWL9zn5U4msvhC85BdESw | Step 2 | proof machinery removed (branch claude/source-lease-steps-2-4) |
| 2026-11-15 | Claude Opus 5.5 · session_01BCWL9zn5U4msvhC85BdESw | Steps 3–4 | one `source_leases` table; per-node readers (default 2, per-node override) and per-domain `source_readers` (default 1); built on claude/source-lease-steps-2-4 |
