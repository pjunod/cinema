# Ripwire status — implementation and promotion progress

**Status:** building · **Updated:** 2026-09-10 · **Owner:** validation.framework

Companion to [the harness plan](AI-HARNESS-IMPLEMENTATION-PLAN.md).
This page tracks the bounded navigation adapter requested by Paul.

## Checkout and decisions

Own clone: `/private/tmp/plurx-ripwire`. Integration: `effort/ripwire`.
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
| M1: installer and bounded runner | complete | Explicit setup passed; 14 focused fake-tool tests passed (15.7 s). |
| M2: queries and coverage | verifying | All verbs executed in real fixtures; both cache families checked after edit, rename, deletion, branch change, and corruption. Kotlin/embedded-JS selectors are refused; Swift callback and Rust qualified cross-module edges are omitted. |
| M3: measured pilot | pending | Fixed source, paired navigation, historical correction, coverage matrix. |
| M4: promotion | pending | Freeze effort, integrate main, one review, fast lane, merge. |

## Scope and remaining limitations

No product or Rust edits planned while scope is being reconciled.
Only macOS ARM release execution has been inspected; other platform hashes
are supplied pins pending setup on those platforms. No navigation result,
benchmark improvement, or default adoption is claimed yet.
