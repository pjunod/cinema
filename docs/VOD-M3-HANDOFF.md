# VOD M3 — serving from the plan, and the first code on the live path

**Status:** BUILT 2026-08-25 on `agent/vod-m3` (copy-path serving; §10 below
records what was deliberately deferred and why) ·
**Executes:** M3 of [VOD-PRESENTATION-PLAN.md](VOD-PRESENTATION-PLAN.md) §8,
with the rulings in [VOD-M2-QUESTIONS.md](VOD-M2-QUESTIONS.md) ·
**Written:** 2026-08-24 · **Branch:** `agent/vod-m3`

Companion to
[VOD-PRESENTATION-IMPLEMENTATION-HANDOFF.md](VOD-PRESENTATION-IMPLEMENTATION-HANDOFF.md)
(the whole programme) — this is *the milestone where it starts serving*.

## 1. Orientation — read this, work like this

Read, in order: plan §2.1–2.5 and §8's M3 row; this file; then
[VOD-M2-QUESTIONS.md](VOD-M2-QUESTIONS.md) §2, §4.1 and §4.4, because three of
M3's tasks are the second halves of rulings whose first halves are already
built.

**M3 is different from M0–M2 in one way that governs everything else: it is the
first milestone whose code runs on the live playback path.** M0 measured, M1 and
M2 landed dark — every module they added is `#![allow(dead_code)]` with no
caller outside its own tests. Nothing shipped so far can break a viewer, because
nothing shipped so far is reachable from a request.

That stops being true the moment `presentation: "vod"` is honored. So:

- **Every M3 change is behind `playback.vod_presentation` AND the per-request
  `presentation` opt-in.** A file with no plan, or a client that did not ask,
  keeps today's presentation byte for byte. There is no "mostly VOD" state.
- **Write the test before the implementation** for anything involving
  cancellation, coalescing, or a deadline. The plan says this for the wait pool
  specifically; it applies to the whole milestone.
- **If a step seems to require changing a decision in the ledger, stop and
  flag it.** Two milestones running, the count of rulings that came back
  "your premise was wrong" is two out of ten — the ledger is load-bearing and
  arguing with it in a commit message is not the way.

## 2. What M2 handed you

All of this is on `main` after `agent/vod-m2d` merges, all tested, none of it
called from a request path yet. M3's job is largely to *call* it.

| Module | What it gives you | What M3 must do with it |
|---|---|---|
| `plurx_core::segplan` | the plan, the index, the landing matcher, `SegmentPlan::playlist()` | render once at attach, store the bytes, serve them unchanged for the rendition's life |
| `plurxd::titlestore` | planned · materialized · admitted, budgets, eviction candidates | one `Manifest` per live rendition |
| `plurxd::prodsched` | `decide(manifest, demands, position) -> Action` | feed it real demand from blocked GETs |
| `plurxd::prodexec` | `next_step(producer, action) -> Step`, `after(...)` | §3 below — turn a `Step` into a signal |
| `plurxd::renditiondir` | materialize/evict/reconcile/make_durable, `InitIdentity` | §4, §5, §6 below |

Two things are deliberately absent and are M3's to invent: **the `Rendition`
itself** (the object owning a manifest, a directory, a producer and a reader
set) and **the wait pool** behind blocking GETs.

## 3. The attachment — `prodexec::Step` against a real child

This is the first task and the riskiest, so it comes with the most rules.

**What exists.** `prodexec::next_step` answers exactly one `Step` for a
`(Producer, Action)` pair, and `prodexec::after` says what to believe once that
step succeeds. Both are pure and exhaustively tested; the table itself is not in
question.

**What M3 writes** is the thin layer that performs a `Step` against a
`tokio::process::Child`, and it is thin on purpose. Every rule below exists
because `transcode.rs` already learned it:

- **`Step::Stop` / `Step::Resume` are `libc::kill` with `SIGSTOP` / `SIGCONT`,
  taken under the child lock**, exactly as `apply_ahead_window` does at
  `transcode.rs:10930`. Read that function before writing this one — in
  particular the `progress.touch()` before the flag flips, which exists so a
  watchdog cannot observe "running" beside a motion clock that spans the
  suspension and fail a healthy session at the moment of its resume.
- **`Step::Terminate` and `Step::Restart` are `child.kill()`, which is
  `SIGKILL`.** Not a graceful signal: a stopped process does not run a `SIGTERM`
  handler until something continues it, so terminating a suspended producer
  politely is a wait that never ends.
- **Record belief only after the operation succeeds.** `after()` is a separate
  function for this reason. A producer recorded as stopped that is actually
  running produces past every horizon; one recorded as running that is actually
  stopped never resumes.
- **Never signal a pid you have not confirmed is still yours.** The idempotence
  in `next_step` is the first defence; the child lock is the second. There is no
  third.

**Acceptance:** a test that drives a real child process through
suspend → resume → reposition → reclaim and asserts the process actually stopped
(`/proc/<pid>/stat` state `T`) and actually resumed. Not a mock: the whole point
of this layer is the parts a mock cannot get wrong.

## 4. Init identity — the second half of §2's ruling

M2 built `InitIdentity` and the canonical `PromotionInputs`. M3 wires them.

**At rendition create:** read the index. If `parameter_sets_constant` is false,
**do not VOD-present this title** — fall back to today's presentation and say
why in one log line. This is the ruling's step 3 and it is the whole reason
mid-film variation is not a mid-playback failure.

**At first generation:** `InitIdentity::establish(muxer_init, index.promotion)`,
write the served init, store both digests on the rendition record.

**At every later generation:** `identity.served_init_for(&muxer_init)` before a
single segment is written. `Err(MuxerDrift)` is the typed `producer_failed` plan
§2.2 names — real pipeline drift under a rendition a client already holds a
playlist for. `Err(PromotionDrift)` should be unreachable; if it fires,
promotion stopped being a pure function of stored facts and that is worth a loud
error rather than a fallback.

