# Live TV said "all slots are busy" with every tuner idle — RCA and fix

**Status:** built · 2026-09-28 · branch `fix/live-tv-background-admission`

Paul reported (2026-09-27, web UI and iOS): pressing a channel intermittently
answers *"All Live TV slots are busy. Close another Live TV session or try
this channel again shortly."* The HDHomeRun FLEX 4K had all four tuners idle
each time it was checked, and `plurx_live_tv_sessions{state="active"}` on the
owner read 0.

## What actually refused the viewer

The owner (nynuc) logged the refusal itself:

```
Live TV session ended channel=6.1 reason="tuner_capacity"
  cause=transcode capacity is temporarily unavailable: background encoding
  did not yield within 5.0s; try again in a moment  duration_s=8 tuner_bytes=0
```

Three defects stacked to produce that line, and a fourth to mislabel it.

### 1. The subtitle backfill held the whole software encoder pool while walking dead jobs

`work_subtitle_queue` took a fragment admission — which reserves the node's
entire software CPU budget at Background priority — **before** claiming a
job, and then walked up to 128 candidates under it. Each claim of a zombie
row failed with the trigger's `RAISE(ABORT, 'subtitle demand no longer
claimable')`, and `claim_with_resolution_inner` treats every store error as
an ambiguous, possibly-committed write: it polls `resolve_claim` every
250 ms for the full 30 s job lease before giving up on that candidate.

On nynuc the same eight zombie job ids came back on every candidate page.
The owner's log shows them being resolved at exact 30 s intervals —
23:49:39, 23:50:09, … 00:07:50 — eight in a row, four minutes, with the
software pool reserved for the whole walk and no ffmpeg running. The two
refusals at 00:08:01 and 00:08:14 fall inside that walk; "reading source"
for the next real job appears at 00:08:21, seconds after it ended.

### 2. A definite refusal was treated as an ambiguous write

The trigger's abort is the store saying *this demand is gone* — cancelled,
already settled, or its source superseded since the job was admitted. The
statement rolled back, so the job stayed `queued`, and nothing ever retired
it. `claim_job` now recognises that message (it is the repository's own SQL,
exported as `background_jobs_subtitle::DEMAND_GONE`), retires the row at the
revision the claimant saw (`state = 'cancelled'`, `last_error_code =
'subtitle_demand_gone'`) and answers `ClaimOutcome::Cancelled`, which the
walker handles immediately. The row never returns to a candidate page. A
`running` job whose lease has lapsed (a takeover candidate) in the same
situation is retired the same way.

