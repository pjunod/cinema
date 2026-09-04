# Streaming reliability — review, repair, and promotion status

**Status:** review approved; merge pending · **Effort:** `effort/streaming-reliability` ·
**Started:** 2026-09-04 · **Baseline:** `origin/main` at `48615baf`

Companion to [PLAYBACK.md](PLAYBACK.md) (the shipped playback contract),
[PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md) (the control-protocol
chronicle), and [VOD-CUTOVER.md](VOD-CUTOVER.md) (the immutable-HLS cutover) —
this page answers *what the current reliability effort is doing, what has been
proved, and what remains before it may reach `main`*.

The detailed, ranked result is
[STREAMING-RELIABILITY-REVIEW.md](STREAMING-RELIABILITY-REVIEW.md). Review task
PR [#903](https://github.com/pjunod/plurx/pull/903) targets the effort branch.

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
| Review and status PR | **Approved; merge pending — [#903](https://github.com/pjunod/plurx/pull/903)** | Ranked findings, current/target architecture, repair order, SLOs, race matrix, saved questions, exact-head adversarial approval, and the focused documentation gate are complete | Merge the current status-only head after its fresh gate and adversarial check |
| Corrective task PRs | **Waiting on review** | No implementation claim yet | Every accepted finding has code, a regression, or an explicit evidence-only disposition |
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

## Update log — newest entry first

| At (America/New_York) | State change |
|---|---|
| 2026-09-04 | PR #903 exact-head adversarial re-review approved the architecture and findings at `b3179001` with no remaining merge-blocking factual or design errors. The required Effort development gate passed; this status-only accuracy update is the final pre-merge change. |
| 2026-09-04 | PR #903 re-review found three remaining acceptance/evidence holes: supported transitions could all refuse, Auto could still visibly stutter, and retained fleet/receipt claims lacked executable detail. The contract now requires 20/20 commits for qualified tuples, applies continuity SLOs to Auto, and retains the commands and named receipts. |
| 2026-09-04 | PR #903 exact-head adversarial review found eight documentation/architecture defects: adaptation authority, seek intent, owner-loss acceptance, gated-feature inventory, numerical SLOs, retained evidence, completion scope, and inconsistent status. All eight were corrected for re-review. |
| 2026-09-04 | Opened draft review PR [#903](https://github.com/pjunod/plurx/pull/903); completed independent client, server, and operations passes; recorded four P0 blockers and the target immutable-rendition/transaction architecture. |
| 2026-09-04 | Created isolated clone and `effort/streaming-reliability`; confirmed authenticated GitHub access and the pinned Rust 1.97.1 toolchain; began independent server, client, and operations/architecture reviews. |
