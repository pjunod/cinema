# Content analysis repair — implementation status

**Status:** implementation compiled; review and qualification pending · **Updated:** 2026-09-17 UTC ·
**Base:** `9e1072e6f3c05201b5ff94058c68bbfff7b5894a` · **Effort:**
`effort/content-analysis-repair`

Companion to the
[implementation contract](CONTENT-ANALYSIS-FAILURES-IMPLEMENTATION.md) — this
is the live ledger for source, compiler, review, qualification, and promotion
evidence. A checked row means the named evidence exists on the exact recorded
tree. Fleet recovery and deployment remain separate actions.

## Current state

| Package | State | Evidence | Next action |
|---|---|---|---|
| W0 · reproduce and reconcile | partial | isolated clone at current `main`; Rust 1.97.1 confirmed; queue, migration, probe, settings, and recovery seams reconciled | record the bounded live compatibility inventory before enablement |
| W1 · shared types and persistence | implemented | typed bounded diagnostics, shared retry policy, SQLite/Hiqlite/local schema updates, and repair receipts compile | adversarial review and focused regressions |
| W2 · selected-video completion | implemented | held-source probe resolves the exact mapped video stream; rational lower-bound and timeline-span checks compile | add/qualify the integration fixture in the final fast lane |
| W3 · durable bounded retry | implemented | fixed backoff, seven-day deadline, attempt preservation, deadline-aware leases, and both Store paths compile | exercise both backends in the final fast lane |
| W4 · identity-correct repair | implemented | exact pipeline/video identity, revisioned preview/apply candidates, atomic receipts, and stale revalidation compile | adversarial review and backend regressions |
| W5 · operator surfaces | implemented | history diagnostics, truthful failure text, repair controls, and advisory Developer enablement compile | browser contract regression in the final fast lane |
| W6 · review and promotion | pending | tests deliberately deferred; implementation commit `c448d216` compiles | finish test fixtures, open the main-bound PR, review, repair, fast lane, merge |

## Standing decisions

1. **One effort branch and one main-bound pull request.** Logical commits stay
   reviewable while the expensive qualification runs once on the integrated
   candidate.
2. **Compilation runs throughout; tests wait for the final reviewed tree.**
   This follows the requested resource policy while preserving the
   repository's compile-before-CI rule.
3. **Enablement belongs in Developer settings and remains authoritative.**
   The UI will explain the required safe conditions and whether each is met.
   Missing evidence is advisory and never blocks the user's enable choice.
4. **No fleet mutation is implied by implementation.** Preview is read-only.
   Applying a repair generation, deploying, or changing the live analysis
   setting needs separate authorization or an already-authorized release
   workflow.
5. **Existing successful artifacts remain valid.** This effort changes the
   completion predicate and recovery policy without changing index bytes,
   recipe fingerprints, or positive catalog entries.

## Evidence ledger

| Evidence | Result |
|---|---|
| Isolated working copy | `/private/tmp/plurx-agent-content-analysis`; the user's checkout was read-only |
| Current intended base | `9e1072e6f3c05201b5ff94058c68bbfff7b5894a` |
| Handoff's verified base | `363a22e28aa53094d899a9ad3c812241a6243548`; all seams are being rechecked |
| Pinned compiler | `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| Baseline compile | green: `cargo check -p plurxd --all-targets --locked` |
| Implementation compile | green on `c448d216`: `cargo fmt --all`; `git diff --check`; `cargo check -p plurxd --all-targets --locked` |
| Focused regressions | deferred until the single post-review fast-lane phase |
| Adversarial implementation review | not run; required only after the PR candidate is complete |
| Main fast lane | not run; starts only after review findings are addressed and the draft PR is marked ready |
| Fleet recovery | not authorized or run |

## Completion

- [ ] W0 bounded compatibility inventory recorded before enablement.
- [x] W1 diagnostics, retry policy, and migrations implemented and compiled.
- [x] W2 selected-video completion implemented and compiled.
- [x] W3 retry lifecycle implemented in SQLite and Hiqlite and compiled.
- [x] W4 exact-identity preview/apply repair implemented and compiled.
- [x] W5 API, browse, analysis, Developer settings, and operations docs implemented.
- [ ] One adversarial review addressed.
- [ ] Fast lane green on the reviewed exact tree.
- [ ] Main-bound pull request merged.
