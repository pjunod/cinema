# Transport-recovery resource baseline — why that lane has never passed

**Status:** measured, and the decision in §6 is taken — Option A as a
campaign floor, built and described in
[TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md](TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md)
§0 · **Measured:** 2026-09-07 · **Decided:** 2026-09-08

This is the measurement the contract was changed on. Read it before touching
`wait_for_stable_idle_resources` in
[`crates/plurx-cluster-check/src/transport_recovery.rs`](../../crates/plurx-cluster-check/src/transport_recovery.rs),
and before raising `RESOURCE_CLEANUP_HORIZON`, relaxing the stability rule, or
opening any margin. Three of those four were tried on 2026-09-07 and §5 records
what each one proved. The lane's failure is understood; what remains is a
choice about what the check should assert, and that choice is §6.

---

## 1. The symptom — it has failed every run since the check landed

`ci / cluster transport recovery campaign` fails on `main` itself, and has
since `14d0519a`. The last green `main` run was `d5232db3` (run#404).

| run | sha | ref | tail |
|---|---|---|---|
| 450 | `abae39e0` | `refs/heads/main` | deadline expired, cycle 6/20 |
| 517 | `e224fa16` | `refs/heads/main` | deadline expired, cycle 6/20 |
| 593 | `dfcd3b54` | `refs/heads/main` | over baseline, cycle 1/20 |
| 466 | `39949bbb` | `refs/pull/74/head` | deadline expired, cycle 9/20 |
| 472 | `135a5757` | `refs/pull/67/head` | writer progress moved backwards |
| 546 / 557 | `fa43ef09` | `refs/pull/93/head` | deadline expired, cycles 4 and 1 |
| 571 | `2a0ba4fc` | `refs/pull/98/head` | idle and within baseline |
| 599 | `448af146` | `refs/pull/102/head` | over baseline, cycle 13/20 |

**How to read it:** a red campaign is not evidence against your branch, and
`Main promotion gate` failing in two seconds is not either — it needs the
campaign. PRs #67, #74, #93, #98 and #100 all merged with this red, which is
the established practice while it stays this way.

---

## 2. What the failure says now

Until 2026-09-07 the entire message was:

```
Error: recovery deadline expired before stable idle resource sampling
```

`wait_for_stable_idle_resources` gives up when any of three conditions is
still outstanding at `RESOURCE_CLEANUP_HORIZON`, and that sentence named none
of them. `cd75fd4d` (PR #98) made the expiry say which, and `ca198da7`
(PR #100) made it name the deltas between the last two samples. Both are on
`main`; every message quoted below comes from them, and the diagnosis in this
document was not possible before them.

```
 node still owning transport work? ──── yes ──▶ a leak. Never wait it out.
        │ no
 node above its warmed baseline? ────── yes ──▶ a leak. Never wait it out.
        │ no
        ▼
 only the repeat-sample rule left ─────────────▶ §4. Not a leak.
```

---

## 3. The measurement

Four consecutive voter recoveries, run locally on 2026-09-07 with the margins
temporarily widened so no cycle could abort the run. Counts are threads /
sockets / owned async tasks, sampled after quiescence.

| sample | node 1 (source) | node 2 | node 3 | node 4 (target) |
|---|---|---|---|---|
| warmup | 17 / **16** / **31** | 10 / **8** / **7** | 10 / **8** / **7** | 11 / **8** / **7** |
| cycle 1 | 17 / **19** / **35** | 10 / **9** / **8** | 11 / **9** / **8** | 10 / **9** / **8** |
| cycle 2 | 17 / **16** / **31** | 10 / **8** / **7** | 10 / **8** / **7** | 12 / **8** / **7** |
| cycle 3 | 17 / **19** / **34** | 9 / **9** / **8** | 10 / **9** / **8** | 11 / **9** / **8** |

**How to read it:** sockets and owned async tasks alternate between exactly
two states one recovery apart, and cycle 2 returns to the warmup values on
every node. Nothing accumulates. The high state is **one connection and one
owned task per peer** — three of each on node 1, which has three peers. With
the margins widened the run passed 3/3.

Threads are a separate, noisier story: node 4 wanders 11 → 10 → 12 across the
same three cycles with no pattern, and node 1 sits flat at 17. Thread counts
have their own jitter, independent of the connection drain.

---

## 4. What it is

The `idle` predicate is "no observation has `operation_owns_work`", and
`transport_status.rs` clears that flag at logical completion
(`observation.operation_owns_work = !done`). The per-peer connection and its
owned task are still open at that moment and close some tens of seconds later.
So each cycle samples on one side of a drain the readiness predicate does not
wait for, and the two-consecutive-identical-samples rule can be satisfied on
either side of it.

The baseline is one sample taken after one warmup recovery, compared against
with all three margins at zero. So the baseline records whichever side of the
drain that one cycle landed on, and every later cycle that lands on the other
side is reported as growth.

**A campaign is 40 cycles. It needs 40 coin flips.** That is the whole reason
this has never passed, and it is why the failure moves around the cycle
numbers — 1, 4, 6, 9, 13 — rather than reproducing at one point.

---

## 5. What was tried, and what each attempt proved

Each of these was run to a real result on 2026-09-07; none of them is on
`main`, and none of them should be reached for again without reading what it
did.

**Raising `RESOURCE_CLEANUP_HORIZON` alone — rejected before it was written.**
In run 546 the counts stayed above baseline for the full sixty seconds. A leak
does not drain, so more time buys nothing against the two conditions that
matter. Rejected on the evidence, not on taste.

**Relaxing the two-identical-samples rule — rejected.** The equality holds
fine. It is the ceiling comparison that trips, so this changes nothing about
the failure and costs the settling proof.

**Baseline as the ceiling of two warmups.** Two warmup cycles, elementwise
maximum, margins still zero. This removed the socket and owned-task failures
exactly as intended — and the run then failed on
`still waiting on every node idle and within its baseline, with 1 of 2
consecutive repeated samples (since the last sample: node 1 sockets 19 -> 16;
node 1 owned async tasks 35 -> 31; ...)`, which is the drain caught
mid-transition with the horizon expiring on top of it.

**Ceiling of two warmups plus a 180 s horizon.** Both together. The socket and
task failures stayed gone and the sampling had time to finish — and the run
failed on `node 4 threads 10 > 9`, because two warmups do not bound thread
jitter either.

**What that sequence proves:** every counter has a settled *range*, not a
settled value. A per-cycle zero-margin ceiling drawn from a small number of
samples will be crossed by one counter or another somewhere in forty cycles,
whatever the horizon. Adding warmups is the same bet at longer odds.

---

## 6. The decision — what should this check assert?

The invariant worth having is **no unbounded growth across the campaign**. The
current implementation approximates it with a per-cycle ceiling against one
warmed sample, and §5 is what that approximation costs. Three ways out, and
this is the choice that wants making:

| option | what it asserts | what it gives up |
|---|---|---|
| **A. Campaign-level growth** | the last cycle's counts do not exceed the first *recorded* cycle's | per-cycle attribution: a leak is reported at the end, not at the cycle that caused it |
| **B. Per-peer allowance** | per-cycle ceiling, margins of one socket and one owned task per peer | blindness to a leak exactly the size of one per-peer connection — the most likely leak there is |
| **C. Wait for the real thing** | readiness requires the recovery's per-peer transports released, not `operation_owns_work` cleared | needs a new signal out of vendored hiqlite; the largest change, and the only one that fixes the cause |

**A** is the cheapest honest version: over twenty cycles a per-cycle leak shows
up twenty times over, far outside the ±1-per-peer swing, and nothing has to be
weakened to see it. **C** is right and is the one to do if this lane is going
to carry weight. **B** is the one to avoid — it opens the margin precisely
where the failure it exists to catch would live.

Whichever is chosen, threads want their own answer: they jitter independently
of the drain, and none of the three options above addresses that on its own.

---

## 7. Non-goals

- **Do not raise the horizon on its own.** §5 says what that is worth.
- **Do not open a margin to make the lane green.** The margins being zero is
  the point of the check; opening them without choosing §6 explicitly turns a
  leak detector into a lane that passes.
- **Do not regenerate the baseline per cycle.** The artifact validator refuses
  a rebased baseline
  (`persistent_source_resource_baselines_cannot_be_rebased_between_cycles`),
  and it is right to: a baseline that follows the counts cannot detect growth.
- **Do not treat a red campaign as a branch defect.** §1.

---

## 8. One gap worth closing regardless

`.github/workflows/ci.yml` uploads
`target/validation/cluster-transport-recovery.json` with the lane's log and
receipt. A failing run aborts before writing that file, so the evidence is
never in the artifact — every failure this document is built on had only the
log to go on, and the per-cycle numbers in §3 needed a local reproduction to
obtain. Printing each cycle's sampled counts as the campaign runs would put
§3 in the CI log for free, on any option in §6.

## Reproducing this

`transport-recovery-voter-smoke` is the bounded form of the same campaign —
same `run_role_campaign`, a cycle count you choose:

```bash
# Linux only; the sha is embedded at compile time, so bind it for the build.
PLURX_BUILD_SHA=$(git rev-parse HEAD) cargo build -p plurx-cluster-check
./target/debug/plurx-cluster-check transport-recovery-voter-smoke 3
```

Roughly four minutes per recovery on two cores, so five recoveries is about
twenty minutes. `make cluster-transport-recovery-check` runs the full 20+20
and needs `PLURX_BUILD_SHA` and a real checkout.
