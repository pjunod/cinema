# Content analysis repair — implementation status

**Status:** local acceptance green; hosted fast lane pending · **Updated:** 2026-09-17 UTC ·
**Base:** `f237d6180073bd395af7e08de00021d3aa7a2e33` · **Effort:**
`effort/content-analysis-repair`

Companion to the
[implementation contract](CONTENT-ANALYSIS-FAILURES-IMPLEMENTATION.md) — this
is the live ledger for source, compiler, review, qualification, and promotion
evidence. A checked row means the named evidence exists on the exact recorded
tree. Fleet recovery and deployment remain separate actions.

## Current state

| Package | State | Evidence | Next action |
|---|---|---|---|
| W0 · reproduce and reconcile | partial | isolated clone merged with current `main`; Rust 1.97.1 confirmed; queue, migration, probe, settings, and recovery seams reconciled | record the bounded live compatibility inventory before enablement |
| W1 · shared types and persistence | reviewed | typed bounded diagnostics, shared retry policy, SQLite/Hiqlite/local schema updates, and repair receipts compile | final fast lane |
| W2 · selected-video completion | qualified locally | held-source probe resolves the exact mapped video stream; rational lower-bound and timeline-span checks plus the longer-audio FFmpeg fixture pass | hosted fast lane |
| W3 · durable bounded retry | reviewed | fixed backoff, seven-day deadline, attempt preservation, deadline-clamped initial and renewed leases, and deadline-aware completion compile in both Store paths | exercise both backends in the final fast lane |
| W4 · identity-correct repair | reviewed | exact pipeline/video identity, current-recipe attribution for legacy and standalone rows, revisioned preview/apply candidates, atomic receipts, and stale revalidation compile | backend regressions in the final fast lane |
| W5 · operator surfaces | implemented | history diagnostics, truthful failure text, repair controls, and advisory Developer enablement compile | browser contract regression in the final fast lane |
| W6 · review and promotion | active | Forgejo PR [#352](http://192.168.4.7:3000/noirr/plurx/pulls/352) is open; adversarial review findings are addressed; focused local acceptance is green on `0db6c804` | push, mark ready, require the hosted Main promotion gate, and merge |

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
| Current intended base | `f237d6180073bd395af7e08de00021d3aa7a2e33` |
| Handoff's verified base | `363a22e28aa53094d899a9ad3c812241a6243548`; all seams are being rechecked |
| Pinned compiler | `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| Baseline compile | green: `cargo check -p plurxd --all-targets --locked` |
| Reviewed candidate compile | green on `112b1ae5`: `cargo fmt --all`; `git diff --check`; `cargo check -p plurxd --all-targets --locked`; `cargo check -p plurx-core --tests --features hiqlite-contract-tests --locked` |
| Focused regressions | green on `0db6c804`: core policy/SQLite 8; real three-voter Hiqlite 2; daemon completion/history 6; fragment index 28; audio-tail 1; web 3; docs index 4 |
| Final lint | green on `0db6c804`: pinned rustfmt and `cargo clippy -p plurxd --all-targets --locked -- -D warnings` |
| Adversarial implementation review | complete; six findings addressed: legacy recipe attribution, deadline-clamped claim/completion, stale-source precedence, blank-identity exclusion, whole-build budget, and configured local attempt limits |
| Main fast lane | not run; starts only after review findings are addressed and the draft PR is marked ready |
| Fleet recovery | not authorized or run |

## Completion

- [ ] W0 bounded compatibility inventory recorded before enablement.
- [x] W1 diagnostics, retry policy, and migrations implemented and compiled.
- [x] W2 selected-video completion implemented and compiled.
- [x] W3 retry lifecycle implemented in SQLite and Hiqlite and compiled.
- [x] W4 exact-identity preview/apply repair implemented and compiled.
- [x] W5 API, browse, analysis, Developer settings, and operations docs implemented.
- [x] One adversarial review addressed.
- [x] Focused local acceptance green after the one adversarial review.
- [ ] Hosted Main promotion gate green on the current PR head.
- [ ] Main-bound pull request merged.
