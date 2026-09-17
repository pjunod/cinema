# Content analysis repair — implementation status

**Status:** W0 base reconciliation active · **Updated:** 2026-09-17 UTC ·
**Base:** `9e1072e6f3c05201b5ff94058c68bbfff7b5894a` · **Effort:**
`effort/content-analysis-repair`

Companion to the
[implementation contract](CONTENT-ANALYSIS-FAILURES-IMPLEMENTATION.md) — this
is the live ledger for source, compiler, review, qualification, and promotion
evidence. A checked row means the named evidence exists on the exact recorded
tree. Fleet recovery and deployment remain separate actions.

## Current state — reconcile before changing behavior

| Package | State | Evidence | Next action |
|---|---|---|---|
| W0 · reproduce and reconcile | active | isolated clone at current `main`; Rust 1.97.1 confirmed; baseline `cargo check -p plurxd --all-targets --locked` green | map current queue, migrations, probes, settings, and recovery seams |
| W1 · shared types and persistence | pending | no implementation claimed | add bounded typed diagnostics, retry policy, migrations, and repair receipts |
| W2 · selected-video completion | pending | no implementation claimed | bind one fresh selected-stream expectation to every production index pass |
| W3 · durable bounded retry | pending | no implementation claimed | apply the fixed deadline and charged-attempt policy in both Stores |
| W4 · identity-correct repair | pending | no implementation claimed | preserve exact video identity and add preview/apply receipts |
| W5 · operator surfaces | pending | no implementation claimed | expose causes and advisory enablement in Developer settings and analysis views |
| W6 · review and promotion | pending | tests deliberately deferred | open one draft main-bound PR, obtain one adversarial review, repair findings, then run the fast lane once |

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
| Focused regressions | deferred until the single post-review fast-lane phase |
| Adversarial implementation review | not run; required only after the PR candidate is complete |
| Main fast lane | not run; starts only after review findings are addressed and the draft PR is marked ready |
| Fleet recovery | not authorized or run |

## Completion

- [ ] W0 reproductions and compatibility inventory recorded.
- [ ] W1 diagnostics, retry policy, and migrations complete.
- [ ] W2 selected-video completion complete.
- [ ] W3 retry lifecycle complete in SQLite and Hiqlite.
- [ ] W4 exact-identity preview/apply repair complete.
- [ ] W5 API, browse, analysis, Developer settings, and operations docs complete.
- [ ] One adversarial review addressed.
- [ ] Fast lane green on the reviewed exact tree.
- [ ] Main-bound pull request merged.