Two things the review made this carry. First, the store's
`background_subtitle_settled` trigger fires on the retirement and used to
rewrite the demand's attempt row at its fence to `canceled` — for the fleet's
shape that is the *published* attempt of a finished request. The trigger's
attempt update is now guarded the way its request update already was (only a
demand still `queued`/`running`), shipped as **SQLite schema v83 / cluster
schema v61**, each of which re-runs the idempotent adapter schema whose
`DROP TRIGGER` + `CREATE TRIGGER` carries the new body; the store test
writes the published attempt and proves it survives. Second, the match is on
the trigger's message text (`StoreError::to_string()`); on the replicated
backend that is proven by the owner's own log line — `error=database error:
Sqlite: subtitle demand no longer claimable` came from nynuc's hiqlite
store — and the match is gated to `SubtitleExtract` claims.

### 3. A live start that waited out background work was refused

`admit_live` announced a live waiter, waited the cooperative window
(`ADMISSION_WAIT` = 5 s for Live TV) for the background holder to yield, and
then refused the viewer with "background encoding did not yield". The design
note on `Admission::WaitingForBackground` — and OPERATIONS.md's
troubleshooting row — said this was deliberate: "absence after five seconds
means that worker is stuck rather than permission to start beside it".

That ruling is reversed here, and the reversal is the one decision in this
change Paul should look at. A background holder that has not yielded in five
seconds is not a busy encoder about to finish; it is a worker in a phase that
never looks at the pool (a candidate walk, a store wait, a hash). Refusing
the viewer on its behalf turns a background defect into a foreground outage,
and the viewer has no way to know why. The same spirit as the Live TV tuner
ruling of 2026-09-13: a possibly-held resource is never a reason to refuse a
viewer.

What a live start now does when the window expires with background still
owning a pool:

- **Hardware within the cap.** `try_acquire_over_background` takes a live
  hardware slot if `hardware_used < max` — the stuck background slot still
  counts, so the GPU is never oversubscribed beyond `max_hw_sessions`.
- **Otherwise the ordinary software decision**, shared with the normal path
  (`software_decision`): a class measured or shaped as hopeless in software
  is still refused, honestly, because that refusal is about the stream. A
  class that fits takes its CPU through `SwPool::take_over_background`,
  which discounts the stuck background reservation **and nothing else**:
  every live reservation still counts against the budget, so a pool other
  viewers have spent is refused with the ordinary bounded answer ("spent by
  live sessions"). The review's first finding was that an unbounded forced
  take here would have let one stuck worker pile forced permits until every
  session ran below realtime; the bound is what keeps this a reclaim of the
  worker's share rather than a licence to oversubscribe. The mixed pipeline
  (hardware slot plus a software decode's cores) is taken whole or not at
  all, as the ordinary bundle is.
- This applies to **every `Priority::Live` start** — VOD as much as Live
  TV — since both go through `admit_live`. Live TV is where it was reported.
- The admitted permit is a live one, so from that instant every background
  holder is `background_blocked` and stops at its next yield check.
- Only `Priority::Live` earns this. A speculative start (prepared
  successor) has nobody waiting on it and keeps the bounded refusal.
- One WARN line names the pool, the threads and the wait, and
  `plurx_transcode_background_overrun_total{pool}` counts it. **Zero is the
  design working.** A rising count means some background worker holds a
  permit through a phase that never checks the pool — go find it.

### 4. The Live TV layer called an encoder refusal a tuner refusal

`LiveTvError::Capacity` was the only capacity variant, its code is
`tuner_capacity`, and every client renders that as "All Live TV slots are
busy" — copy that also offers to stop a recording and lists holders, none
of which applies when the tuners are free. A transcode admission failure is
now `LiveTvError::EncoderCapacity` → code `encoder_capacity`, HTTP 503,
retry advice `later`, and it rides the same `holders`/`watchable`-free
envelope through `wire_api_error`, the cluster placement path and the
internal wire. The three clients each carry copy for it, obliged by a new
row in the shared start-cases fixture (`tests/playback/live-tv-start-
cases.json`), which every client's "every render key has copy" test reads.
The session-end metric reason follows the code, so
`plurx_live_tv_session_ends_total{reason="encoder_capacity"}` now separates
this from real tuner exhaustion.

## What changed, by commit

| Area | Change |
|---|---|
| `plurx-core` store | `claim_job` retires a subtitle job the demand trigger refuses and answers `Cancelled`; `background_jobs_subtitle::{DEMAND_GONE, demand_gone, DEMAND_GONE_CODE}`. Store test `a_subtitle_claim_the_demand_trigger_refuses_retires_the_zombie_job` grows the exact fleet shape (request ready, job queued) and proves the claim heals it. |
| `subtitle_work.rs` | Claim first, then `admit_fragment`; a claim the pool then refuses is settled `Yield` (5 s) and released. `fragment_worker_may_start` — the admission's own predicate (pool idle **and** the shared heavy-worker gate free) — before the claim, so neither a viewer nor another heavy worker causes claim/yield churn (a claim charges the demand an attempt and two replicated writes). |
| `background_jobs_subtitle.sql`, `sqlite/mod.rs`, `hiqlite.rs` | The settled trigger's attempt update guarded to open demand; SQLite v83, cluster v61. |
| `admission.rs` | `try_acquire_over_background`, `admit_over_background`, `software_decision` (shared). |
| `transcode/manager/start.rs` | `admit_live` admits over background after the window for `Priority::Live` on both the hardware and software routes; `note_background_overrun` log + counter. |
| `telemetry.rs` | `plurx_transcode_background_overrun_total{pool="hardware"\|"software"}`. |
| `live_tv.rs`, `http/live_tv.rs`, `http/internal_live_tv.rs`, `http/live_tv_cluster.rs` | `EncoderCapacity` / `encoder_capacity`. |
| `web/live-tv.js`, Android `LiveTvApi.kt`, Apple `LiveTv.swift` | Copy for `encoder_capacity`; Android build 133, Apple build bumped. |
| `tests/playback/live-tv-start-cases.json` | The `encoder_capacity` answer row. |
| `docs/API.md`, `docs/OPERATIONS.md` | The code, and the troubleshooting row rewritten for the new behaviour. |

## What was not changed, and why

- The 5 s cooperative window itself. Healthy holders (the pre-transcode
  producer polls `live_is_waiting` at `PRODUCER_POLL`; the subtitle pass at
  250 ms) yield well inside it; the window only bites on a wedged holder,
  and after this change it costs the viewer a delay, not a refusal.
- The other `admit_fragment`-then-claim walkers (fragment index, artwork,
  library, semantic, integrity). They take the admission per candidate, so
  a single ambiguous claim costs at most one lease under the pool, and the
  admission-layer change now covers them from the viewer's side. Worth the
  same reorder if `plurx_transcode_background_overrun_total` ever moves.
- How the eight zombie rows came to exist. Each had its analysis request
  `ready` with `attempts = 1` and a job `queued` at revision 0, admitted in
  one 300 ms burst on 2026-09-26 04:36 UTC (the intents loop's cadence). The
  enqueue fence refuses a job for a ready request and the dedupe key folds a
  second admission into a running one, so the ordinary paths cannot grow it;
  a boot-time legacy drain or a migration replay is the likeliest origin.
  The claim path now heals the shape whatever produced it, and
  `last_error_code = subtitle_demand_gone` on the retired rows is the
  breadcrumb for whoever chases the origin.
- `claim_with_resolution_inner`'s treatment of *every* store error as
  ambiguous. A typed distinction between "refused" and "lost" would be the
  right general fix; this change handles the one message that was
  producing zombies in production.

## Verification

- Store test `a_subtitle_claim_the_demand_trigger_refuses_retires_the_zombie_job` (the fleet shape written directly: no public store path admits a job for a ready request, so how the eight rows arose is still open — see below).
- `live_admission_starts_over_a_background_permit_that_does_not_release`
  (hardware arm, the live permit parks background at a cap with headroom,
  the counter moves),
  `live_admission_over_background_falls_to_software_when_the_cap_is_held`
  (the `Admission::Software` arm),
  `live_admission_over_background_takes_software_forced_when_hardware_is_capped`
  (the pure-software route),
  `live_admission_over_background_is_still_bounded_by_live_usage` (a pool
  spent by viewers is refused, background hold or not),
  `speculative_admission_is_still_refused_while_background_holds` and
  `speculative_software_admission_is_still_refused_while_background_holds`.
- Adversarial review of the branch (one round, five findings, all
  addressed): the unbounded forced take, the settled trigger clobbering the
  finished attempt, the SQLite-only proof of the message match, claim/yield
  churn from the reorder, and tests that did not reach the arms they named.
  Rollout notes from it: an ingress older than this build folds
  `encoder_capacity` to its fallback code, and clients older than Android
  133 / Apple 194 print the raw sentence; a healthy producer whose
  checkpoint-and-kill runs past five seconds counts as an overrun.
- Live TV HTTP: `encoder_capacity` code, 503, `retry: later`, preserved
  through the ingress relay.
- Fleet: after deploy, `plurx_transcode_background_overrun_total` on nynuc
  is expected to move while the subtitle backfill still has zombie rows to
  retire on first contact, then settle at a constant. The eight zombie ids
  from the 2026-09-27 log should stop appearing in `docker logs` within one
  candidate walk.
