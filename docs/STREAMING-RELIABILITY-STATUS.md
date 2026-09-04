# Streaming reliability — review, repair, and promotion status

**Status:** corrective implementation active · **Effort:** `effort/streaming-reliability` ·
**Started:** 2026-09-04 · **Baseline:** `origin/main` at `48615baf`

Companion to [PLAYBACK.md](PLAYBACK.md) (the shipped playback contract),
[PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md) (the control-protocol
chronicle), and [VOD-CUTOVER.md](VOD-CUTOVER.md) (the immutable-HLS cutover) —
this page answers *what the current reliability effort is doing, what has been
proved, and what remains before it may reach `main`*.

The detailed, ranked result is
[STREAMING-RELIABILITY-REVIEW.md](STREAMING-RELIABILITY-REVIEW.md). The review
and evidence tasks were merged before the Forgejo cutover; all current task
branches and reviews use `noirr/plurx` on Forgejo.

The working copy is an isolated clone under `/private/tmp`; the existing
developer checkouts are not used for implementation or validation. Task
branches merge into the effort branch through the compile-only development
gate. The effort reaches `main` only after one frozen-candidate full
qualification run, an exact-head adversarial approval, and green unit tests.

## Outcome — playback changes must preserve the player

The effort has four product outcomes:

- ordinary playback does not freeze or stutter because a producer, playlist,
  session owner, or client recovery loop disagreed about progress;
- a seek stays on the same immutable film timeline and does not recreate the
  stream;
- a quality, resolution, track, dynamic-range, or placement change uses one
  make-before-break transaction when the destination is compatible;
- an unsupported transition fails explicitly and leaves the working stream
  intact.

The review may change any component or contract needed to reach those
outcomes. It will not preserve the retained live-HLS engine as an unannounced
production fallback, hide an unfinished capability behind a compile-time gate,
or call a green unit suite physical playback evidence.

## Workstreams — the review decides the build order

