# Claim local verdict — stop proposing job claims the node can see will be refused

**Status:** open — ready to build · **Executes:** A1 of
[CLUSTER-LAYER-AUDIT-2026-11-14.md](CLUSTER-LAYER-AUDIT-2026-11-14.md) ·
**Design:** Claude (Fable 5.2), session `session_01BCWLA3JD1HCNMEid3AZrc3` ·
**Verified against:** `main @ 8785492be` · **Written:** 2026-11-14

> **Who reads this.** The build agent (Claude, GPT or OpenRouter). Work §5 one
> milestone at a time and record each in §7. If a step seems to need a change
> to what a claim *is* — the predicates, the lease, the reservation trigger —
> stop and flag it; that is out of scope (§4).

Companion to the audit, which measured the defect. This is how to remove it.

## 1. Objective

On the fleet, workers on every node propose a claim for every queued
background job while the job cannot be admitted, are refused, and repeat every
25–30 s. One node attempted 155,873 `subtitle_extract` claims in 14.6 hours
for 7 accepted. Each attempt is a quorum read plus a Raft proposal; together
they are more than half of the cluster's log.

After this change a node proposes a claim only when its own applied replica
says the claim statement would accept it. Nothing about admission changes: the
replicated statement stays the only authority, and it stays exactly as strict.

Acceptance, on one node with a deep queue and the source slot taken:
`claim_refused / claim_write` below 5 % per kind, and a new
`claim_skipped{reason}` series that names why.

## 2. How a claim works today

All paths are under `crates/`; re-verify line numbers at build time.

| Step | Where | Cost |
|---|---|---|
| Worker lists candidates | `plurx-core/src/store/background_jobs.rs:4215` `job_candidates`, a local read, no capacity predicates | free |
| Worker calls `claim_with_resolution` | `plurxd/src/background_jobs.rs:291`; counts `claim_write` before the store call | — |
| Quorum pre-read | `plurx-core/src/store/background_jobs.rs:4826-4829`; refuses without proposing only for a missing, replayed or cancelled job | 1 quorum read |
| `CLAIM_SQL` | `:513-688`, proposed for everything else; zero rows is `Contended` | 1 Raft proposal |
| Refusal | `Ok(None)` → `claim_refused`; the worker loop does `continue` | next candidate |
| Idle | `IdlePoll::delay` 5→25 s plus jitter (`plurxd/src/background_jobs.rs:208-222`) | then repeat |

`CLAIM_SQL`'s WHERE clause has three groups of predicates. Every one reads
plain tables, so every one can be evaluated on the local replica, and the time
input is the caller's own `now_ms` in both places:

- **identity and state** — revision, kind, payload version, target node,
  queued or lease-expired, retry deadline, attempt limit, `not_before`, a
  pending waiter, owner not removed, counters not exhausted;
- **kind demand** — `dv_title_scan` needs its live demand row,
  `subtitle_extract` its live analysis request, plus the translation, text and
  subtitle-model admissions already written as macros (`:104-140`);
- **source admission** — expired custody, the foreground permission window,
  the two-holder ceiling, and the second reader reserved for live demand.

The `dispatch_turn_job` trigger can also make the update match zero rows
(`background_job_turns.sql:151-169`). It writes, so it cannot be pre-evaluated.

`dv_title_scan` proposes twice more per candidate before it claims: an
unconditional demand-reset update and a bind insert
(`plurx-core/src/store/dv_title_jobs.rs:72-125`,
`plurxd/src/source_probe/title_work.rs:135-176`).

## 3. Design

```
 worker pass
   │
   ▼
 job_candidates  ── built from the claim predicates ──▶ only admissible jobs
   │ (local read)
   ▼
 claim_job
   │
   ├─ no local replica ───────────────────────────────▶ today's path
   │
   ▼
 local verdict (same predicates, local read)
   │
   ├─ Blocked(reason) ──▶ ClaimOutcome::Contended
   │                       no quorum read, no proposal, claim_skipped{reason}
   ▼ Admit
 quorum pre-read ─▶ CLAIM_SQL (proposal) ─▶ Claimed | Contended (a real race)
```

1. **One set of predicates.** Split `CLAIM_SQL`'s WHERE clause into
   alias-parameterised `macro_rules!` fragments — the pattern
   `translation_admission!` already uses — one per group in §2. `CLAIM_SQL`,
   the verdict query and `job_candidates` are all `concat!`-ed from them. The
   reason is drift: a hand-copied twin is how `job_candidates` got its one
   copied `subtitle_extract` block and nothing else.
