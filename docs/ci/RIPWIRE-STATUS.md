# Ripwire status — implementation and promotion progress

**Status:** implementation and trial complete; main promotion pending · **Updated:** 2026-09-10 · **Owner:** validation.framework

Companion to [the harness plan](AI-HARNESS-IMPLEMENTATION-PLAN.md).
This page tracks the bounded navigation adapter requested by Paul.

## Checkout and decisions

Own clone: `/private/tmp/plurx-ripwire`. Integration: `effort/ripwire`.
[Adapter task PR #244](http://192.168.4.7:3000/noirr/plurx/pulls/244) merged
into the effort. [Pilot task PR #247](http://192.168.4.7:3000/noirr/plurx/pulls/247)
is a draft into that effort; main promotion has not started.
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
| M1: installer and bounded runner | complete | Explicit setup passed; 19 focused fake-tool tests passed (16.442 s). |
| M2: queries and coverage | complete | All verbs executed in real fixtures; both cache families checked after edit, rename, deletion, branch change, and corruption. Kotlin/embedded-JS selectors are refused; Swift callback and Rust qualified cross-module edges are omitted. |
| M3: measured pilot | complete with limits | Frozen `da51ffb7`; 24 timing/RSS samples complete. Warm medians: find 494 ms, callers 325 ms, coverage 318 ms. All sixteen navigation sessions returned across all four tasks; interrupted timing is excluded from adoption claims. Both blind patches scored 3/4 against retained tests (same macOS symlink-spelling mismatch); diagnostic canonical-temp runs pass 4/4. Verdict: opt-in only; no default prompt additions. See [pilot](RIPWIRE-PILOT.md). |
| M4: promotion | pending | No adversarial review requested and no main PR/fast-lane activation. Integrate current main, then exactly one review, fixes, fast lane, merge. |

## Scope and remaining limitations

Product code and workspace Rust sources are unchanged.
Only macOS ARM release execution has been inspected; other platform hashes
are supplied pins pending setup on those platforms. The pilot records
measured results and omissions; default adoption is
not justified by the inconsistent trial metrics. Earlier account-limit interruptions were recovered; their timing remains
invalid for speed comparisons. Main advanced to `d9c15581` during the trial
and will be integrated before final review. Full suites have not run. The source-only
fixture Rust is parser input, not a workspace/product Rust change.

Disposable trial clones were removed after capturing completed trial patches and findings.
Duplicate release downloads remain removed.
The own implementation clone, ignored evidence, and reconstruction helpers
remain available. Current checks: 19 adapter tests and 4 documentation
contracts pass; catalog lint passes. No full suites or fast lane ran.