| Workstream | State | Current evidence | Exit condition |
|---|---|---|---|
| Last-seven-days change review | **Complete** | 1,193 commits · 217 first-parent changes · 512 files · 220,101 insertions · 50,655 deletions since 2026-08-28; method and disposition are in the findings document | Exact-head review verifies scope and evidence |
| End-to-end architecture review | **Complete — adversarially approved** | Independent server, client, and operations passes converge on the missing prepare/commit transaction, incomplete VOD recipe coverage, and unbounded client freezes; exact-head re-review approved every correction at `b3179001` | Keep the approved contract authoritative while implementation evidence accumulates |
| Review and status PR | **Merged — [#903](https://github.com/pjunod/plurx/pull/903)** | Exact-head adversarial approval at `2fd62daf`; Effort development gate green; merged as `be088173` | Complete |
| Corrective task PRs | **Active — playback evidence merged; client recovery corrections ready to commit** | Evidence head `73d9bab7` passed exact-head adversarial review and merged as `7db47b35`. The first client-recovery review rejected nine correctness gaps. The local correction now bounds startup and established freezes, publishes explicit intent before mutation, fences stale recovery, distinguishes Auto from Original, coalesces seeks, retains request sequence across replacement, and retires a seek only after a destination frame or advancing audio clock. Android JVM units, the web control contract, pinned-Rust all-target compilation and focused units, 359 Apple simulator tests, and iOS/tvOS compilation are green | Commit the corrected head, pass exact-head adversarial review and the task's one post-fix full-suite run, then merge through Forgejo into the effort |
| Final qualification | **Not started** | The candidate is not frozen | Current `main` merged into the effort; unit suite and full promotion gate green once on the final fixed tree; qualification receipt inspected |
| Promotion and cleanup | **Not started** | No main-bound PR exists | Effort PR merged; task and effort branches removed only after the merge is proven |

## Known facts — claims already verified against the baseline

1. **The documented VOD-only cutover is not the production default.** Omitted
   presentation means VOD and an explicit live request is refused, but the
   default Cargo feature retains growing live HLS and a missing runtime setting
   enables fallback. VOD refuses transcode and subtitle burn, so those common
   classes silently enter the old mutable engine. Tests invert the runtime
   default and therefore miss the shipped configuration.
2. **The control plane stages but does not yet complete transparent change.**
   The 2026-09-04 M6 slice can write a prepared successor from an accepted
   in-session selection change. Its own handoff records the commit trigger as
   the next slice, and the current clients still perform several changes as a
   replacement open.
3. **A passing source test is not the playback claim.** The VOD cutover records
   automated Chrome coverage but still carries named-device and two-hour 4K
   physical evidence debt. This effort will preserve that distinction.
4. **The required compiler is available.** Rust 1.97.1 is installed and is
   invoked explicitly because Homebrew Rust 1.95.0 precedes it on `PATH`.
5. **The strongest freeze gates are not running.** Pull-request CI explicitly
   omits VOD steady, seek-storm, and stall-recovery cases. All eight inspected
   nightly runs from 2026-08-28 through 2026-09-04 failed before playback
   because the installed Playwright Chromium executable was not passed to the
   playback lab.
6. **The fleet reproduces an unbounded-hold class.** Seven-day node-local
   telemetry includes repeated Apple `server_hold` outcomes at one frozen
   position for many minutes. The current Apple and web paths allow an advisory
   producer hold to defer a decoder recovery without spending a finite budget.

## Decisions made without waiting

1. **Use the existing VOD and control foundations.** Their core direction is
   sound: immutable film-time resources remove entire freeze categories, and
   prepare/commit/abort is the right shape for compatible stream changes. The
   effort will repair or replace faulty details instead of introducing a third
   playback architecture.
2. **Expose unsafe enablement in Developer settings.** A capability that needs
   prerequisites will name them beside its control. No new compile-time or
   hidden configuration feature gate will be added.
3. **Prefer explicit refusal over destructive fallback.** If a destination
   cannot be prepared, the current stream remains authoritative. Recreating a
   session or silently changing presentation is not a transparent handoff.
4. **Use focused tests during development and qualify once.** Each task PR gets
   the smallest regression that proves its behavior plus the effort gate. The
   final fixed candidate gets the unit suite and one complete main-promotion
   qualification, in that order.
5. **Use Forgejo as the write authority.** GitHub is retained as a read-only
   historical remote. Current branches, task reviews, merges, and build status
   are published through `noirr/plurx` on Forgejo; final reporting records the
   exact branch and head so no GitHub status is mistaken for current evidence.

## Update log — newest entry first

| At (America/New_York) | State change |
|---|---|
| 2026-09-04 | Moved current work to Forgejo as the sole write authority. The first exact-head adversarial client review rejected nine gaps: Android could still freeze before establishment; deadlines counted control latency incorrectly; quality state went stale; Apple did not publish the exact seek target before mutation; request sequence could be lost; seek completion was not presentation; stale actions crossed awaits; terminal verdict lifetime was too broad; and Original collapsed into Auto. All nine are corrected locally. Android units, the web control contract, Rust 1.97.1 all-target compilation and focused units, 359 Apple simulator tests, and generic iOS/tvOS compilation are green. The first Apple test run exposed one stale fixture that asserted an implicit seek target; the fixture now supplies the explicit target and the entire rerun passes. |
| 2026-09-04 | Opened client-recovery PR #911 against the effort after the tracked fast lane passed. Its exact head is being sent for adversarial review; no merge occurs until review findings are addressed and the Effort development gate is green on that same head. |
| 2026-09-04 | Client recovery is implemented locally without a feature flag. Android now detects a still-open established buffering freeze at 8 seconds, gives control at most one deferral inside a 20-second absolute deadline, and turns exhausted recovery into a visible failure. Apple and web use the same finite-authority rule. Owner changes settle old asks immediately; web terminal diagnoses now survive same-title replacement only for their lease; Android carries stable playback identity, exact quality, and explicit seek intent across controller replacement. Focused proof: web control passed, Android JVM units passed, and Apple ran 274 selected tests with zero failures. |
| 2026-09-04 | Playback evidence PR #908 passed exact-head adversarial review at `73d9bab7`, passed the Effort development gate, and merged into the effort as `7db47b35`. PR #905 was superseded and auto-closed when its old head branch was removed. |
| 2026-09-04 | PR #905 third-pass review also found that one 200 ms Auto hitch could pass the unimplemented p95 target, and that Auto never checked its first target frame against the film-position boundary. The singleton nightly transition now must stay within 100 ms, the 250 ms absolute ceiling remains explicit, and target-tagged frame evidence rejects both forward and backward jumps over 250 ms. |
| 2026-09-04 | PR #905 third-pass review found API presence could be mistaken for an operative frame oracle when Chromium never invoked its callback. Auto now requires a finite post-cliff callback before suppressing sampled-clock evidence; zero-callback and stopped-callback regressions both fail closed. |
| 2026-09-04 | PR #905 second-pass adversarial review rejected four P1 evidence races and one P2 seam despite a green gate: outgoing frames could satisfy target readiness, hold-window gaps could be reset, Auto moves between polls could collapse, non-finite Auto runway could pass, and final pre-replacement faults could escape sampled rollup. The local correction tags frames, closes each transition after its hold, uses a monotonic timestamped switch ledger, fails runway closed, and increments lifetime faults at their source. |
| 2026-09-04 | Addressed every first-pass PR #905 finding: the Auto cliff is a strict nightly point; rendition completion needs the expected method, exact height, and a new frame; the boundary snapshot and page-lifetime frame/counter probes cover seams and silent freezes; 60-second post-downshift stability is mandatory; identity, runway, position, per-transition p95/max gaps, and partial-browser scope are explicit. |
| 2026-09-04 | Opened corrective PR #905. Its first adversarial pass rejected unscheduled Auto evidence, stale-rendition acceptance, silent-freeze and first-sample seams, bad hold-time math, incomplete identity/dwell checks, and overclaimed browser SLO coverage. All findings are being implemented; audio remains explicitly device evidence. |
| 2026-09-04 | PR #903 merged into the effort as `be088173`. Started the first corrective task in the isolated clone: repair the playback evidence path before changing the behavior it judges. |
| 2026-09-04 | PR #903 exact-head adversarial re-review approved the architecture and findings at `b3179001` with no remaining merge-blocking factual or design errors. The required Effort development gate passed; this status-only accuracy update is the final pre-merge change. |
| 2026-09-04 | PR #903 re-review found three remaining acceptance/evidence holes: supported transitions could all refuse, Auto could still visibly stutter, and retained fleet/receipt claims lacked executable detail. The contract now requires 20/20 commits for qualified tuples, applies continuity SLOs to Auto, and retains the commands and named receipts. |
| 2026-09-04 | PR #903 exact-head adversarial review found eight documentation/architecture defects: adaptation authority, seek intent, owner-loss acceptance, gated-feature inventory, numerical SLOs, retained evidence, completion scope, and inconsistent status. All eight were corrected for re-review. |
| 2026-09-04 | Opened draft review PR [#903](https://github.com/pjunod/plurx/pull/903); completed independent client, server, and operations passes; recorded four P0 blockers and the target immutable-rendition/transaction architecture. |
| 2026-09-04 | Created isolated clone and `effort/streaming-reliability`; confirmed authenticated GitHub access and the pinned Rust 1.97.1 toolchain; began independent server, client, and operations/architecture reviews. |