2. **The verdict is a local read that names the failing group.** One SELECT
   over the job row returning the first group that fails, as a fixed reason:
   `state` · `kind_demand` · `foreground_window` · `source_holders` ·
   `maintenance_reader` (lapsed foreground custody fails the foreground
   group; it has no predicate of its own), with whether `kind_demand` holds. It runs at the top of `claim_job`,
   before the quorum pre-read, on the local replica.
3. **A stale verdict is safe in both directions.** A stale "blocked" skips a
   claim that the replica will re-admit as soon as it applies the write that
   freed it. A stale "admit" costs one refused proposal, which is today's
   behaviour. So the verdict needs no serving proof. A process with no local
   replica (the SQLite backend is its own authority; a remote-authority client
   has none) takes today's path. This is narrower than the audit's wording,
   which also sent a proofless node down today's path; that would bring the
   spin back during any quorum gap, for no safety gain.
4. **The worker keeps its cadence.** A pass with nothing admissible ends and
   waits on the existing `IdlePoll` and the existing local wakes
   (`source_pressure_changed`, the queue `Notify`s). Pickup latency after
   capacity frees is what it is today, 25–30 s at worst; what changes is that
   a blocked pass costs local reads only. No new timer, no per-kind hold. A
   general applied-change wake is the audit's B4 and is not built here.
5. **`dv_title_scan` does not write before it knows it can claim.** The demand
   reset becomes conditional on a local read showing something to reset, and
   the bind and enqueue happen only for a demand whose job the verdict admits.
6. **Counters say what happened.** `claim_write` counts proposals only. A
   skipped claim counts `claim_skipped{reason}`.

## 4. Non-goals

- **Do not change admission.** No predicate is loosened, tightened or
  reordered. The single background source reader per key is the throughput
  defect behind this one and belongs to the source-lease work (PR #1597, plan
  steps 2–4), not here.
- **Do not add a timer, a watchdog or a per-kind backoff.** The fix is that a
  doomed claim is never made, not that it is made less often.
- **Do not pre-evaluate the dispatch-turn trigger.** It writes; it stays
  authoritative, and a turn lost there is an ordinary `Contended`.
- **Do not add a setting or a gate.** There is nothing to switch.
- **Do not touch the SQLite standalone path's behaviour.** It may share the
  macros; it must not start skipping claims.

## 5. Milestones

Base the branch on `main` with PR #1597's head merged in if #1597 has not
landed: it edits the same WHERE clause.

### 5.1 M1 — shared predicates, no behaviour change

Split the WHERE clause into the fragments and rebuild `CLAIM_SQL` from them.

**Acceptance:** the expanded `CLAIM_SQL` text is byte-identical to the text
before the split apart from whitespace — assert it in a test against the old
constant kept as a test-only literal, then delete the literal in M2. The store
contract suite for background jobs passes unchanged.

### 5.2 M2 — the verdict, and `claim_job` consulting it

Add the verdict query and reason enum; call it first in `claim_job` on the
replicated backend; a blocked verdict returns `ClaimOutcome::Contended`,
what the statement itself returns for the same refusal on either backend,
without a pre-read or a proposal.

**Acceptance:** a store-level test builds each blocking state (a held
maintenance reader, two holders, expired custody, a foreground window, a
missing analysis request, a stale revision) and asserts for each that the
verdict blocks with the right reason **and** that `CLAIM_SQL` run directly
refuses the same job; and for an admissible job, that both admit. Prove the
test rejects a wrong verdict: break one fragment's use in the verdict query,
see the test fail, restore.

### 5.3 M3 — candidates, `dv_title_scan`, counters

Build `job_candidates` from the fragments (dropping the copied
`subtitle_extract` block). Make the `dv_title` demand reset conditional and
order verdict before bind. Move the `claim_write` count to the proposal and
add `claim_skipped{reason}` to `background_job_metrics.rs`. Hoist the
learner's `may_execute_job` quorum read from once per candidate to once per
pass where the worker loop allows it without changing what is checked.

**Acceptance:** a `plurxd` test with a queue of N jobs behind a held source
slot runs one worker pass and observes zero claim proposals and N or fewer
`claim_skipped`; releasing the slot and running a pass claims one job.

