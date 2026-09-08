# Transport-recovery resource contract — the decision

**Status:** decided and built — Option A, as a campaign *envelope* (§0) ·
**Decides:** what the campaign's resource check asserts · **Companion to:**
[TRANSPORT-RECOVERY-RESOURCE-BASELINE.md](TRANSPORT-RECOVERY-RESOURCE-BASELINE.md)
(the measurement) · **Written:** 2026-09-08 · **Decided:** 2026-09-08

§0 is what was chosen and why it differs in one respect from §4 as first
written. §1–§10 are kept as the record the choice was made from. The
diagnostic work that made the choice visible is on `main` — `cd75fd4d` (#98),
`ca198da7` (#100), `4036e8dc` (#121) — and the per-cycle log line from #123
lands with the same change as §0.

---

## 0. What was decided

**Option A, with the comparison made between the two halves of the campaign
rather than between its first and last cycle, and at both edges of the range
a resource moves in rather than at one point.** Option C stays open as the
follow-up that makes `idle` honest; nothing here depends on it. Option B was
not taken.

### 0.1 Why not §4 exactly as written

§4.1 asserts "the last recorded cycle's counts do not exceed the first
recorded cycle's, margins at zero". That is two samples of the same two-state
range §2.3 describes. The drain lands each sample high or low; cycle 1 low and
cycle 20 high reads as +3 sockets on node 1 at zero margin — the same false
positive as today, drawn twice per campaign instead of forty times. The
measured series alternates strictly (low, high, low, high), which would put
cycles 1 and 20 on *opposite* sides every time. §4.3's arithmetic is right
about a leak; it is wrong about the noise, because a single sample carries
the full noise amplitude however far apart two samples are.

A statistic over a *window* does not. Ten samples of a series that visits
both of its states every few cycles contain both states, so the minimum of
ten is the low state and the maximum of ten is the high state, whichever
cycles happened to land where. A leak moves both. So the assertion is on the
band each resource moved in — its **envelope** — over each half of the
campaign:

> For each node and each resource, neither the floor nor the ceiling of the
> *closing* half of the campaign exceeds the floor or ceiling of the
> *opening* half by more than that resource's allowance.

The series is the warmup baseline (cycle 0) followed by the twenty recorded
cycles. The opening window is the first half rounded up — cycles 0..=10 —
and the closing window is cycles 11..=20. The two edges catch different
leaks: one that starts early adds every cycle and lifts the closing *floor*
by ten or more; one that starts after the opening window never lifts the
closing floor (its first cycles are still at the old floor) but puts most of
the closing window above anything the opening half showed, and that is the
closing *ceiling*.

The two edges are not the same kind of number, and the ceiling is defined
accordingly. The floor is a resting state — one sample of it is evidence.
The ceiling is whatever was still in flight when the sampler looked, and
§2.2's own table shows it is not one value: node 1's owned tasks sat at 35
on one high-side cycle and 34 on the next. A raw maximum at zero allowance
would fail the campaign on that alone. So the **closing ceiling is the
fourth-highest sample of the closing window**
(`RESOURCE_CEILING_SUSTAINED_SAMPLES`): a count that three cycles reached
and seven did not is a spike — a replacement connection during one
recovery, a slower drain on one cycle, a blocking thread that had not idled
out — and a leak is not a spike. A leak from cycle 13 puts eight samples
above the opening ceiling and its fourth-highest is five over; one socket
leaked at cycle 15 and never released puts six samples one over, and fails
at zero allowance. The opening ceiling is the plain highest of cycles 1
through 10, so a spike there raises the bar for the closing window rather
than lowering it. The warmup baseline, the one number in the artifact that
nothing recomputes, bounds the floor only.

### 0.2 The allowances, and what each one hides

| resource | allowance | why, and the boundary |
|---|---|---|
| sockets | **0** | the drain is one connection per peer, and a ten-cycle opening window holds both of its states; a per-peer transport that stops being released is +10 at the floor, and one leaked connection at cycle 15 is +1 at the sustained ceiling. What hides: a leak that starts after cycle 11 *and* has fewer than four closing samples above the opening's high state by cycle 20 — a per-cycle leak from cycle 18 on, or one leaked connection after cycle 17, or any late leak that has accumulated less than the drain amplitude on the low-side samples. That is the limit of a sustained statistic on this signal, and the alternative was a ceiling that fails on one sample |
| owned async tasks | **0** | same shape, same reasoning, same boundary |
| threads | **2** | §2.3 and §5.3: threads jitter with no recovery in flight, and the floor is not seen on every cycle; two is the widest measured swing. A thread leaked every recovery is +10 at the floor; one every third recovery lifts the closing floor by three and is caught; **one every fourth recovery or slower hides** inside the allowance at both edges. The per-cycle check could not see a leak that slow either, because it never finished a campaign |

These replace `THREAD_MARGIN` / `SOCKET_MARGIN` / `OWNED_ASYNC_TASK_MARGIN`
as `THREAD_ENVELOPE_ALLOWANCE` / `SOCKET_ENVELOPE_ALLOWANCE` /
`OWNED_ASYNC_TASK_ENVELOPE_ALLOWANCE`, recorded in the artifact as
`resource_*_envelope_allowance` and pinned by its identity check as before.
This is not §6: nothing per cycle is relaxed, and the socket and task
allowances are exactly where the leak that matters would show.

### 0.3 What it rests on, and the two things that look like a leak

The claim is not "noise cannot move a minimum". It is: if a cycle lands in
the drain's low state with probability *p*, the closing floor misses the low
state with probability (1−*p*)^10, and the sustained closing ceiling clears
the opening ceiling only when the opening's ten cycles never showed the high
state *and* at least four of the closing ten did — *p*^10 · P(Bin(10, 1−*p*)
≥ 4). At the measured *p* = ½ that is 0.1% and 0.08% per node and resource,
about 2% per campaign over the six persistent node-resources of two roles;
at strict alternation, which is what the CI runner's four measured samples
show, it is zero. The two edges fail on opposite hosts. A slow runner that
mostly lands high is exposed at the floor; a fast one that mostly lands low
— nuc3 sampled `16/31` on the warmup and cycles 1 and 2, then `19/35` on
cycle 3 — is exposed at the ceiling, and needs four slow drains in the
closing half after none in the opening: at one slow drain in five, 0.7% per
role. The other wobble in the record, 34 against 35 owned tasks on node 1's
high side, needs the rarer value four times in the closing half and never in
the opening ten; at one in four it is about 1% per node-resource, and it was
the reason the raw maximum was not shipped. What would move any of these
numbers is a runner whose drain timing *changes between the halves* of a
campaign; the per-cycle print (§0.4) is how the next red says whether it
did, and the first green campaigns are what calibrates them.

Two benign things fail this assertion, and both are visible in the log:

- **The snapshot source moving between the halves.** The leader carries the
  peers' connections (16 sockets on node 1 against 8 on a follower in §2.2).
  If leadership among nodes 1–3 moves at cycle 12 and stays, the new leader's
  floor rises by the difference. The series printed on failure carries the
  source node of every cycle for exactly this reason.
- **Node 4.** It is killed and restarted every cycle, so its twenty-one
  samples are twenty-one fresh processes; nothing can leak *inside* it across
  cycles. Its row is a different claim — that the footprint a fresh target
  comes up with, against the cluster state the previous cycles left, does not
  grow — and it is kept because that is a property of the campaign, not of
  the process.

### 0.4 What moved, site by site

| # | site (§3) | after |
|---|---|---|
| 1 | `wait_for_stable_idle_resources` | readiness is *idle and two identical samples*; the ceiling term is gone, so a high-side sample no longer burns the sixty-second horizon |
| 2 | `collect_node_resource_evidence` | records `baseline` and `post_quiescence`; the `leaked resources` bail is gone |
| new | `resource_envelopes` · `resource_envelopes_over_allowance` | computed at the end of `exercise_role_campaign` for the campaign and for every smoke, and printed; a rise past the allowance prints the per-node series with the source of each cycle, then fails the role |
| 3 | `validate_cycle_resources` | shape only — four nodes, in order, correctly labelled |
| 4 | `validate_role_campaign` | the no-rebase rule stays; `validate_campaign_resource_envelopes` recomputes the envelopes from the cycles, refuses a recorded envelope that differs, and asks the same question with the artifact's recorded allowances |

`resource_envelopes` refuses, rather than skips, a campaign whose cycle
records are missing, reordered, mislabelled or renumbered, so the offline
check does not depend on the per-cycle checks having run first. The
per-cycle sample is printed as the campaign runs (#123's line), so a failure's
series is in the CI log without a local reproduction.

**The smoke records and prints its envelope but does not fail on it** unless
the closing window holds the ten samples the statistics above are for
(`RESOURCE_ENVELOPE_WINDOW_SAMPLES`): a window of two samples is the coin flip
this change exists to remove, and `transport-recovery-voter-smoke 3` is a
rehearsal of the recovery path, not the leak verdict. Nineteen cycles is the
shortest campaign that asserts.

The artifact gains `resource_envelopes` per role campaign
(`opening_window_last_cycle`, `closing_window_samples`, and each node's
`opening` and `closing` band as floor and ceiling), the same way it carries
`worst_durations` — recomputed and compared by the validator, so the file is
self-proving without the harness. That is a field-set change, so the schema is
**version 2** (`cluster-transport-recovery-v2.json`). No version-1 artifact
was ever produced; the version-1 contract never completed a campaign.

### 0.5 The pinning tests

`transport_recovery::tests` holds 39 (from 31: eleven added, three removed —
the two `one_extra_*_after_quiescence_is_rejected` and the wait report that
named a count over its baseline, which the sampler no longer computes).

Leaks that must fail, and do, naming node, resource, edge and both values:
`a_socket_leaked_every_cycle_is_rejected` ·
`a_thread_leaked_every_cycle_is_rejected` ·
`a_leak_that_begins_late_in_the_campaign_is_rejected` (onset at cycle 13,
caught at the ceiling with the floor untouched; and one socket leaked at
cycle 15) · `a_thread_envelope_past_the_allowance_is_rejected` (including
one thread every third recovery, and every fourth as the documented miss) ·
`three_high_closing_samples_are_spikes_and_four_are_a_trend` (and a
baseline written as 99 sockets bounds nothing at the ceiling) ·
`owned_async_task_growth_has_no_resource_slack` — name kept, the Makefile
and `test_contracts.py` pin it; it now proves the sampler records a high
sample rather than refusing it, and that a task floor rising by exactly one
is a leak.

Measured noise that must pass, and does:
`the_per_peer_transport_drain_is_not_growth` (§2.2's exact series, both
bands identical across the halves; and the same drain with the warmup on the
high side, landing high every third cycle) ·
`thread_jitter_inside_the_allowance_is_not_growth` ·
`a_single_high_sample_in_either_window_is_recorded_not_rejected` (a lone
spike in either window, unmatched by the other).

The artifact proving itself: `recorded_resource_envelopes_must_match_the_cycles`
(each of the four recorded numbers tampered) ·
`an_envelope_over_incomplete_or_reordered_records_is_refused` ·
`the_opening_window_is_the_first_half_of_the_series_rounded_up` (20 →
0..=10 | 11..=20; smoke 3 → 0..=1 | 2..=3; asserted from nineteen cycles) ·
`every_persistent_source_voter_requires_resource_evidence` ·
`persistent_source_resource_baselines_cannot_be_rebased_between_cycles` ·
`schema_is_closed_and_valid_draft_2020_12`.

### 0.6 What is still true

`RESOURCE_CLEANUP_HORIZON`, `RESOURCE_SAMPLE_INTERVAL`,
`RESOURCE_STABLE_SAMPLES` and the one warmup cycle are unchanged. The `idle`
predicate is still `operation_owns_work`, and it is still wrong on its own
terms — Option C. The campaign stops being broken by it; nothing else that
reads it gets better until C is built, and C is also what would let the
windows shrink and the boundaries in §0.2 tighten with evidence.

---

## 1. What you are deciding

`ci / cluster transport recovery campaign` has never passed since its resource
check landed at `14d0519a`. The check is not wrong about anything it sees; it
is asserting a property the system does not have, and no amount of tuning
makes a system have a property it does not have.

**The question: should the campaign assert "no cycle exceeds a warmed
ceiling", or "the campaign does not grow"?** Everything below follows from
that.

---

## 2. The problem

### 2.1 The symptom

Red on `main` and on every pull request that runs it, at a different cycle
each time — 1, 4, 6, 9, 13 — which is the signature of a per-cycle
probability, not a defect at a point. `ci / Main promotion gate` fails behind
it in two seconds on every PR. PRs #67, #74, #93, #98, #100 and #121 all
merged with it red, because the alternative was merging nothing.

### 2.2 The measurement

Four consecutive voter recoveries, margins temporarily widened so no cycle
could abort the run. Threads / sockets / owned async tasks, per node, sampled
after quiescence:

| sample | node 1 (source) | node 2 | node 3 | node 4 (target) |
|---|---|---|---|---|
| warmup | 17 / **16** / **31** | 10 / **8** / **7** | 10 / **8** / **7** | 11 / **8** / **7** |
| cycle 1 | 17 / **19** / **35** | 10 / **9** / **8** | 11 / **9** / **8** | 10 / **9** / **8** |
| cycle 2 | 17 / **16** / **31** | 10 / **8** / **7** | 10 / **8** / **7** | 12 / **8** / **7** |
| cycle 3 | 17 / **19** / **34** | 9 / **9** / **8** | 10 / **9** / **8** | 11 / **9** / **8** |

**How to read it:** cycle 2 is identical to the warmup on every node. Nothing
accumulates. With the margins widened the run passed 3/3. This is not a leak.

### 2.3 Two independent noise sources, not one

**The drain.** Sockets and owned async tasks alternate between exactly two
states one recovery apart. The high state is **one connection and one owned
task per peer** — three of each on node 1, which has three peers. The `idle`
predicate is "no observation has `operation_owns_work`", and
`vendor/hiqlite/src/transport_status.rs` clears that flag at logical
completion (`observation.operation_owns_work = !done`). The connection and its
task are still up at that moment and close some tens of seconds later. Each
cycle samples on one side of a drain the readiness predicate does not wait
for.

**Thread jitter.** Separate, and not caused by the drain. Node 4 goes
11 → 10 → 12 → 11 across the same four recoveries; node 1 sits flat at 17.
This is pool behaviour, and it is present with no recovery in flight.

Any option that addresses only the first leaves the second.

### 2.4 Why the current contract cannot hold

The baseline is one sample, taken after one warmup recovery, on whichever
side of the drain that cycle happened to land. Every margin is zero. So a
later cycle landing on the other side is reported as growth.

```
 warmup lands low ─┬─ cycle lands low  ──▶ pass
                   └─ cycle lands high ──▶ "leaked resources"   ◀── false
 warmup lands high ┬─ cycle lands low  ──▶ pass
                   └─ cycle lands high ──▶ pass
```

A campaign is `TRANSPORT_RECOVERY_CYCLES_PER_ROLE` = 20 cycles per role, two
roles, **40 cycles**. Every one of them is a fresh draw. Thread jitter draws
again on top. **The lane needs forty consecutive favourable draws to go
green**, which is why it never has.

---

## 3. Where the contract is enforced today

Four sites, all in
[`crates/plurx-cluster-check/src/transport_recovery.rs`](../../crates/plurx-cluster-check/src/transport_recovery.rs).
Any option has to move all of them or say why it doesn't. Line numbers are
`main` at `e1c82934`; re-verify at build time in case they moved.

| # | Site | What it does |
|---|---|---|
| 1 | `wait_for_stable_idle_resources` (`:1287`) via `resources_within_limits` (`:1508`) → `resources_over_limits` (`:1462`) | waits for the sample to come inside the ceiling; expires at `RESOURCE_CLEANUP_HORIZON` |
| 2 | `collect_node_resource_evidence` (`:1515`, ceiling at `:1529`) | hard `bail!("node N leaked resources: baseline …, post …")` at record time |
| 3 | `validate_cycle_resources` (`:2532`) | re-checks every recorded cycle offline against its own recorded baseline and the recorded margins |
| 4 | `validate_role_campaign` (`:2412`–`:2433`) | the baseline must be byte-identical across all 20 cycles |

Site 3 is what makes the artifact self-proving: the evidence file can be
validated by anyone later, without the harness. Site 4 is what stops a
baseline that follows the counts.

The constants: `RESOURCE_CLEANUP_HORIZON` 60 s · `RESOURCE_SAMPLE_INTERVAL`
3 s · `RESOURCE_STABLE_SAMPLES` 2 · `RESOURCE_BASELINE_WARMUP_CYCLES` 1 ·
`THREAD_MARGIN` / `SOCKET_MARGIN` / `OWNED_ASYNC_TASK_MARGIN` all 0. The
margins and the horizon are recorded in the artifact and pinned by its
identity check, so changing one is a deliberate act with a visible record —
that part of the design is right and should survive whatever is chosen.

Pinning tests that will move with any option:
`one_extra_socket_after_quiescence_is_rejected` (`:2950`),
`one_extra_thread_after_quiescence_is_rejected` (`:2959`),
`every_persistent_source_voter_requires_resource_evidence` (`:2968`),
`persistent_source_resource_baselines_cannot_be_rebased_between_cycles`
(`:2975`), `owned_async_task_growth_has_no_resource_slack` (`:3161`),
`schema_is_closed_and_valid_draft_2020_12` (`:3290`).

---

## 4. Option A — assert growth across the campaign

### 4.1 What it asserts

The last recorded cycle's counts do not exceed the first recorded cycle's,
per node and per resource, with margins still at zero. The per-cycle ceiling
becomes recorded evidence rather than a gate.

### 4.2 What changes

Sites 1 and 2 stop failing a cycle and start recording it. Site 3 keeps
validating that each cycle's recorded numbers are internally consistent but
stops enforcing the ceiling per cycle. Site 4 is unaffected in spirit — the
baseline still may not be rebased — but the campaign-level comparison becomes
the assertion, and needs its own validator rule so the artifact stays
self-proving. The schema keeps `baseline` and `post_quiescence` per node per
cycle; it is the *interpretation* that moves, which is why the closed-schema
test survives but four of the six pinning tests need rewriting.

Confined to `plurx-cluster-check`. No vendored code, no daemon change.

### 4.3 Detection math

Noise amplitude is ±1 socket and ±1 owned task per peer, ±2 threads. A leak
of one socket per cycle shows as **+19 between cycle 1 and cycle 20** — an
order of magnitude outside the noise. A leak has to be slower than one unit
per 20 cycles to hide, and a leak that slow is invisible to the current check
too, since the current check cannot finish a campaign to notice it.

### 4.4 Good and bad

**Good.** It is the invariant actually wanted; the per-cycle ceiling was only
ever a proxy for it. Immune to *both* noise sources, so it needs no separate
answer for thread jitter. Nothing is weakened — margins stay at zero, the
comparison just spans 20 cycles instead of one. It makes the evidence
stronger: "these counts did not grow over twenty recoveries" is a better
claim than "this cycle was ≤ one warmed sample". Entirely inside our own
crate.

**Bad.** Per-cycle attribution is lost — a leak is reported at the end, not
at the cycle that caused it. (#123 puts the per-cycle series in the log, so
the step is still visible, but the *assertion* no longer names it.) It is the
larger diff: four enforcement sites and four pinning tests. And it leaves the
lying `idle` predicate in place — the campaign stops being broken by it, and
nothing else that depends on `operation_owns_work` gets better.

---

## 5. Option C — wait for the transports to be released

### 5.1 What it asserts

The same per-cycle ceiling as today, but readiness means the recovery's
per-peer transports are actually released, not that `operation_owns_work`
cleared. The drain stops being sampled.

### 5.2 What changes

There is no fact to hang this on today. `SnapshotTransportObservation`
(`vendor/hiqlite/src/transport_status.rs:95`–`119`) carries
`operation_owns_work`, `phase`, `attempt_count`, `reconnect_count`,
`retry_count`, ages and byte counters — nothing about whether the connection
is gone. `SnapshotTransportStatus` carries a process-wide `owned_async_tasks`
and no per-recovery breakdown.

So C is: a new lifecycle fact in vendored hiqlite, plumbed through the status
route, consumed by site 1. Sites 2, 3 and 4 are untouched, and so is the
schema.

### 5.3 Good and bad

**Good.** It fixes the cause. A readiness predicate that reports idle while
its connections are up is wrong independently of this lane, and everything
downstream of `operation_owns_work` inherits that. Per-cycle attribution
survives, zero margins survive, the artifact schema does not move, and the
pinning tests keep their meaning.

**Bad — and this is the part that decides it.** *C alone probably does not
turn the lane green.* It removes the drain and leaves thread jitter, and
thread jitter alone breaks a zero-margin per-cycle ceiling: a trial run with
a two-warmup ceiling baseline **and** a 180 s horizon — both drain remedies —
still failed on `node 4 threads 10 > 9`, with no recovery in flight.

The second cost is where the change lives. `vendor/hiqlite` is a maintained
fork; a new status field is a patch carried against upstream indefinitely, in
the file PR #71 already rewrote by 3,050 lines. That is the most expensive
kind of change in this repo to keep.

If C is chosen alone it needs a third piece for threads. The cheapest is to
exempt thread counts from the per-cycle ceiling and assert them
campaign-level — which is Option A for one counter, and rather makes the
point.

---

## 6. Option B — a per-peer allowance (recorded, not recommended)

Keep the per-cycle ceiling; set `SOCKET_MARGIN` and `OWNED_ASYNC_TASK_MARGIN`
to one per peer.

Cheapest possible change: two constants, and the artifact identity check
self-adjusts because the margins are recorded. It also opens the margin
exactly where the leak it exists to catch would live — one connection and one
owned task per peer is precisely the shape of a per-peer transport that stops
being released. It is written here so it is not re-proposed as an obvious
quick win. It is the one option that makes the lane green while making the
check less able to do its job.

---

## 7. A and C together

They are not alternatives at the same level. **A changes what is asserted; C
fixes what is measured.** A makes the lane pass and subsumes thread jitter.
C makes `idle` honest and helps everything else that reads it.

Done together, C narrows A rather than replacing it: with the drain gone, the
campaign-level comparison sits on a quieter signal and could later tighten
toward per-cycle again with evidence rather than hope. Sequenced A first, then
C, each lands independently and neither blocks the other.

---

## 8. Recommendation

**A, and C afterwards if the lane is to carry real weight.**

A is the only single option that ends with a green lane and an assertion
worth having. C is the only option that fixes a predicate that is wrong on its
own terms. B is the trap.

If exactly one thing is built: A.

---

## 9. Non-goals — already tried, do not re-propose

Each of these was run to a real result on 2026-09-07. The findings document
has the detail.

- **Raising `RESOURCE_CLEANUP_HORIZON` alone.** A leak does not drain, so
  more time buys nothing against the condition that matters. Run 546 stayed
  over baseline for the full sixty seconds.
- **Relaxing the two-identical-samples rule.** The equality is not what
  fails; the ceiling comparison is.
- **Baseline as the ceiling of two warmups.** Removes the socket and
  owned-task failures exactly as intended, then fails on thread jitter.
- **That, plus a 180 s horizon.** Same thread failure. This is the pair of
  results that proves the per-cycle ceiling is the problem, not its tuning.
- **Regenerating the baseline per cycle.** Site 4 refuses it, correctly: a
  baseline that follows the counts cannot detect growth.

---

## 10. Acceptance — what "done" looks like

Whichever option is chosen, the same three things:

1. `make ci-rust-gate` clean, and
   `cargo test -p plurx-cluster-check transport_recovery::tests --lib` at its
   declared count — the count is pinned in two places, the `Makefile` target
   and `test_transport_recovery_campaign_is_a_persistent_affected_linux_gate`
   in `tests/operations/test_contracts.py`, and a bare Makefile edit fails
   preflight.
2. A real bounded run, not just unit tests:
   `./target/debug/plurx-cluster-check transport-recovery-voter-smoke 3`
   with `PLURX_BUILD_SHA` bound at build time. Roughly four minutes per
   recovery on two cores.
3. **A mutation that proves the check still bites.** Whatever the new
   assertion is, add a unit that fails when a node's counts are made to grow
   the way a real leak would, and check that it actually fails — a contract
   change that also removes the teeth is worse than the false positive it
   replaced.

And one thing to fix regardless of the option, because it cost a full local
reproduction to work around: a failing campaign aborts before
`publish_artifact_atomically`, so the workflow's upload of
`cluster-transport-recovery.json` finds nothing and the log is the only
channel a failure has. #123 puts the per-cycle sample in that log.
