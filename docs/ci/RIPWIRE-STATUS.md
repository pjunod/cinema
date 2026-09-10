# Ripwire status — implementation and promotion progress

**Status:** blocked on agent capacity; implementation preserved · **Updated:** 2026-09-10 · **Owner:** validation.framework

Companion to [the harness plan](AI-HARNESS-IMPLEMENTATION-PLAN.md).
This page tracks the bounded navigation adapter requested by Paul.

## Checkout and decisions

Own clone: `/private/tmp/plurx-ripwire`. Integration: `effort/ripwire`.
[Adapter task PR #244](http://192.168.4.7:3000/noirr/plurx/pulls/244) merged
into the effort; main promotion has not started.
Initial main: `4519f87aa17def278dc4ad6c08a411e7829c2ec4`.
The user's working tree is untouched. No hooks installed.

The current request and supplied contributor policy override the clone's
stale effort-gate and pre-push-test instructions. Final promotion requires
one adversarial review, findings addressed, then the current-head fast lane.
Full suites belong to the separate sweep.

After Paul asked to keep going, proceeding with the scoped CLI-only brief
and its targeted fixture/pilot checks. Product Settings integration would
require a separate runtime feature. Full suites remain deferred. These
scope decisions are recorded for Paul to revisit.

## Milestones

| Milestone | State | Evidence / next action |
|---|---|---|
| M0: base and release | complete on macOS ARM | Archive SHA-256 matches; exact member and version 0.5.0 verified; help confirms CLI flags and lean/rich cache families. |
| M1: installer and bounded runner | complete | Explicit setup passed; 18 focused fake-tool tests passed (15.728 s). |
| M2: queries and coverage | complete | All verbs executed in real fixtures; both cache families checked after edit, rename, deletion, branch change, and corruption. Kotlin/embedded-JS selectors are refused; Swift callback and Rust qualified cross-module edges are omitted. |
| M3: measured pilot | blocked | Frozen `da51ffb7`; 24 timing/RSS samples complete. Warm medians: find 494 ms, callers 325 ms, coverage 318 ms. Three navigation sessions returned; next three failed on account limit; ten remain. Retained correction tests are sensitive, but blind replay has not run. See [pilot](RIPWIRE-PILOT.md). |
| M4: promotion | blocked | No adversarial review requested and no main PR/fast-lane activation. Finish trial, integrate current main, then exactly one review, fixes, fast lane, merge. |

## Scope and remaining limitations

Product code and workspace Rust sources are unchanged.
Only macOS ARM release execution has been inspected; other platform hashes
are supplied pins pending setup on those platforms. The pilot records
measured results and omissions; default adoption is
deferred. No reset credits were available when the account limit blocked
the remaining agent sessions. Full suites have not run. The source-only
fixture Rust is parser input, not a workspace/product Rust change.