### 5.4 M4 — one adversarial review, then the lane

One adversarial agent review of the whole diff. Address it. Run the fast lane
once on the result; rerun only what fails.

### 5.5 M5 — fleet check (after Paul deploys)

**Acceptance:** on one node, `claim_refused / claim_write` per kind below 5 %
over ten minutes with the queue blocked, and the commit index advancing at
less than half its 2026-11-14 rate of 38/s under the same conditions.

## 6. Risks

| Risk | Why it is bounded |
|---|---|
| A stale replica says "blocked" forever | It stops being stale when it applies the freeing write; a replica that never applies cannot win a claim either |
| A time-only change (lease expiry, `not_before`) is missed | The verdict takes `now_ms`; the next pass re-evaluates it. Worst case is one `IdlePoll` period, as today |
| The fragments drift from the statement | They are the statement; M1's identity test and M2's paired test hold it |
| The verdict query is slow on a deep queue | It is keyed by job id like the statement; `job_candidates` pages at 128 rows as today. Measure one pass in M3 and record it in §7 |

## 7. Execution log

| Date | Milestone | Agent-Model | Agent-Session | Result |
|---|---|---|---|---|
| 2026-11-14 | Design | claude-fable-5-2 | session_01BCWLA3JD1HCNMEid3AZrc3 | this document |
| 2026-11-14 | M1 | claude-opus-5-5 | session_01BCWLA3JD1HCNMEid3AZrc3 | `CLAIM_SQL` assembled from seven fragments (identity, kind demand, state, attempt ledger, foreground window, source holders, maintenance reader); equals the pre-split text modulo whitespace (23,847 bytes); the identity check rejects a changed `>=2` and a dropped predicate line. The pre-split literal stays test-only through M3: it is what holds the fragments that three queries now share |
| 2026-11-14 | M2 | claude-opus-5-5 | session_01BCWLA3JD1HCNMEid3AZrc3 | `CLAIM_VERDICT_SQL` + `ClaimBlock`; `claim_job` consults it first on a process with an applied replica. All six blocking states and the admissible job pair verdict with the statement on an embedded voter beside SQLite; breaking the verdict's holders or foreground fragment fails the test. Blocked verdicts return `Contended` (not `ResourceUnavailable`): SQLite and the both-backend contract suite answer `Contended` for the same refusals. No `source_recovery` predicate exists on its own since custody retention: lapsed foreground custody fails the foreground group (review round 2 folded the reason into `foreground_window`) |
| 2026-11-14 | M3 | claude-opus-5-5 | session_01BCWLA3JD1HCNMEid3AZrc3 | `job_candidates` filters by kind demand, state and the three source fragments (copied `subtitle_extract` block dropped); depth 300 behind a held slot lists 0 jobs in 12–14 ms (was 127 jobs in 6.6 ms), one skipped claim 1.8 ms on the replica vs a 58–60 ms refused SQLite proposal; released, one claim wins and the rest list nothing again. Title demand reset proposes only when the replica shows a demand to reset; a queued title job blocked on its source is neither bound nor claimed. `claim_write` counts proposals only; `plurx_background_claim_skipped_total{kind,reason}`. Authority read once per pass in the pre-transcode, fragment and subtitle passes. plurxd tests not run here (memory) |
| 2026-11-14 | Review r2 | claude-opus-5-5 | session_01BCWLA3JD1HCNMEid3AZrc3 | B1: the admissible listing is opt-in (`CandidateQuery::admissible_only`), honoured only on a replica (SQLite lists as before), set only in the pre-transcode, fragment and subtitle claim passes; library turns keep the unfiltered listing, and a scan behind a held reader keeps its ready turn and wins the freed slot on both backends. The verdict returns every group (`ClaimVerdict`); the title pass reads it from the replica alone and binds whenever kind demand fails. `source_recovery` folded into `foreground_window`. Differential test: 2,000 random SQLite states, verdict ⇔ statement and admissible listing ⇔ statement, rejects the verdict ignoring source holders at case 50. Depth 3,000 behind a held slot: admissible page 12.8 ms (0 jobs) on the replica, unfiltered SQLite page 6.1 ms, released 11.0 ms — flat against depth 300 (14.5 ms), so no per-resource rewrite. `CLAIM_SQL` is not byte-identical to the pre-split text: one space differs (offset 13,247, `$1, '` for `$1,'`); equality modulo whitespace was proven and the literal deleted |