**Never re-promote and rewrite a stored init.** Viewers and already-materialized
segments hold the old bytes. The ruling prohibits it; the type does not offer
it.

## 5. Durability and adoption — the second halves of §4.1 and §4.4

**At admission:** `RenditionDir::make_durable(&manifest)` **before**
`manifest.complete(&budgets)`. Admission is the durability boundary and the
only one — that ordering is the point, and reversing it publishes a promise
before the bytes behind it are real.

**At adoption (restart, resurrection):** `reconcile`, then act on what it says.

- `init_present: false` → **regenerate and verify, else purge.** Run a
  generation head, check its muxer init against the stored digest, apply the
  stored promotion, check the served digest. Match keeps every surviving
  segment; mismatch purges to planned-only and re-produces. This is only valid
  because §2 made the init reproducible independently of the segments.
- `forgotten` non-empty on an admitted rendition → it has already been demoted
  by `Manifest::forget`; do not re-admit without re-completing.

**Carried from §4.3:** before adoption lands on the restart path, replace
`reconcile`'s per-entry `metadata()` with a single `read_dir` pass. It is
correct today and dormant; a node restart over a large store would make it
4,000 × N blocking-pool round trips.

## 6. The working set — the second half of §3.3

`prodsched::WorkingSet::budget_bytes` reads 0 as *not configured*. When you
expose it through `/api/v1/settings`, **validation must reject a parsed zero**
from an operator who meant "no working set" and offer a small floor instead. The
two zeroes mean opposite things and only the caller knows which it got; the doc
comment is not enough on its own.

## 7. What M3 owes that M2 did not touch

Straight from plan §8's M3 row — this file adds nothing to it, and lists it so
the milestone is not read as only the five sections above:

- **Create path:** parse `presentation` / `block_budget_secs`; attach the
  session handle to a `Rendition`; answer `vod: true` and the full duration.
- **Segment GET:** the three-outcome contract with one hard deadline, typed
  `segment_pending` 503, per-session (4) and global wait caps, coalescing, and
  disconnect cancellation. **Verify axum/hyper actually cancels on body drop,
  and write that test first** — the plan says so, and a wait pool that leaks a
  future per abandoned request is a slow leak nobody sees until a seek storm.
- **Lifecycle:** dormant/terminal per the plan's state diagram; tombstones on
  DELETE, supersession, admin stop, revocation and file replacement; sliding TTL
  on authorized GETs; resurrection from the persisted recipe.

## 8. Non-goals — do not do these

- **Do not touch the transcode EXTINF question.** D6 is settled for the
  playlist: one renderer, both arms. If the device halves come back demanding
  D6-B, the work item is routing the transcode encode through
  `fmp4::Segmenter` — a production-side change — not a second playlist artifact.
- **Do not add a second promotion path.** Anything that reads parameter sets
  from a generation's own landing fragment reintroduces exactly the collision
  §2 dissolved.
- **Do not fsync per segment** to be safe. It was ruled against with a reason:
  a materializing producer runs at several times realtime and a power loss
  before admission re-materializes honestly.
- **Do not delete anything.** M8 is the deletion pass. Today's live-edge
  machinery must keep working for every non-VOD session throughout M3–M7.
- **Do not hook `delete_files`** for node-local cleanup. The per-node sweep
  exists because that write is replicated and these rows are not.

## 9. Definition of done

Plan §8's M3 acceptance, unchanged, plus:

- the attachment test drives a real child and observes real process states;
- a title with `parameter_sets_constant: false` demonstrably keeps the legacy
  presentation;
- a recorded curl transcript of a full life — create → playlist → blocking
  fetch → far seek → reap → resurrect → ENDLIST-complete — in
  `docs/PLAYBACK-TESTING.md`;
- `make lint && make fmt-check && make history-check && make validation-lint`
  and `cargo test --workspace`, the last of which has now twice caught what a
  targeted run did not. Run it before calling anything ready.

## 10. What was built, and what was deliberately not (2026-08-25)

Everything in §§2–7 is implemented and on the request path except the
following, each deferred on a recorded reason rather than dropped:

- **Transcode-rung serving.** Gated on the D6/P2 device measurement, which is
  still owed by an operator run (§8 said not to touch it; nothing did). A
  `presentation:"vod"` transcode request keeps the live presentation with a
  log line.
- **Native-subtitle VOD sessions.** The multivariant playlist reads the init
  through `exact_hls_context` and mirrors the video timeline per rendition;
  wiring that against a plan playlist is M4 work beside the web flag. Falls
  back, logged.
- **`/status` for VOD sessions** answers 404. `SessionInfo` is live-session
  shaped (encoder speed, ahead window); the client that learns to send the
  flag in M4 is the right moment to decide what a VOD status even says.
- **`playback.vod_materialize_budget` (ruling A3's 30 s producer bound).**
  The block deadline is enforced per request; the producer-side watchdog is
  not yet. A wedged generation today answers `segment_pending` at each
  deadline rather than being killed at 30 s.
- **Durable 410s.** Tombstones are process-local. A terminal session's
  durable route is released, so after a daemon restart it answers 404, not
  410 — and still never resurrects, because resurrection requires an active
  route. The client-visible invariant (a terminal session never comes back)
  holds; only the status code after a restart differs from B6's letter.
- **Wait caps as settings.** Global 64 / per-session 4 are consts.

The §3 attachment, §4 init identity, §5 durability + adoption (including the
one-`read_dir` reconcile), §6 zero refusal, and §7's create/segment/lifecycle
contract are all in, with the four wait-pool cases and the process-state
attachment test among the module suites.
