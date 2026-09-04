# Streaming reliability — review, repair, and promotion status

**Status:** review in progress · **Effort:** `effort/streaming-reliability` ·
**Started:** 2026-09-04 · **Baseline:** `origin/main` at `48615baf`

Companion to [PLAYBACK.md](PLAYBACK.md) (the shipped playback contract),
[PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md) (the control-protocol
chronicle), and [VOD-CUTOVER.md](VOD-CUTOVER.md) (the immutable-HLS cutover) —
this page answers *what the current reliability effort is doing, what has been
proved, and what remains before it may reach `main`*.

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
outcomes. It will not restore the removed live-HLS presentation, hide an
unfinished capability behind a compile-time gate, or call a green unit suite
physical playback evidence.

## Workstreams — the review decides the build order

| Workstream | State | Current evidence | Exit condition |
|---|---|---|---|
| Last-seven-days change review | **Running** | 1,193 commits · 217 first-parent changes · 512 files · 220,101 insertions · 50,655 deletions since 2026-08-28 | Every playback-affecting subsystem and cross-cutting cluster/storage change is dispositioned in the findings document |
| End-to-end architecture review | **Running** | Server, web, Apple, Android, operations, and test passes assigned independently | One current-state diagram, failure model, ranked findings, and target architecture agree with the code rather than prior status prose |
| Review and status PR | **Building** | This page is the first artifact | Adversarial exact-head approval and focused documentation contracts green |
| Corrective task PRs | **Waiting on review** | No implementation claim yet | Every accepted finding has code, a regression, or an explicit evidence-only disposition |
| Final qualification | **Not started** | The candidate is not frozen | Current `main` merged into the effort; unit suite and full promotion gate green once on the final fixed tree; qualification receipt inspected |
| Promotion and cleanup | **Not started** | No main-bound PR exists | Effort PR merged; task and effort branches removed only after the merge is proven |

## Known facts — claims already verified against the baseline

1. **Public HLS creation is VOD-only.** Omitted presentation means VOD and an
   explicit live request is refused; the removed growing-live engine is not a
   recovery option. That architectural correction is real, but transcode and
   subtitle-burn VOD classes still return typed unavailable responses.
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
| 2026-09-04 | Created isolated clone and `effort/streaming-reliability`; confirmed authenticated GitHub access and the pinned Rust 1.97.1 toolchain; began independent server, client, and operations/architecture reviews. |

