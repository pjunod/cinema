# Playback lifecycle — implementation status

**Status:** P1–P2 built and compiled; first promotion pending integration ·
**Updated:** 2026-09-12 · **Base:** `30cd51afc` · **Effort:**
`effort/playback-lifecycle` · **Task:** `codex/playback-lifecycle-p1-p2` at
`7c21d238`

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
| P1 / S02–S03 refill | built; runtime deferred | `7c21d238` models empty and 22-second loaded waits; existing rolling flow already resumes below its fixed demand target, so no duplicate refill-credit policy was added; loaded waits now outrank producer holds | integrate the finalized handoff docs, then promote with P2 |
| P2 / S04–S05 ownership | built; runtime deferred | Apple retains its pre-ask nudge; Android and web claim one native reevaluation per loaded wait, observe passive `none` under the existing absolute deadline, and fence stale intent before recovery | merge task into effort and compile the current-main result |
| First promotion | pending integration | source and changed test targets compile; Apple build 143 and Android versionCode 86 are claimed; no review requested | wait for reviewed handoff PR #256 to merge, integrate current `main`, open the draft main PR, then use exactly one review and one fast lane |
| P3 / S06–S07 prepared handoff | queued after first promotion | existing v1 local-switch → presentation → committed acknowledgement → durable settlement order is retained | re-plan compound candidates before removing proof vetoes; finish bounded settlement |
| P4 / S08 owner transition | queued after P3 | existing epoch, route, lease, and drain machinery is the foundation | connect planned drain to the prepared transaction; keep abrupt recovery bounded |
| P5 / S09 closeout | active throughout | this ledger owns decisions and deferred evidence | remove superseded authority, reconcile status/remainder, prepare the sweep run card |
| Second promotion | not opened | no review or fast lane run yet | repeat the one-review/current-head fast-lane process and merge only when green |

## Build receipt — compilation is not a test result

| Source | Command | Result |
|---|---|---|
| `30cd51afc` | `rustup run 1.97.1 rustc --version` | `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| `30cd51afc` | `rustup run 1.97.1 cargo check -p plurxd --all-targets` | passed in 69 seconds; compiled test targets without executing tests |
| `7c21d238` tree | `rustup run 1.97.1 cargo fmt --all -- --check` and `cargo clippy -p plurxd --locked --all-targets -- -D warnings` | passed; Clippy completed in 16 seconds on the final tree |
| `7c21d238` tree | `scripts/js-check` | passed; both shipped inline script blocks parsed without executing the browser suite |
| `7c21d238` tree | `make apple-build` | passed for iOS and tvOS at Apple build 143 |
| `7c21d238` tree | iOS and tvOS `xcodebuild ... build-for-testing` | both test bundles compiled; no simulator test executed |
| `7c21d238` tree | host Gradle `:app:assembleDebug :app:compileDebugUnitTestKotlin` with the installed SDK | passed at Android versionCode 86; the pinned Docker image build had timed out twice reading Docker Hub metadata, so the cached host toolchain supplied compile evidence without executing tests |

No unit, integration, browser, simulator, emulator, playback, or physical
suite has run in this campaign. Regressions added during implementation will
be marked `written; not run` until the separate sweep supplies a result.

### Focused regression inventory

| Surface | Assertion | Runtime result |
|---|---|---|
| rolling flow / server control | empty supply below its target reaches the existing resume path; a 22-second loaded wait is presentation-owned even with no fetch gap | written and compiled; not run |
| VOD | retained `accepted_seek_intent_controls_demand_and_retention_in_both_directions` and nearest-request cases keep the current contiguous gap ahead of distant work and pin admitted readers | existing source assertion retained; not run |
| Apple | loaded buffering and silent waits cannot be deferred by a producer hold; the existing monitor's one nudge and recovery deadline remain separate | written and compiled for iOS/tvOS; not run |
| Android | one reevaluation may be claimed, passive control does not restart the deadline, progress/reset owns the next episode | written and compiled; not run |
| web | both a producer hold and its server-suppressed `none` preserve one browser reevaluation without an immediate replacement | written and syntax-checked; not run |

### Recovery ownership audit

- Apple `startPlaybackRecoveryMonitor` remains the serialized native nudge and
  reopen executor. `startStatusPolling` observes delivery starvation and routes
  the result through the same `retrySameDeliveryAfterStall` executor; it does
  not attach or reopen independently. Seek-presentation expiry uses that same
  executor with current generation/action fences.
- Android `stallWatchdogJob` samples the one `OpenPlaybackStallTracker` and
  invokes `onStall`; `applyStallVerdict` may defer that tracker but cannot own a
  replacement. `restartAt` / the stall reopen coordinator remains the only
  attachment mutation path.
- Web `beginWait` owns one `persistentWait` timer per attached player and
  episode. The timer performs the native reevaluation, control ask, absolute
  deferral, and eventual existing replacement; control maintenance remains an
  observation/lease path. No legacy recovery executor was deleted because the
  audited call sites already converge on these owners.

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
5. **Do not add the proposed rolling refill credit.** The coupled fixture shows
   that supply below the existing fixed target already clears the time hold and
   reaches the producer resume operation. The demonstrated gap is downstream:
   a loaded client wait was not allowed to observe its own native recovery when
   a normal producer hold coexisted. The incident initiator remains unknown;
   this change fixes the reachable recovery boundary and does not claim a
   deterministic replay of the field freeze.
6. **Treat passive `none` as no server command.** Astra confirmed that Android
   and web should spend their client-local reevaluation before/surrounding the
   ask, then observe it under their existing absolute episode deadline. The
   server does not preserve or invent a hold-like action to trigger presentation
   recovery, and no wire vocabulary or second watchdog was added.

## Review and delivery — evidence must name the exact head

No adversarial review has been requested for the implementation and no
implementation main-bound PR exists yet. The separate finalized handoff-doc PR
#256 is waiting on its already-reviewed current-head gate; its Android runner
cannot currently route to Forgejo, so that infrastructure owner is monitoring
and will retry rather than bypass the gate. This implementation will not absorb
or re-review that PR before it lands.
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
