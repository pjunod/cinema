# Playback lifecycle — implementation status

**Status:** building P1–P2 · **Updated:** 2026-09-12 · **Base:**
`30cd51afc` · **Effort:** `effort/playback-lifecycle`

Companion to the
[implementation contract](PLAYBACK-LIFECYCLE-IMPLEMENTATION.md) (what to build),
the [coverage map](PLAYBACK-LIFECYCLE-COVERAGE.md) (required transitions), and
the [rewrite remainder](PLAYBACK-REWRITE-REMAINDER.md) (scope audit) — this is
the one execution ledger for *what is built, compiled, reviewed, and merged*.
Runtime and physical observations stay `not run` until the separate sweep
actually executes them.

## Progress — two main promotions, one review each

| Package | State | Current evidence | Next action |
|---|---|---|---|
| S01 foundation | complete | independent Forgejo clone at `30cd51afc`; Rust 1.97.1 `cargo check -p plurxd --all-targets` passed before edits | keep the warm compiler target and update this page with each package |
| P1 / S02–S03 refill | source audit | implementation contract and incident receipt read; no initiating freeze boundary is claimed yet | write the coupled wait regressions, then change only the boundary they demonstrate |
| P2 / S04–S05 ownership | queued behind P1 episode identity | existing Apple, Android, and web owners remain unchanged | consolidate one recovery executor per native adapter and compile all changed targets |
| First promotion | not opened | tests will be written and compiled but not executed; no review requested | merge current `main`, compile exact head, open draft PR, request one adversarial review, address it, then run the fast lane |
| P3 / S06–S07 prepared handoff | queued after first promotion | existing v1 local-switch → presentation → committed acknowledgement → durable settlement order is retained | re-plan compound candidates before removing proof vetoes; finish bounded settlement |
| P4 / S08 owner transition | queued after P3 | existing epoch, route, lease, and drain machinery is the foundation | connect planned drain to the prepared transaction; keep abrupt recovery bounded |
| P5 / S09 closeout | active throughout | this ledger owns decisions and deferred evidence | remove superseded authority, reconcile status/remainder, prepare the sweep run card |
| Second promotion | not opened | no review or fast lane run yet | repeat the one-review/current-head fast-lane process and merge only when green |

## Build receipt — compilation is not a test result

| Source | Command | Result |
|---|---|---|
| `30cd51afc` | `rustup run 1.97.1 rustc --version` | `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| `30cd51afc` | `rustup run 1.97.1 cargo check -p plurxd --all-targets` | passed in 69 seconds; compiled test targets without executing tests |

No unit, integration, browser, simulator, emulator, playback, or physical
suite has run in this campaign. Regressions added during implementation will
be marked `written; not run` until the separate sweep supplies a result.

## Decisions — assumptions made without waiting

1. **Use two substantial main promotions.** P1–P2 restore and consolidate the
   ordinary playback loop; P3–P4 plus P5 complete prepared and owner
   transitions. This follows the contract and keeps the freeze repair from
   waiting on cluster relocation.
2. **Treat the freeze initiator as unknown until a regression isolates it.**
   The retained trace proves that waiting and a producer time hold coexist,
   including with about 22 seconds loaded. It does not prove that pacing caused
   either freeze, so no speculative global timer or buffer change is justified.
3. **Use advice, never qualification, for enablement.** Developer settings may
   report `met`, `not met`, or `unknown` with observed facts and age. The user
   can still enable the feature; only real runtime capability, ownership,
   authorization, or resource outcomes can fail an operation.
4. **Do not deploy as part of source completion.** Main promotion and the
   explicitly separate runtime/device sweep are recorded independently. The
   assignment authorizes implementation and merge, not an unsolicited fleet
   release.

## Review and delivery — evidence must name the exact head

No adversarial review has been requested and no main-bound PR exists yet.
Each promotion will be opened as draft only when its source, regressions,
documentation, counters, and compilation are complete. Exactly one adversarial
review will examine that merge-ready draft. Author corrections will be made
without requesting re-review; only then will the PR become ready and receive
`fast-lane`. A green `Main promotion gate` must name the current head before
merge.

## Cleanup — keep only reusable build state

The working clone is `/private/tmp/plurx-codex-playback-lifecycle-20260912`.
The failed explicit deploy-key attempt left no clone. The token askpass helper
contains no token; it will be removed when Forgejo API work is complete. The
warm Rust `target/` is retained deliberately for repeated compiler checks and
will be removed with the temporary clone after both promotions are merged.
