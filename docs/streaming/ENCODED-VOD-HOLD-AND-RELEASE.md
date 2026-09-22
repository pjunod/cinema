# Encoded VOD hold and release — stop the encoder instead of killing it, and give it up when someone is waiting

**Status:** M1-M3 implemented; controlled M0/M4 fleet traces pending · **Executes:** §2.6, F-stream-6 (the hold
half), assessment correction 5, §5.1 item 8 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Companion to [VOD-ENCODING.md](VOD-ENCODING.md) ("Capacity belongs to a
process, not a rendition" is the section this plan must not contradict) and
[VOD-PRESENTATION-PLAN.md](VOD-PRESENTATION-PLAN.md) (§2.4 is the ahead-window
contract `prodsched` implements). Read §2 before touching anything: the
scheduler (`prodsched.rs`) and the executor (`prodexec.rs`) are pure, tested
tables, and every change here lands in those tables first, with the
late-arrival and contention cases as tests, before `vodserve.rs` changes one
line. Work §5 in order. If a step seems to require the driver to signal a
process that `prodexec` did not ask it to, or to hold a permit past reap,
stop and flag it.

**Correction to the review:** none on the mechanism. Two refinements the
code adds. First, `vodserve.rs:6296` sets `WorkingSet.held` from
`matches!(belief, Producer::Stopped { .. })`, which today is only true for
copy producers stopped for `Hold::Ahead`; once encoded producers also stop,
that flag would lower the working-set entry line to half the budget for
every stopped encoder, which is not what the hysteresis at
`prodsched.rs:225-237` was written for. §3.1 gives the ahead hold its own
flag. Second, the review's "1,400 launches" and "7.6 s" are models, as the
assessment says; §5.0 counts real generations on media1 before anything is
sized.

## 1. Objective

1. An encoded rendition whose producer has run `AHEAD_HORIZON_SECONDS`
   (180 s) past every reader is **SIGSTOP'd**, as copy renditions already
   are, instead of SIGKILL'd and respawned — keeping its decoder state, its
   x264 rate-control history and its 2 s preroll, and paying no
   `recipe_engine_is_current` walk on resume.
2. A stopped producer resumes only when the reader has consumed enough that
   the next run is a burst of at least `AHEAD_RESUME_SECONDS` (90 s) of film
   — a low-water mark — instead of one segment after the frontier moves.
3. A stopped encoded producer **yields** its encoder permit when the shared
   pool reports a live start waiting (`Admissions::live_is_waiting`), with a
   bounded re-evaluation so the yield happens whether or not the waiter is
   a VOD rendition, and the pure tables cover "second viewer arrives after
   the first has stopped" and fairness under capacity contention.
4. Generations per session are counted on media1 before and after.

## 2. Contract today

Re-verify every line number at build time; they are from `88a3957a`.

### 2.1 The scheduler's ahead hold has no hysteresis

[`prodsched.rs`](../../crates/plurxd/src/prodsched.rs). `AHEAD_HORIZON_SECONDS
= 180` (`:39`), `REPOSITION_GAP_SECONDS = 60` (`:44`). `Position` (`:154-195`)
carries `produced_through`, `positioned_at`, `seconds_per_segment` and a
`WorkingSet { used_bytes, budget_bytes, held }` — `held` is documented
(`:203-212`) as "whether the producer is already stopped **for this
bound**", the working set. There is no field saying "stopped for Ahead".

`decide` (`:266-424`): with nothing owed, ahead-fill runs from
`position.reach()`; at `:372-374`:

```rust
if gap > furthest.saturating_add(horizon) {
    return stop(position.produced_through, furthest, horizon);
}
```

and `stop` (`:470-478`) returns `Suspend { reason: Hold::Ahead { horizon } }`
when `through >= furthest + horizon`, else `Idle`. So the producer is
suspended at exactly `frontier + 90 segments` (2 s segments) and, because
the same comparison is made on every pass with no memory of the hold, the
first pass after the frontier advances one segment sees `gap <= furthest +
horizon` and produces one segment, then suspends again. That is the
"resumes as soon as one segment of gap opens" the review describes.

The one hysteresis that exists is `WorkingSet::over_by` (`:225-237`): enter
at `budget`, release at `budget / 2`, keyed on `held`.

### 2.2 The executor turns the hold into a signal, and forgets nothing else

[`prodexec.rs`](../../crates/plurxd/src/prodexec.rs). `Producer::{Absent,
Running, Stopped { produced_through, positioned_at, reason }}` (`:53-69`).
`Step::{Nothing, Start, Resume, Stop, Terminate { why }, Restart, MakeRoom,
Report}` (`:113-146`); `Termination::{Idle, IndefiniteHold}` (`:150-156`).
`clears_on_its_own(Hold::Ahead) == true` (`:167-172`).

`next_step` for `Action::Suspend` (`:243-268`): `Ahead` + `Running` →
`Stop`; `Ahead` + `Stopped` → `Nothing` (`:250-254`, "do not re-signal");
non-clearing hold + any live producer → `Terminate { IndefiniteHold }`.
`Action::Produce { next }` + `Stopped { produced_through }` → `Resume` when
`next > through`, else `Restart` (`:194-211`). `Action::Idle` + `Stopped` →
`Terminate { Idle }` (`:178-185`). `after` (`:260-285`) maps `Stop` to
`Stopped { reason: Hold::Ahead { horizon: 0 } }` and `Terminate` to `Absent
{ produced_through }`.

The module header (`:13-27`) records the design decision this plan bends:
"SIGSTOP is only the right answer for a hold that clears on its own and
soon. `Hold::Ahead` does … `Hold::WorkingSetFull` and `Hold::NoRoom` do not"
— and it quotes `transcode.rs` on why a stopped ffmpeg is a problem for a
*waiting viewer*: "a stopped ffmpeg still holds the hardware codec session,
so the viewer this is yielding to would be blocked by a process that is
doing nothing". Nothing in `prodexec` knows whether a viewer is waiting.

### 2.3 The driver overrides `Stop` for encoded renditions

[`vodserve.rs:6314-6322`](../../crates/plurxd/src/vodserve.rs), in
`driver_pass`:

```rust
let step = if rendition.recipe.encoding.is_some() && matches!(step, Step::Stop) {
    // A full ahead window is no reason to reserve scarce encoder
    // capacity while another viewer waits. Encoded restarts reproduce
    // the same video grid and audio phase, so release instead of SIGSTOP.
    Step::Terminate { why: Termination::IndefiniteHold }
} else {
    step
};
```

Unconditional: "while another viewer waits" is the reason, but no waiter is
consulted. `Terminate` goes through `ProducerSlot::perform`
(`prodrun.rs:225-233`): `child.kill().await` (SIGKILL + reap), then
`inner.resources = None`, which drops the boxed `EncodePermit` — that is
where the permit is released, and it is released only after reap, which is
the VOD-ENCODING.md rule.

The next `Produce` then goes `Absent → Start { at }` → `try_permit`
(`:6323-6350`) → `spawn_generation` (`:6733`), which pays
`recipe_engine_is_current` (`:6738`; a stat of every object in the ffmpeg
dependency closure, plus the font closure for text burns —
[FONT-ATTESTATION-AND-BLOCKING-IO.md](FONT-ATTESTATION-AND-BLOCKING-IO.md)),
`reopen_encoded_audio`, and a fresh ffmpeg with decoder init and the
preroll VOD-ENCODING.md §"Audio owns a global sample lattice" describes.
The log line is `"spawned a producer generation"` (`:6860`) with
`rendition = <key>`.

### 2.4 The waiting signal exists, but the stopped producer never hears it

`Encoding::is_waiting` (`vodencode.rs:160-163`) is true while **this**
rendition holds a `LiveWait` or a policy-read retry is pending; it drives
the driver's 250 ms re-check (`vodserve.rs:6167-6181`) while *this*
rendition is queued. It says nothing about other renditions.

`Admissions::live_is_waiting` (`admission.rs:580-586`) is
`permits.live_waiting > 0`; `wait_for_slot` (`:563-573`) increments it and
the returned `LiveWait` guard decrements on drop. A VOD encoder at
`Priority::Live` registers one in `try_permit_after` (`vodencode.rs:127-129`)
when it fails to get a bundle; rolling transcodes register at
`transcode.rs:19473` and `:20118`. The background pretranscode worker polls
`live_is_waiting()` every `PRODUCER_POLL` and kills its child when true
(`transcode.rs:17626-17634`) — that is the existing precedent for "yield on
the pool signal", and its comment is the same sentence `prodexec` quotes.

The driver loop (`vodserve.rs:6165-6203`) wakes on `rendition.wake`
(`kick()`), or every 250 ms while `is_waiting()`. A rendition whose producer
is `Stopped` and whose encoding is not waiting sleeps on `wake` alone.
`kick_all` (`:6138-6159`) is called on working-set changes (`:6009, 6129,
6421`), never on admission failure.

## 3. Change

### 3.1 `prodsched`: an ahead low-water mark with its own flag

```rust
/// Once stopped for `Hold::Ahead`, a producer resumes only when its lead
/// over the furthest reader has shrunk to this, so each run is a burst
/// of at least (AHEAD_HORIZON_SECONDS - AHEAD_RESUME_SECONDS) seconds of
/// film. Without it the producer wakes one segment after the frontier
/// moves, produces one segment, and stops — once per segment for the film.
pub const AHEAD_RESUME_SECONDS: u32 = 90;
```

`Position` gains `ahead_held: bool` — "the producer is currently stopped for
`Hold::Ahead`" — kept apart from `WorkingSet.held` for the reason in the
correction above. `stop()` becomes:

```rust
fn stop(produced_through: Option<u32>, furthest: u32, horizon: u32,
        resume: u32, ahead_held: bool) -> Action {
    match produced_through {
        Some(through) if through >= furthest.saturating_add(horizon)
            || (ahead_held && through > furthest.saturating_add(resume)) =>
            Action::Suspend { produced_through: through,
                              reason: Hold::Ahead { horizon } },
        _ => Action::Idle,
    }
}
```

and the ahead-fill branch at `:372` uses the same predicate: while
`ahead_held`, a gap is "not owed yet" until `through <= furthest + resume`.
`resume = position.horizon_segments(AHEAD_RESUME_SECONDS)`. The blocked
branch (`:308-360`) is untouched: a blocked request still outranks the
hold, so a seek or a stall never waits on the low-water mark. Copy
renditions get the same hysteresis; for them it removes a
SIGSTOP/SIGCONT pair per segment, nothing else.

`ahead_held` is a **latch on the rendition**, not a reading of the belief:
`Rendition.ahead_hold: AtomicBool`, set by the driver when it performs
`Step::Stop` or `Step::Terminate { YieldToWaiter }` (§3.2), cleared when it
performs `Start`, `Restart` or `Resume`. It has to outlive the process
because a yielded producer is `Absent`, and an `Absent` producer with no
latch would be re-admitted one segment after the frontier moves — ahead of
the waiter it just yielded to. `WorkingSet.held` is filled from
`matches!(belief, Producer::Stopped { reason: Hold::WorkingSetFull { .. } |
Hold::NoRoom { .. }, .. })` — today always false, because those holds
terminate; the field keeps its documented meaning. For an `Absent` producer
with the latch set, `next_step(Absent, Suspend { Ahead })` is `Nothing`
(`prodexec.rs:255`), so nothing is spawned until the low-water mark.

### 3.2 `prodexec`: an explicit stopped→release transition on contention

Keep `next_step(producer, action)` as it is — every existing test stays
valid — and add one pure function the driver calls after it:

```rust
/// Why a producer is being killed rather than stopped (extended).
pub enum Termination { Idle, IndefiniteHold,
    /// Stopped for a hold that would clear, but a live start is queuing
    /// for the pool this producer's permit belongs to. The permit goes
    /// back now; the next `Produce` re-admits behind the waiter.
    YieldToWaiter }

/// What the shared pool says, observed by the driver on this pass.
#[derive(Clone, Copy)]
pub struct Contention { pub live_waiting: bool, pub holds_permit: bool }

/// A stopped producer that holds a permit yields it to a queuing live
/// start. Pure: the driver observes the pool, this decides.
pub fn yield_step(producer: Producer, step: Step, contention: Contention) -> Step {
    if !contention.holds_permit || !contention.live_waiting { return step; }
    match (producer, step) {
        // About to stop, or already stopped, for a clearing hold: give the
        // codec session back instead.
        (Producer::Running { .. }, Step::Stop)
        | (Producer::Stopped { .. }, Step::Nothing) => Step::Terminate {
            why: Termination::YieldToWaiter,
        },
        // Resuming, restarting, terminating, or making room: the decision
        // already moves the process; a yield on top would be a second op.
        _ => step,
    }
}
```

`after(_, Terminate { YieldToWaiter })` is `Absent { produced_through }`
like the other terminations; no new belief state. With the `ahead_hold`
latch set (§3.1) the next passes get `Suspend { Ahead }` → `Nothing`
(`:255`) until the low-water mark, so the yielded producer does **not**
re-admit one segment later; when `decide` finally says `Produce`,
`next_step(Absent, Produce)` is `Start` and `try_permit` competes for the
pool like any live start. That is the fairness property §5.2 tests: a
yielded producer cannot take the permit back before its own reader needs
it. A blocked request (seek, stall) still outranks the latch, because the
blocked branch of `decide` never reads it.

`holds_permit` is `rendition.recipe.encoding.is_some()` (copy producers hold
no permit and are never yielded — the asymmetry the `prodexec` header
already names). `live_waiting` is `encoding.admissions.live_is_waiting()`
(each `Encoding` carries an `Admissions` handle, `vodencode.rs:29`) read once
per pass; a copy rendition has no `Encoding` and passes `holds_permit:
false`.

### 3.3 `vodserve`: remove the override, add the observation, add the wake

1. Delete the `Step::Stop → Terminate { IndefiniteHold }` override at
   `:6314-6322`. Replace with
   `let step = crate::prodexec::yield_step(belief, step, contention);`.
2. `capacity_hold` (`:6302-6308`) is unchanged: `YieldToWaiter` is not a
   capacity hold on this rendition and must not show as one in `status`.
3. Wake sources for the stopped producer, both needed:
   - **Event**: in `try_permit_after`, when `try_admit_bundle` returns
     `None` and a `LiveWait` was just registered, the VOD registry calls
     `shared.kick_all()` (it already does so for working-set changes).
     This covers a second **VOD** viewer.
   - **Bounded poll**: the driver loop's `select!` at `:6172-6175` gains a
     third condition: while `belief` is `Stopped` and the recipe is
     encoded, also wake every `STOPPED_ENCODER_POLL = 1 s`. This covers a
     rolling-transcode live start (`transcode.rs:19473`), which cannot kick
     the VOD registry. 1 s matches the precedent's order of magnitude
     (`PRODUCER_POLL`) and is under the 5 s a queued live start waits
     before it gives up on hardware (`QUEUE_WAIT`, `admission.rs:46`); it
     costs one mutex read per stopped encoder per second. A constant with
     its reason, not a setting. §7.4 asks whether 1 s is close enough to
     5 s to matter.
4. `Step::Terminate { YieldToWaiter }` is performed exactly as the other
   terminations (`:6380-6385`): `gen_epoch` bump, `slot.perform`, which
   kills, reaps and drops the permit in that order.
5. The `ahead_hold` latch (§3.1): `store(true)` after a successful `Stop`
   or `Terminate { YieldToWaiter }`, `store(false)` after a successful
   `Start`, `Restart` or `Resume` — after `slot.perform` returns `Ok`, never
   before, for the same reason `after` is separate from `next_step`: a
   failed signal must not move the latch.

### 3.4 What a stopped encoder holds, and for how long

A SIGSTOP'd x264/x265 process keeps its RSS (hundreds of MB for 2160p) and
its hardware session. Bounds on that: the yield above (a waiter takes it
within ≤1 s of queuing); `SESSION_IDLE_TTL = 300 s` (`vodserve.rs:72`) after
which the reader detaches, demand empties, `decide` returns `Idle`, and
`next_step(Stopped, Idle)` is `Terminate { Idle }` (`:178-185`) — unchanged;
and `Action::Suspend` with a non-clearing hold on a `Stopped` producer, which
still terminates (`:257-262`). A paused viewer therefore holds an encoder
for at most five minutes, then it is reclaimed; that is the trade §5.0's
measurement and §7.1 weigh.

## 4. Guardrails (non-goals)

- **Not "SIGSTOP for everything".** `WorkingSetFull` and `NoRoom` still
  terminate; only `Hold::Ahead` stops, as the `prodexec` header decided.
- **The permit is released only after reap.** VOD-ENCODING.md: "The
  producer slot releases it only after confirmed child reap". `yield_step`
  produces a `Terminate`, which `ProducerSlot::perform` implements as
  `kill().await` then `resources = None`. No path drops the permit while a
  child is alive.
- **No change to the audio preroll or the frame grid.** F-stream-6's
  disposition rejects "two seconds of audio preroll to two AAC frames".
  This plan removes restarts; it does not change what a restart does.
- **A blocked request still outranks every hold.** The low-water mark
  lives in the ahead-fill branch only.
- **Pure first.** No `vodserve.rs` change before the `prodsched`/`prodexec`
  tests in §5.1–5.2 exist and pass.
- **No new switch.** The behaviour is the documented intent of
  `prodexec.rs:151-164`; the override it removes carried the reason "while
  another viewer waits", and the waiter is now consulted. If a fleet
  observation in §5.4 shows memory pressure from stopped encoders, the
  answer is a lower `SESSION_IDLE_TTL` for encoded renditions or a smaller
  horizon — measured — not a gate.
- **Registry unification and the two-registries finding (§4.1) are out of
  scope.**

## 5. Milestones

### 5.0 M0 — count real generations on media1 (no code)

GPT prompt (paste to a session with fleet access):

> On media1, pick one encoded VOD session: start *Harbor Lights* in the web
> client at a quality that transcodes (not Original), note the rendition
> key from the first `spawned a producer generation` line after your start
> time, and let it play for 30 minutes without seeking. Then run
> `journalctl -u plurxd --since "<start>" --until "<start+30m>" | grep
> "spawned a producer generation" | grep "<rendition key>" | wc -l` and
> paste the count with the first and last five lines (timestamps kept).
> Also paste `grep -c "spawned a producer generation"` for the same window
> without the key filter, and say whether any other playback was active.
> Repeat once with a text-subtitle burn enabled.

Acceptance: two counts (with and without burn), per rendition, over a known
interval, recorded in this document's status note. This is the "before".

### 5.1 M1 — `prodsched` low-water mark

Code: §3.1. Tests in `prodsched.rs` (existing fixture: 7 s segments, so the
180 s horizon is 26 segments and the 90 s resume line is 13):

| Test | Asserts |
|---|---|
| `a_producer_stopped_for_ahead_does_not_resume_one_segment_later` | `ahead_held`, through = frontier + 25: still `Suspend { Ahead }` |
| `a_producer_stopped_for_ahead_resumes_at_the_low_water_mark` | `ahead_held`, through = frontier + 13: `Produce { next: through + 1 }` |
| `the_first_stop_is_still_at_the_horizon` | not `ahead_held`, through = frontier + 25: `Produce`; + 26: `Suspend` |
| `a_blocked_request_ignores_the_low_water_mark` | `ahead_held`, a `waiting_on(frontier + 1)` demand behind the producer: `Reposition`/`Produce` per the existing blocked rules, never `Suspend` |
| `ahead_held_does_not_lower_the_working_set_line` | `ahead_held = true`, `WorkingSet { held: false, used = budget * 3/4 }`: no `MakeRoom`/`WorkingSetFull` (the line is still `budget`) |
| `the_resume_line_is_seconds_not_segments` | 2 s segments: resume at 45 segments; 7 s: at 13 |

Acceptance: `cargo test -p plurxd prodsched` green; `make unit` green.

### 5.2 M2 — `prodexec` yield on contention

Code: §3.2. Tests in `prodexec.rs`:

| Test | Asserts |
|---|---|
| `a_stopped_encoder_yields_when_a_live_start_is_waiting` | `(Stopped { Ahead }, Nothing, { live_waiting: true, holds_permit: true })` → `Terminate { YieldToWaiter }` |
| `an_encoder_about_to_stop_yields_instead` | `(Running, Stop, waiting)` → `Terminate { YieldToWaiter }` |
| `a_copy_producer_never_yields` | `holds_permit: false` → step unchanged for every `(producer, step)` pair |
| `nobody_waiting_means_no_yield` | `live_waiting: false` → unchanged for every pair |
| `a_yield_never_replaces_a_move` | `(Stopped, Resume)`, `(Stopped, Restart)`, `(Running, Nothing)`, `(_, MakeRoom)` → unchanged |
| `a_second_viewer_arriving_after_the_first_stopped_gets_the_permit` | sequence: `after(Running, Stop)` → `Stopped`; pass with waiting → `Terminate { YieldToWaiter }`; `after` → `Absent { through }`; next pass with `ahead_held = true` and the frontier one segment on: `decide` → `Suspend { Ahead }`, `next_step(Absent, …)` → `Nothing` (no re-admission while still ahead) |
| `a_yielded_producer_re_admits_only_when_its_reader_needs_it` | continuing, `ahead_held = true`: `decide` at through = frontier + 14 → `Suspend`; at frontier + 13 → `Produce`; `next_step(Absent, Produce)` → `Start` — and not before |
| `a_blocked_request_outranks_the_latch_for_a_yielded_producer` | `Absent`, `ahead_held = true`, a `waiting_on` demand behind `produced_through`: `Reposition`/`Start`, not `Nothing` |
| `every_yield_step_is_a_fixed_point_when_repeated` | `yield_step(after(p, s), s', c)` twice gives the same step, in the style of `every_step_is_a_fixed_point_when_repeated` (`:608`) |

Acceptance: `cargo test -p plurxd prodexec` green; `make unit` green.

### 5.3 M3 — `vodserve` wiring, with the two-rendition contention test

Code: §3.3. Tests use `vodserve.rs`'s existing fake-child and
`attach_owned` harness with the hardware limit set to 1. The mechanism-level
cases may drive `driver_pass` directly. The rolling-live, yielded low-water,
and TTL acceptance cases must instead own and settle a real `spawn_driver`
task; the rolling-live case observes the armed and fired
`STOPPED_ENCODER_POLL` without sending a rendition kick. A `LiveWait` comes
from the fixture's `Encoding::admissions`:

| Test | Asserts |
|---|---|
| `an_encoded_producer_past_the_horizon_is_stopped_not_killed` | after the pass: belief `Stopped { Ahead }`, `last_child_pid` unchanged, no new `spawned a producer generation` |
| `a_second_rendition_arriving_takes_the_stopped_producers_permit` | rendition A stopped; rendition B's `try_permit` fails and registers a `LiveWait`; within one `kick_all` + one pass A's belief is `Absent`, its permit dropped, and B's next pass spawns |
| `a_rolling_live_start_also_releases_a_stopped_encoder` | no VOD kick: hold a `LiveWait` from the fixture's `Admissions::wait_for_slot()` directly; A yields within `STOPPED_ENCODER_POLL + one pass` (paused-time test) |
| `a_yielded_encoder_does_not_take_the_permit_back_while_still_ahead` | after A yields and B is running, drive A's passes: no `Start` until A's reader frontier crosses the low-water mark |
| `capacity_hold_status_stays_none_across_a_yield` | `rendition.capacity_hold` is `None` after `YieldToWaiter` |
| `the_ahead_latch_follows_the_performed_step` | set after `Stop` and after `YieldToWaiter`; clear after `Resume`, `Start`, `Restart`; unchanged when `slot.perform` returns `Err` |
| `an_idle_stopped_encoder_is_reclaimed_after_the_session_ttl` | reader detaches; after `SESSION_IDLE_TTL` the pass terminates with `Idle` (existing behaviour, now reachable for encoded) |

Also run `make vodencode-restart-check` (part of `make unit`) — it is the
explicit restart-continuity check, and this plan must not weaken it: a
resumed producer and a restarted one must still publish identical bytes for
the same entry.

Acceptance: `cargo test -p plurxd vodserve::tests::.*stopped.*
vodserve::tests::.*yield.*` green; `make unit` green including
`vodencode-restart-check`.

### 5.4 M4 — deploy and count again

Same GPT prompt as §5.0, after the deploy, plus:

> Also report, for the 30-minute window, `ps -o pid,stat,rss,etime -C
> ffmpeg` sampled every 60 s (a `T` in `stat` is a stopped encoder; note
> its RSS), and `curl -s http://10.42.0.10:32400/metrics | grep
> plurx_vod_producer_generations_total`.

Acceptance: generations per rendition per 30 min drop from the M0 count to
a number consistent with the low-water arithmetic (one burst per ~90 s of
playback at 1× — about 20 in 30 minutes plus the initial run, not 900);
no `T`-state ffmpeg older than `SESSION_IDLE_TTL` with no reader; no
`live start … timed out` in the journal while a stopped encoder existed.

## 6. Verification and rollout

- Focused: `cargo test -p plurxd prodsched` (M1), `cargo test -p plurxd
  prodexec` (M2), the named `vodserve` tests and `make
  vodencode-restart-check` (M3).
- Lane: `make unit` on every PR.
- Metric, added in M3 and rendered beside the `plurx_vod_*` gauges in
  `waitpool.rs:208`:
  `plurx_vod_producer_generations_total{kind="copy"|"encoded"}` (counter,
  incremented at `spawn_generation`'s `attach_owned`) and
  `plurx_vod_producer_terminations_total{why="idle"|"indefinite_hold"|"yield_to_waiter"|"restart"}`
  (counter, incremented in `driver_pass` where each `Terminate`/`Restart` is
  performed). Both label sets are closed enums. Reason: §2.6's rate was a
  model because nothing counted it; after this, the M0/M4 journal grep has a
  counter it can be checked against, and `yield_to_waiter` staying at zero
  on a busy node is the signal that the wake path is broken.
- Rollout: one whole-plan draft PR under the fast lane, with logical M1 → M2
  → M3 commits and execution-log rows. The work-board protocol and the
  delegated whole-plan assignment supersede the older three-PR wording. No setting, no schema, no
  recipe-identity change (the recipe is captured before any of this and is
  not read by the scheduler). Rollback is a revert of M3; M1/M2 are pure
  functions whose new behaviour only reaches a process through M3.
- The interaction with
  [FONT-ATTESTATION-AND-BLOCKING-IO.md](FONT-ATTESTATION-AND-BLOCKING-IO.md):
  fewer spawns means fewer `recipe_engine_is_current` walks per spawn; the
  per-segment walk in `materialize` is that plan's, not this one's.

## 7. Open questions

1. `SESSION_IDLE_TTL = 300 s` now bounds how long a paused viewer holds a
   stopped encoder's memory and hardware session. Is five minutes right for
   an encoded rendition on a node with one QSV session, or should encoded
   renditions idle out sooner (say 120 s)? M4's `ps` samples give the RSS
   figure to decide with.
2. `AHEAD_RESUME_SECONDS = 90` is half the horizon. A larger value means
   shorter stops and more bursts; a smaller one means the producer runs
   closer to the reader before stopping. The review asked for "≥90 s
   bursts"; nothing measured says 90 is better than 60 or 120. M4's count
   is the input.
3. `Admissions` has no FIFO: after a yield, the yielded producer and the
   waiter compete on the mutex when both ask. §3.2 makes the yielded
   producer not ask until its reader needs it, which is fairness by
   construction for the two-viewer case. Three or more encoded viewers on a
   one-session node will still see mutex-order admission. Is that
   acceptable, or does the pool need arrival-ordered admission (a larger
   change in `admission.rs`, out of this plan)?
4. `STOPPED_ENCODER_POLL = 1 s` against `QUEUE_WAIT = 5 s`: a rolling live
   start that queues while an encoder is stopped waits up to 1 s for the
   yield, then the kill-and-reap time, out of its 5 s budget. That is
   enough on paper; the M3 PR must list every `wait_for_slot` caller's
   deadline (`transcode.rs:19473`, `:20118`, `vodencode.rs:128`) and, if
   any is under 2 s, the poll drops to 250 ms (`ADMISSION_POLL`).

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M0 | [#412](http://192.168.4.7:3000/noirr/plurx/pulls/412) | needs: run §5.0's controlled 30-minute transcode and subtitle-burn plays. Read-only SSH observation on the media host at 2026-09-21 05:53 UTC found deployed build `v0.3.0-3052-g882862e88`, a container started at 04:18:48 UTC, zero `spawned a producer generation` records since that start, zero retained journal matches over seven days, and no pre-M3 generation metric. No playback was initiated and no before-count was inferred from the empty interval. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M1 | [#412](http://192.168.4.7:3000/noirr/plurx/pulls/412) | done: added the rendition-owned ahead-hold input and 90-second low-water decision, separate from working-set hysteresis; all 33 focused `prodsched` tests pass on pinned Rust 1.97.1. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M2 | [#412](http://192.168.4.7:3000/noirr/plurx/pulls/412) | done: added the pure contention table and explicit `YieldToWaiter` termination; all 29 focused `prodexec` tests pass on pinned Rust 1.97.1, including no-yield, no-double-operation, fairness and fixed-point cases. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M3 | [#412](http://192.168.4.7:3000/noirr/plurx/pulls/412) | done: wired the rendition latch, VOD registration wake, rolling-live poll, success-only transition updates, and closed-label generation/termination counters. Production callers that register a live wait use the five-second `QUEUE_WAIT`; VOD admission retries at 250 ms and its one-second policy-read bound registers no capacity waiter, so the conservative one-second stopped-encoder poll remains below every live admission deadline. Sole adversarial review [#3230](http://192.168.4.7:3000/noirr/plurx/pulls/412#issuecomment-3230) was disposed by executable real-`spawn_driver` coverage: the no-kick rolling waiter yields only after `STOPPED_ENCODER_POLL`, the yielded rendition cannot reach admission above low-water but does at the boundary, and session-TTL maintenance detaches the last reader, reaps the stopped child, and permits a full bundle reacquisition. All three tasks are explicitly settled. Focused scheduler/executor, named M3 and restart-continuity checks pass on pinned Rust 1.97.1. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M4 | [#412](http://192.168.4.7:3000/noirr/plurx/pulls/412) | needs: after merge/deploy, run §5.4's two controlled 30-minute plays, 60-second `ps` sampling, generation/termination metric capture, idle-age check and live-timeout journal check. No deployment or playback was performed from this source-only author session. |
