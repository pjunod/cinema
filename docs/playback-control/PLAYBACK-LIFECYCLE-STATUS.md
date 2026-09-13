# Playback lifecycle — implementation status

> **Next-work reconciliation, 2026-09-12:** The lifecycle implementation landed
> through PR #259 at `5548c3d3`. For remaining work and current execution rules,
> read the [rewrite remainder](PLAYBACK-REWRITE-REMAINDER.md). The test-deferral
> and fast-lane-only instructions below describe the earlier campaign; Paul’s
> replacement AGENTS.md requires focused local tests and current qualification
> for the new effort. Historical receipts below remain unchanged.

**Status:** combined B01–B05 implementation, review, qualification and main
promotion complete · **Updated:** 2026-09-12 · **Promotion:**
[PR #263](http://192.168.4.7:3000/noirr/plurx/pulls/263), merge `eaecb199`

Companion to the
[implementation contract](PLAYBACK-LIFECYCLE-IMPLEMENTATION.md) (what to build),
the [coverage map](PLAYBACK-LIFECYCLE-COVERAGE.md) (required transitions), and
the [rewrite remainder](PLAYBACK-REWRITE-REMAINDER.md) (finite evidence debt) —
this is the one execution ledger for *what is built, compiled, reviewed, and
merged*. Runtime and physical observations stay `not run` until the separate
sweep actually executes them.

## Remainder execution — B01–B05 integrated

**Status:** source complete; one adversarial review addressed; focused matrix
and main-promotion lane green; merged · **Started:**
2026-09-12 · **Base:** `10f2afe60b3d177866fdcc5741acd9f494525d73` ·
**Effort:** `effort/playback-rewrite-remainder` · **Clone:**
`/private/tmp/plurx-sol-remainder-20260912`

| Package | State | Current evidence | Next action |
|---|---|---|---|
| B01 · buffer observability/UI | complete | server PR #262 and client PR #264 merged through `effort/buffer-observability`; its complete focused receipts remain in `PLAYBACK-BUFFER-OBSERVABILITY-STATUS.md` | none |
| B02 · refill and operation races | complete; focused green | Apple rechecks session/open generation and viewer-action epoch after its ask; Android binds media-request and transport generations; web binds player, wait timestamp, playback generation, and control-intent generation | none |
| B03 · prepared transition | complete; focused green | recipe planning, both engines, exact settlement, timeout, supersession, readiness, and terminal cleanup anchors are named below | none |
| B04 · relocation and alternate ingress | complete; focused green | planned-fence cancellation, takeover, relay ingress, ownership settlement, and cleanup anchors are named below; destructive physical fault injection remains an explicit evidence gap | run the separately scoped physical sweep when the device/fleet is available |
| B05 · subtitle and compatibility | complete; focused green | subtitle windows/retry, seek coalescing, VOD/rolling identity, and compatibility owners are named below; `eb8fb452` puts authoritative prepared-handoff enablement and advisory readiness together in web Developer settings | none |
| Final promotion | complete | the one review of `83693106` was addressed by `09cd99c4` and `4e0991f0`; exact head `91eb2546` passed run 1903 and merged through PR #263 at `eaecb199` | none |

### Current decisions and boundaries

1. **Use one substantial effort branch and one main-bound pull request.**
   Logical commits remain reviewable without paying for multiple broad gates.
2. **Keep the repository's focused pre-push proofs narrow.** The current user
   request defers tests until final review, while `AGENTS.md` requires the
   smallest changed-behavior regression before a push. Only that focused proof
   runs when a source commit needs it; the broad fast lane runs once, after the
   adversarial review is addressed.
3. **Treat all enablement requirements as advisory.** Missing device, fleet,
   throughput, or runtime evidence is shown in Developer settings and in this
   ledger, but it never rewrites or rejects the user's enable choice.
4. **Do not overlap B01.** Any needed edit to shared player-input fixtures,
   `PlaybackSessionStatus` wire models, or stats rendering is coordinated with
   the B01 owner before commit.
5. **Keep experimental enablement in Developer settings.** Apple and Android
   already did. Web now places the server-wide prepared-handoff switch beside
   its browser capability control and safety evidence; the saved switch stays
   authoritative even when readiness is missing or unmet.

### Static ownership receipt

The B02 wait/control association is not inferred from a shared session ID.
Apple accepts the status sample only for the polled session and current open,
then rechecks both the open generation and viewer-action epoch after the
control await. Android captures a `ControllerStallGuard.Observation` containing
both request and transport generations and rejects it after any pause, seek,
selection, visibility, transport, or playback-attempt change. Web's
`persistentWait` checks the exact player, wait timestamp, playback generation,
and control-intent generation both before and after its ask. No second recovery
executor or new timer is required.

### Remainder regression matrix

The representative anchors name the real fixture that covers the package; no
row treats source presence as a pass. These focused results were collected only
after the one adversarial review, in the requested final validation phase.

| Package | Representative retained anchors | Result |
|---|---|---|
| B02 · server refill | `lifecycle_refill_empty_and_loaded_waits_are_not_answered_with_their_hold` · `lifecycle_refill_loaded_native_wait_outranks_a_producer_hold` | green: 2 Rust tests |
| B02 · client ownership | Apple `testAViewerCommandRacingAStallDropsTheStallBinding` and `testStartStopStartRestoresOnlyTheNewTitlesPlaybackIntent` · Android `PlaybackTelemetryTest` and `PlaybackRequestOwnershipTest` · `tests/playback/web-control.test.js` | green: included in 37 Apple, 22 Android, and the complete web-control run |
| B03 · server transaction | `prepared_candidate_reuses_the_ordinary_plan_for_original_and_compound_changes` · retained settlement/timeout/abort anchors | green: representative Rust transaction test plus complete prepared client groups |
| B03 · clients and stores | Apple prepared-replacement groups · Android `Prepared*` and `PlaybackControlPrepared*` groups · web prepared-replacement cases | green: 34 Apple prepared tests, 50 Android prepared tests, and complete web-control run |
| B04 · ownership and ingress | `a_fresh_takeover_engine_discards_the_departed_owners_preparation` and retained relocation/relay reconciliation anchors | green: representative takeover test; destructive live-cluster fault injection not run |
| B05 · subtitle, seek, engines | `server_ready_uses_absolute_origin_and_does_not_cross_pruned_media` · `a_far_seek_reports_the_frontier_the_client_can_actually_fetch` · `a_twenty_seek_storm_starts_work_only_for_the_settled_target` · client readiness-retry cases | green: 3 focused Rust tests plus complete selected client/web runs |

### Combined adversarial review

The one requested adversarial review examined frozen candidate `83693106`
without running tests. All four findings are resolved; no second review was
requested.

| Finding | Disposition |
|---|---|
| Web prepared switches left predecessor health and presentation age attached to the successor | switch now clears those observations, and rollback restores the predecessor snapshot |
| Apple and Android treated the first position sample as presentation progress | both trackers keep progress age unavailable until a later sample proves real movement |
| Rolling and immutable VOD could publish an anchored or behind run as the later-ready island | both engines now require a strictly later materialized interval |
| Android painted paused Media3 buffering as an active playback wait | waiting now also requires the user's play intent; the overlay consumes that shared predicate |

The final test phase found one misplaced Swift field that prevented the Apple
test bundle from compiling and two stale web fixtures. Commit `4e0991f0`
corrects those demonstrated failures. The rebuilt Apple bundle and every
selected test then passed.

### Final focused validation receipt

| Surface | Command scope | Result |
|---|---|---|
| Rust 1.97.1 | two refill tests plus one representative each for rolling readiness, VOD frontier, prepared planning, takeover, and seek coalescing | green: 7 passed, 0 failed |
| Web | playback policy, input contract, player DOM, full playback-control reporter, and settings sections | green; settings sections reported 26/26 |
| Android | `PlaybackTelemetryTest`, `PlaybackRequestOwnershipTest`, all `Prepared*`, and both `PlaybackControlPrepared*` groups | green: 72 passed, 0 failed |
| Apple iOS simulator | exact-head `build-for-testing`, two telemetry/ownership cases, one operation-ownership case, and four prepared-replacement groups | green: 37 passed, 0 failed |

### Main promotion receipt

Exact candidate `91eb25464ca2ca9511c91c58c8aa294b16cf9c62` passed
[main fast-lane run 1903](http://192.168.4.7:3000/noirr/plurx/actions/runs/1885):
scope · mobile release version · policy/contracts · web · Rust · Apple ·
Android · `Main promotion gate` all succeeded. Because every rostered
`android-kvm` runner was offline, Android used one capability-matched ephemeral
runner on trusted `nynuc`; the host is x86_64 and exposes `/dev/kvm` and Docker.
No changed SSH host key was trusted or bypassed. The runner self-deleted after
its one job, its temporary directory and diagnostic log were removed, and the
four merged task/effort branches were pruned after ancestry checks.

`main` was still the reviewed base `10f2afe6` immediately before merge. PR #263
then merged the unchanged qualified head as
`eaecb19988851c1efb1c8beb541b6a5b497643de`.

### B05 action-making caller reconciliation

| Symbol / path | Owner and requirement | Disposition |
|---|---|---|
| Server `resolve_action` | one control exchange chooses typed hold, retry, or terminal advice from fenced client/server facts | retain; this is the server decision owner, not a media attachment executor |
| Server `process_preparation_candidate` plus `PreparationExecutor` | one durable successor transaction; client capability and saved server enablement are explicit asks, while throughput/fleet evidence is advisory | retain; exact slot, deadline, commit, abort, and cleanup ownership is already bounded |
| Server `attempt_takeover` and release reconciliation | abrupt owner-loss authority under durable epoch/lease fencing | retain; this is cluster ownership recovery, not a duplicate client stall owner |
| Apple `startPlaybackRecoveryMonitor` / `retrySameDeliveryAfterStall` | serialized native nudge and bounded reopen; status polling contributes facts to the same executor | retain; detector count is not executor count, and B01 needs the status observations |
| Android `OpenPlaybackStallTracker` / `onStall` / `restartAt` | one absolute no-progress episode and one attachment mutation path | retain; pause, visibility, selection, and generation guards fence stale work |
| Web `beginWait` / `persistentWait` | one timer per player wait episode; control is consulted before the existing bounded replacement | retain; `stallDiagnose` and `retryPlayback` are viewer actions, not automatic competitors |
| Apple/Android `retryMediaOnNextNode` | same session and authorization retried through a different advertised ingress | retain; alternate ingress changes transport, not playback identity or durable owner |
| Client prepared-replacement coordinators | one successor per exact action; switch requires contiguous runway and an observed successor frame | retain; failure settles once and never creates a hidden session-wide veto |
| Native/web subtitle retry and seek coalescers | same video session for native text; latest destination/selection owns work | retain; subtitle observation does not restart video, and abandoned destinations release their work |
| Rolling HLS and immutable VOD adapters | selected engine keeps its own pacing/materialization and terminal lifetime | retain both; rolling retirement is a separate product decision, not cleanup for this effort |

The old M9 fixed-timer and blanket `/status` deletion instruction is now marked
superseded in `REMAINING-ROADMAP-HANDOFF.md`. No caller above is deleted merely
to satisfy that historical count.

## Historical lifecycle campaign — PR #259 receipt

The tables below preserve the already-merged lifecycle campaign's original
branches, test deferral, review, and promotion evidence. They do not describe
the active remainder candidate above.

| Package | State | Current evidence | Next action |
|---|---|---|---|
| S01 foundation | complete | independent Forgejo clone at `30cd51afc`; pinned Rust 1.97.1 compiler loop established before Rust edits | retain only reusable build state until merge |
| P1 / S02–S03 refill | integrated into effort | `7c21d238` models empty and 22-second loaded waits; the existing rolling flow already resumes below its fixed demand target, so no duplicate refill-credit policy was added | include in the integrated main promotion |
| P2 / S04–S05 ownership | integrated into effort | Apple retains its one pre-ask nudge; Android and web claim one native reevaluation per loaded wait, observe passive `none` under the existing absolute deadline, and fence stale intent before recovery | include in the integrated main promotion |
| P3 / S06–S07 prepared handoff | built and compiled; runtime deferred | successor planning reuses create-time capabilities; Original, grade, audio, offset, subtitle, and compound changes produce one recipe; VOD and rolling prime through their distinct engines; proof/headroom/direction vetoes and session-wide failure suppression are removed | include in current-main integration |
| P4 / S08 owner transition | built and compiled; runtime deferred | restart/maintenance fences cause the next accepted local control exchange to reserve and prime one remote successor; durable identity precedes resource allocation; cancellation is checked before reserve, after reserve, after prime, and before commit | compile and inspect current-main integration |
| P5 / S09 closeout | complete | obsolete proof/headroom authority is removed from code comments, superseded M6 briefs are marked as such, and Developer settings expose explicit advisory enablement | preserve receipts through promotion |
| Main promotion | fast lane correcting operations truth on PR #259 | Apple build 143 and Android versionCode 86 are claimed once; the one adversarial review's six blockers are addressed; history, catalogue, and ownership inventory now pass; operations checks identified the undocumented internal prepare route and an advisory Apple status that was not explicit | publish the API/readiness correction and its anchor, retrigger `fast-lane`, and merge only its current green head |

The original plan allowed two main promotions. No implementation slice reached
`main` before P3–P4 completed, so one integrated promotion is smaller in CI and
review cost without weakening the final-head rule. Task commits and task PRs
remain separate history inside the effort branch.

## Build receipt — compilation is not a test result

| Source | Command | Result |
|---|---|---|
| `99cf1c6e` tree | `rustup run 1.97.1 cargo check -p plurxd --locked --all-targets` | passed in 1 minute 15 seconds; established the pinned compile loop without executing tests |
| `30cd51afc` | `rustup run 1.97.1 rustc --version` | `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| `30cd51afc` | `rustup run 1.97.1 cargo check -p plurxd --all-targets` | passed in 69 seconds; compiled test targets without executing tests |
| `7c21d238` tree | pinned Rust fmt and Clippy | passed; Clippy completed in 16 seconds |
| `7c21d238` tree | `scripts/js-check` | passed; shipped inline scripts parsed without running browser tests |
| `7c21d238` tree | `make apple-build` plus iOS/tvOS `build-for-testing` | passed at Apple build 143; test bundles compiled but did not execute |
| `7c21d238` tree | host Gradle `:app:assembleDebug :app:compileDebugUnitTestKotlin` | passed at Android versionCode 86; test source compiled but did not execute |
| `139ad01f` tree | `rustup run 1.97.1 cargo fmt --all -- --check` | passed |
| `139ad01f` tree | `rustup run 1.97.1 cargo check -p plurxd --locked --all-targets` | passed in 27 seconds; compiled test targets without executing tests |
| `139ad01f` tree | `rustup run 1.97.1 cargo clippy -p plurxd --locked --all-targets -- -D warnings` | passed in 34 seconds |
| `139ad01f` tree | `scripts/js-check` | passed; shipped inline scripts parsed without running browser tests |
| `139ad01f` tree | `make apple-build` plus iOS and tvOS `build-for-testing` | passed at Apple build 143; test bundles compiled but did not execute |
| `139ad01f` tree | `ANDROID_HOME=/Users/pjunod/Library/Android/sdk ./gradlew :app:assembleDebug :app:compileDebugUnitTestKotlin --no-daemon` | passed at Android versionCode 86; test source compiled but did not execute |
| `180ddcdd` tree | pinned Rust fmt, `cargo check -p plurxd --locked --all-targets`, and Clippy with `-D warnings` | passed in 2, 7, and 13 seconds respectively |
| `180ddcdd` tree | `scripts/js-check` | passed; two shipped inline script blocks parsed |
| `180ddcdd` tree | `make apple-build` | passed for iOS and tvOS at Apple build 143 |
| `180ddcdd` tree | iOS and tvOS `xcodebuild ... build-for-testing` | passed; changed XCTest sources compiled without execution |
| `180ddcdd` tree | `ANDROID_HOME=/Users/pjunod/Library/Android/sdk ./gradlew :app:assembleDebug :app:compileDebugUnitTestKotlin --no-daemon` | passed in 6 seconds at Android versionCode 86; no tests executed |
| `88066364` tree | pinned Rust fmt, `cargo check -p plurxd --locked --all-targets`, and Clippy with `-D warnings` | passed in 2, 8, and 15 seconds respectively; test targets compiled without execution |
| `88066364` tree | `ANDROID_HOME=/Users/pjunod/Library/Android/sdk ./gradlew :app:assembleDebug :app:compileDebugUnitTestKotlin --no-daemon` | passed in 11 seconds at Android versionCode 86; changed app and test sources compiled without execution |

The compiler loop will be repeated only if `main` moves before review or review
corrections change compiled source. No unit,
integration, browser, simulator, emulator, playback, or physical suite has run
in this campaign. Regressions remain `written; not run` until the separate
sweep supplies results.

### Focused regression inventory

| Surface | Assertion | Runtime result |
|---|---|---|
| rolling flow / server control | empty supply below its target reaches the existing resume path; a 22-second loaded wait is presentation-owned even with no fetch gap | written and compiled; not run |
| VOD | existing demand/retention cases preserve the current contiguous gap and pin admitted readers | existing assertion retained; not run |
| prepared planning | Original may restore copy/remux; quality, delivery, grade, audio, offset, and subtitle changes produce one capability-aware recipe; all axes and link observations remain eligible | written and Rust-compiled; not run |
| prepared lifecycle | Apple, Android, and web allow a later explicit action after a failed attempt; web proves first presentation with frame callback or monotonic decoded-frame fallback | written and source-compiled where noted; not run |
| planned relocation | exact restart/maintenance generation fences placement; the remote wire names only the already-reserved identity and cannot carry a recipe | written and Rust-compiled; not run |
| cancellation / cleanup | stale fence generations cannot commit; refused, cancelled, expired, or actor-rejected successors discard the durable row and retire the exact local or remote worker | written and Rust-compiled; not run |

### Recovery ownership audit

- Apple `startPlaybackRecoveryMonitor` remains the serialized native nudge and
  reopen executor. `startStatusPolling` observes delivery starvation and routes
  through the same executor; it does not attach or reopen independently.
- Android `stallWatchdogJob` samples the one `OpenPlaybackStallTracker`.
  `applyStallVerdict` may defer that tracker but cannot own a replacement;
  `restartAt` remains the attachment mutation path.
- Web `beginWait` owns one `persistentWait` timer per player and episode. It
  performs the native reevaluation, control ask, absolute deferral, and existing
  replacement; control maintenance remains observation and lease traffic.
- Prepared replacement is a separate bounded transaction on every client. A
  failed transaction settles only its action and does not create a session-wide
  capability veto.

## Decisions — assumptions made without waiting

1. **Use one integrated main promotion.** P1–P4 completed on the effort before
   any implementation main PR opened. One promotion preserves proper task
   commits while paying for exactly one adversarial review and one fast lane.
2. **Treat the freeze initiator as unknown until a regression isolates it.**
   The retained trace proves a wait and producer hold coexisted with about
   22 seconds loaded; it does not prove pacing initiated either freeze.
3. **Use advice, never qualification, for enablement.** Developer settings show
   measured, missing, and unknown facts but never rewrite or disable the user's
   choice. Runtime capability, authorization, ownership, and actual resource
   outcomes remain legitimate operation boundaries.
4. **Retain create-time planning facts beside the additive durable response.**
   The strict mixed-version worker request keeps `deny_unknown_fields`; a new
   owner can still re-plan a successor after takeover without guessing caps.
5. **Reserve remotely before allocating remotely.** The preparation wire names
   only incarnation, session, user, protocol, and owner epoch. The target reads
   and validates the durable recipe, so a peer cannot invent work outside the
   reservation transaction.
6. **Let planned drain ride the next accepted control exchange.** Restart and
   maintenance already fence new work while existing control remains live. The
   exact fence token is sufficient intent and avoids a second session scanner
   or watchdog.
7. **Do not deploy as part of source completion.** Merge and runtime/device
   observation are separate events. No fleet release was authorized here.
8. **Respect the cross-task privacy boundary.** A detailed progress message to
   Astra's task was rejected because it contained private-repository details.
   The implementation continued from Astra's checked-in contract; no alternate
   channel or secret-bearing workaround was used.

## Separate sweep run card — finite evidence, no merge gate

Run this after an explicitly deployed build exists. Record pass, fail, or not
run for every row; an unavailable device is an evidence gap, never a hidden
enablement gate.

| Run | Required observation | Record |
|---|---|---|
| Apple TV, forced VOD | 45–60 minutes normal play; pause/resume, cold seek, restart, compound quality/track change, stop during preparation | installed client/server builds, actual engine, freezes, reopens, TTFF/resume time, prepared commit or fallback, cleanup |
| Apple TV, forced rolling | same bounded sequence, including one producer hold/resume if it occurs naturally | same fields; say `not observed` when no hold occurs |
| Android | one successful prepared handoff, one failed attempt followed by a later retry, one compound change | device class, tunneling state, first frame, fallback interruption, resource release |
| web | frame-callback path where supported and decoded-frame fallback where absent | browser build, proof path, first presentation, retry after failure |
| cluster | restart and maintenance relocation with cancellation before reserve, after reserve, after prime, and before commit | exact fence generation, predecessor tuple, target owner, final route, orphan count |
| abrupt owner loss | existing takeover path without planned-fence metadata | epoch increment, bounded interruption, stale-owner refusal, cleanup |

R09 fleet breadth remains evidence debt for the manual sweep. R07 fallback
retirement and R10 semantic/prewarm expansion remain separate product
decisions; neither keeps this implementation open.

## Review and delivery — one review, author-verified corrections

The one adversarial implementation review examined draft PR #259 at
`69b2e2b2` and requested six corrections. Commit `88066364` addresses all six;
no second review or re-review was requested.

| Finding | Disposition |
|---|---|
| Android native reevaluation survived presentation progress | progress now resets the per-wait allowance; the retained multi-episode regression directly covers it |
| Original overrode explicit codec/grade policy | the compound policy is resolved against source facts; matching sources may copy, required conversions transcode, and unsupported outputs receive a typed refusal |
| planned-outage cancellation could cross the durable commit | the exact planned fence is retained with the successor and its operation guard is held from the current-token predicate through the Store pointer CAS |
| reserve or actor-activation timeout could lose cleanup ownership | detached reservation and activation tasks transfer one cancellation-safe guard; a late answer without a receiver aborts the exact row, slot, and worker |
| prepared work competed as foreground and ignored incumbent starvation | speculative admission uses only spare capacity, never registers ahead of foreground, and both an admission waiter and an accepted Waiting/Stalled exchange cancel the exact successor |
| saving prepared handoff off left in-flight or staged work alive | the settings transition wakes current candidates, serializes the final setting read with reservation, and settles every unswitched registered successor before reopening |

The stale Android and Rust phase comments called out by the reviewer were also
removed. PR #259 moved ready and received `fast-lane` at `af02d539`. Its first
preflight accepted validation scope and mobile-version bookkeeping, then found
two superseded client-fix anchors plus missing anchor rows for `7c21d238` and
`88066364`. Commit `5b28ae7e` updated that ledger and removed the stale Apple
test expectation that a failed attempt disables later preparation; the second
preflight then correctly required `5b28ae7e` itself to join that Apple anchor
row. At `3fd3ea5c`, history and the functionality catalogue passed; the
ownership inventory then reported the exact admission-token, publication-fence,
blocked-sentinel, task-owner, and timer-constructor deltas introduced by the
prepared lifecycle. Those counts are reconciled to their concrete owners in
the inventory. At `63d5a6a3`, all 198 validation-contract cases passed before
operations checks reported two documentation assertions for the new internal
prepare route and one Apple advisory-label assertion. The route and total are
now documented, and Apple explicitly reports the still-uncollected fleet/link
evidence as `Not met · Checked during playback`; neither status gates its
toggle. The Apple source correction is pinned by a current production-to-test
anchor in the client-fix ledger. Merge still requires a green
`Main promotion gate` for the corrected exact head.

## Cleanup — keep only reusable build state

The working clone is `/private/tmp/plurx-codex-playback-lifecycle-20260912`.
The warm Rust `target/` is retained only for repeated compiler checks and will
be removed with the temporary clone after the integrated promotion merges. No
deploy-key copy, repository credential, generated Xcode project, APK, derived
data, or test artifact belongs in the commit.
