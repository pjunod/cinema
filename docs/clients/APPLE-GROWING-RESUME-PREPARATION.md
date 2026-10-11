# Growing HLS resume — distinguish server preparation from item readiness

**Status:** built; physical acceptance open · **Written:** 2026-10-08 · **Investigation base:** `f0c597df20297fe33c8016a6f027cc06ac54b26e`

Companion to [APPLE-BLACK-VIDEO-IMPLEMENTATION.md](APPLE-BLACK-VIDEO-IMPLEMENTATION.md) (the separate diagnostic-process incident) and [PLAYBACK-SURFACE-CONTRACT.md](PLAYBACK-SURFACE-CONTRACT.md) (which owner may report a failure). This document explains Naked Gun's premature preparation error, specifies its bounded repair, and records the evidence that does and does not establish an incident resolution.

## 1. A keyframe correction armed the wrong preparation deadline

The user reported “The video took too long to prepare,” followed by playback and “Playback recovered.” Both retained attempts used server revision `eb547f35d` and the same copy/Dolby Vision conversion/EAC3 command recipe, including `-readrate_initial_burst 90.0 -readrate 2.00`.

| Evidence | First attempt | Later successful attempt |
|---|---|---|
| Requested film position | 814.473 s | 822.264 s |
| Server media origin | 813.730 s | 822.238 s |
| Correction to requested position | 743 ms | 26 ms |
| `sessionAttachSeekMs` result | 743 ms; readiness wait entered | nil; below its 100 ms correction threshold |
| Apple readiness deadline | 15 s | No readiness deadline on this growing-HLS branch |
| Producer start to first segment | 600 ms | 285 ms |
| Producer start to first served playlist | 20.951 s | 3.814 s |
| First snapshot time, UTC | 05:51:55.258 | 05:53:09.634 |

`open()` correctly excludes a growing fresh start from `shouldBoundFreshStartReadiness`, because the server may still be filling its publication buffer. But when the server starts on an earlier keyframe, `seekAfterAttach` enters `seekWhenReady` instead. That path calls the same fixed 15-second item-readiness waiter used by direct/completed media. It does not distinguish an unpublished growing stream from a published item that AVPlayer cannot ready.

At 05:51:49.905, when the first attempt reported failure, its exact server session had `playlist_ready=false`, `producer_state=running`, 27,527 ms of produced media, and `progress_idle_ms=80`. The producer's progress deadline still had 9,919 ms remaining. The server was actively preparing valid media for its conservative 48-second first-publication threshold. The client declared failure approximately six seconds before publication.

The later attempt both produced faster and bypassed this readiness branch because its correction was only 26 ms. Its success is not proof that the same timed path recovered. Different throughput is established; cold storage cache, CPU contention, or another resource cause for that difference is not established by retained evidence.

## 2. The mismatch predates yesterday; the later AVPlayer error remains unclassified

| Change | Commit | Date |
|---|---|---|
| Resume readiness limited to 15 seconds | `fb7afea3f1` | 2026-08-01 |
| Growing-copy keyframe correction | `6feda24ad7` | 2026-08-15 |
| Named readiness constant and fresh-start bound | `47c06809a` | 2026-08-17 |
| 48-second first-publication threshold | `52c1a0fab3` | 2026-09-19 |
| Cumulative publication frontier | `6f4e279e6` | 2026-09-20 |
| Startup-policy split; production stays Conservative | `63af0d89e`, `8ecc102ad0` | 2026-10-02 |

The relevant rolling publication, copy segmenter, DV converter, and exact-init context files are unchanged between the retained October 7 image `9023815cb` and the incident image `eb547f35d`. No evidence identifies a yesterday-to-today introducing commit. A longstanding mismatch became visible on this slower resume path.

At 05:51:55.280, AVPlayer separately reported `NSURLErrorDomain -1008` / `CoreMediaErrorDomain -12884`. A relayed master request succeeded at 05:51:55.463, followed by an item-replacement fence at 05:51:56.708. The unchanged session/attempt identifiers and absence of another create until 05:53 support automatic node failover through `handleItemFailure` and `retryMediaOnNextNode`.

The production playlist budget is 55 seconds with five seconds reserved for response publication; relay preserves the caller's absolute deadline up to 60 seconds. Master/init inspection passes that deadline into the publication wait, rather than using the generic 20-second segment convenience timeout. Playlist deadline exhaustion therefore does not explain the error around 21 seconds.

The retained record cannot identify that failed HTTP request. `PlayerItemFailure.classify` discards error-log event details when `item.error` already exists; the stored payload consequently has no matching HTTP status/comment or request duration. The complete retained owner/ingress interval has no corresponding 404/503, but that does not prove AVPlayer saw no HTTP failure. Do not claim that suppressing the premature timer proves the later resource error eliminated. Native item failure must remain immediate in the repaired wait.

## 3. Preparation gets progress evidence and a fixed total bound

Keep the server's publication, buffering, compatibility, and HDR policies unchanged. Keep the 100 ms seek correction threshold: discarding the 743 ms correction would replay unwanted media and hide the timing defect.

Use one item-scoped readiness policy inside the existing readiness waiter:

| Phase | Bound and evidence |
|---|---|
| Direct/completed VOD or unavailable preparation evidence | Existing 15 seconds from wait start |
| Exact growing session still unpublished | Beyond 15 seconds, continue only with fresh authorized status: matching session ID, `playlistReady == false`, `producerState == running`, positive `producedEndMs`, present nonnegative `outTimeMs`, nonnegative `progressIdleMs`, and progress age plus sample age no greater than 10 seconds |
| First fresh `playlistReady == true` | Latch publication once; allow 15 seconds for AVPlayer readiness from that observation, never restarting this budget on later samples |
| Every growing wait | Absolute 60-second cap from this attachment's wait start, including the item-readiness phase |
| Native item `.failed`, cancellation, or lost ownership | Preserve immediate native failure/cancellation and existing ownership fences; healthy server progress cannot hide it |

A status sample is fresh only if its monotonic age is nonnegative and no greater than five seconds. Record request-start and response-observation monotonic timestamps separately. Request start measures freshness including network latency; the first accepted publication observation starts the item-readiness budget, capped by the original absolute deadline. Count that request age together with `progressIdleMs`; negative/unknown values never extend the wait. The lease's optional `startupState` is not required: the incident was a valid legacy session without that field.

The initial 15-second budget remains available even when telemetry is missing. After it is spent, missing, stale, mismatched, stopped, or unproductive preparation evidence cannot buy more time. Once publication is latched, the finite item-readiness budget proceeds independently of subsequent status availability, but the absolute cap remains.

```text
 attached item
      |
 ready or failed? ---- yes ---> return readiness / original error
      | no
 direct / completed? - yes ---> existing 15-second bound
      | no
 first fresh publication seen? -- yes --> one 15-second item-readiness budget
      | no                                      |
 initial 15 seconds remain? -- yes --> wait      |
      | no                                      |
 exact fresh producer advancing? -- no --> timeout
      | yes                                     |
 wait and sample existing evidence <------------+
      |
 60 seconds total reached ---------> timeout
```

Use the existing status poll and readiness observation. No extra HTTP polling, no second recovery ladder, no new video output, no new producer session, and no blanket 60-second timer for direct/VOD playback are needed.

## 4. Implementation ownership and lifecycle contract

This is one ordinary main-bound fix. All changes belong to one branch and one PR.

| Surface | Change |
|---|---|
| `clients/apple/Sources/PlayerItemReadiness.swift` | Pure preparation/item-readiness budget and exact-session status evidence |
| `clients/apple/Sources/PlayerController.swift` | Pass readiness context, feed monotonic request-age evidence from existing status polling, and use the policy in the existing observer/timer race |
| `clients/apple/Sources/Models.swift` | Decode the already-served optional `playlist_ready` field |
| `PlayerAttempt.swift`, `AttemptScopesTests.swift`, `validation/attempt-census.toml` | Give the post-staged-poll continuation its own audited `.open` fence; preserve pause and reject replacement |
| Apple readiness/ownership tests | Deterministic incident timeline, timer bounds, failures, cancellation, replacement, pause, and status decoding |
| Apple build metadata, README, parity/status/build note and client-fix ledger | Claim the next build and record the behavioral regression |
| This document and `docs/README.md` | Root cause, review dispositions, validation and acceptance limits |

The readiness context captures the expected item, session, and open/lifecycle owner. A predecessor's status cannot extend a successor's wait. Same-session ingress failover advances the open generation, invalidates old evidence, and restarts the single status poll for the replacement owner. Initial polling begins before item attachment, so its fence binds lifecycle/open/session rather than the predecessor item. Recheck that fence after every await, including optional staged telemetry. Clear evidence when polling/session state is retired. Restoring an incumbent restarts polling and clears preparation evidence; it cannot grant a fresh lease to an old sample. Existing `seekWhenReady` checks remain before the native seek and after each await; pause retains the requested position but cannot grant permission to play.

The production readiness bridge keeps subscribe-before-inspect ordering. A failed native item wins over a simultaneously observed publication or ready event. Test the actual observer/timer race through injected status, clock, and sleep boundaries; do not validate only a duplicated policy formula. The policy's timer is monotonic and finite. Cancellation must terminate both branches and retire observation rather than leaving a sleeping task behind.

## 5. Regressions and build sequence

1. Replay the first attempt: 743 ms correction, still unpublished and actively producing at 15 seconds, publication around 21 seconds, then ready. It must complete the seek once without raising the preparation error.
2. Preserve the 26 ms correction branch and the existing 100 ms threshold.
3. Reject missing/mismatched/stale/negative evidence and failed/held/nonprogressing producers after the original deadline; retain the ordinary 15-second direct/VOD bound.
4. Keep continuously healthy prepublication work under the absolute 60-second cap. Publication near that cap does not grant another full 15 seconds beyond it.
5. Latch publication once. Repeated fresh status or a producer restart cannot reset the item-readiness budget or attachment cap.
6. Exercise native failure before and after publication, including failure and publication/ready in the same scheduling turn. Return the original failure immediately.
7. Exercise cancellation and replacement while waiting. A late success/failure cannot seek or fault a different item. Preserve pause authority and the existing resume destination.
8. Decode current status and older responses without `playlist_ready`. Count delayed status-request time toward freshness. A 4.9-second publication response retains its full 15-second item-readiness budget, subject to the cap. Positive produced media with missing `outTimeMs` and zero idle cannot extend preparation. Exercise real same-session failover and a delayed predecessor poll response.

Run the focused readiness and ownership suites on tvOS and iOS, then the complete affected Apple suites and Release builds. Run the docs index, declared regression-reference checks, corrective-history audit, and normal pinned hook. The local Rust 1.97.1 compiler loop was established before changes: `cargo check --locked -p plurxd --all-targets` passed on base `f0c597df` in 2m06s. The fix contains no Rust behavior change.

Obtain adversarial plan review before implementation; record and address its findings here. Open the resulting PR as draft, obtain exactly one adversarial PR review, address its findings, run the current-head/base Main promotion gate, and merge with every declared `Regression-Test:` line in the landing message. A source change after validation requires the affected checks again.

## 6. Evidence and acceptance limits

Retained local evidence, available to this investigation:

- `/private/tmp/naked-gun-owner-startup-full.log`: complete owner interval, 05:51:30–05:51:58 UTC.
- `/private/tmp/avatar-naked-gun-m6-startup.log`: ingress interval.
- `/private/tmp/avatar-naked-gun-recent.log`: both attempts.
- `/private/tmp/avatar-naked-gun-surface-events.json`: server state at the premature client verdict.
- `/private/tmp/avatar-naked-gun-failure-full-events.json`: native failure and failover timeline.
- `/private/tmp/avatar-naked-gun-prior-events.json`: earlier telemetry, not physical-picture proof.

These were collected with read-only `docker logs --since ... --until ... plurxd` and `sqlite3 -readonly` queries. Do not copy credentials or media URLs into fixtures or logs.

Deterministic tests establish the repaired preparation decision and ownership behavior. They cannot establish why throughput differed or classify the separate AVPlayer resource error. Physical acceptance requires a named installed build and an observed resume on the Apple TV. Do not interrupt viewing or leave diagnostic apps/processes installed to obtain it.

## 7. Review and execution record

Adversarial plan review found three issues, all incorporated before implementation: (P1) rebind the single status poll after same-session failover and reject delayed predecessor responses; (P2) require a known producer progress witness (`outTimeMs`) because zero idle can mean unknown; (P2) separate request-start freshness from response-observation time for the one-shot item-readiness budget. The reviewer also requested “item readiness” terminology: `.readyToPlay` is not proof of displayed video. Implementation and local Apple validation are complete; merge qualification and physical acceptance are tracked separately below.


Implementation is PR [#914](http://forge.lan:3000/noirr/plurx/pulls/914), Apple build 217. The shared production waiter is exercised directly with injected monotonic time, native status, and cancellable sleep. Two controller tests additionally traverse actual growing-session attachment and same-session failover; the latter holds the predecessor HTTP result across replacement to prove it cannot populate successor evidence.

Validation passed: focused readiness/ownership suites on tvOS (34 tests) and iOS (40 tests); complete tvOS (866 tests) and iOS (882 tests) suites, zero failures; Debug test builds and Release builds on both platforms; docs index (4 tests). The normal commit hook passed pinned Rust formatting, workspace/all-target Clippy with warnings denied, catalog validation and all 77 served JavaScript syntax checks.

The single adversarial PR review examined implementation `2bc3425f5` plus the PR-specific documentation/ledger additions and found no actionable issues. It checked freshness and publication clocks, native failures, cancellation, ownership, failover polling, and whether regressions traverse the production waiter/controller. Qualification remains required before merging.

The first full history audit exposed two existing unrecognized API merge subjects: PR #909 at `4a737e92c` and PR #910 at `98af2eca9`. Forgejo reports both merged; their actual ordered parents, trees and retained regression fields were verified. Two bounded `landings` identity records let the existing audit recognize them while still resolving every original field against its own immutable tree. The audit boundary, parser, runtime and requirements remain unchanged. The existing identity-mutation test checks altered parents/tree/title and missing/invalid trailers are refused.


### Reproduce the build and focused checks

From the repository root on the pinned Apple toolchain, generate the project once:

```bash
xcodegen generate --spec clients/apple/project.yml
```

For `platform=tvOS Simulator,name=Apple TV 4K (3rd generation)` use scheme `plurx-tvOS` and test target `plurx-tvOSTests`; for `platform=iOS Simulator,name=iPhone 17 Pro` use `plurx-iOS` and `plurx-iOSTests`. Run this for each pair, substituting those values:

```bash
xcodebuild -project clients/apple/plurx.xcodeproj -scheme "$scheme" \
  -destination "$destination" -derivedDataPath /private/tmp/naked-gun-derived \
  -only-testing:"$test_target/AppleItemReadinessTests" \
  -only-testing:"$test_target/PlayerOperationOwnershipTests" \
  CODE_SIGNING_ALLOWED=NO test
# Remove both -only-testing arguments to execute the complete platform suite.
# For Release compilation, use -configuration Release and build instead of test.
python3 -m unittest discover -s tests/operations -p test_docs_index.py
python3 -m validation.regression_field --body-file /private/tmp/naked-gun-pr-body.md
make history-check
```

These commands use simulators only. Physical acceptance is a separate normal-app build 217 resume with the preceding keyframe more than 100 ms behind the selected position. Record the installed build, selection, preparation events, publication time and observed picture. A fresh start or a correction below 100 ms does not exercise the repaired branch. No physical acceptance was performed during this work.

If deployment reveals a regression, restore the previously accepted normal application build and revert PR #914 through the same qualification process. Do not reduce the server's 48-second publication buffer or remove resume alignment to conceal the error. Extended preparation is limited to fresh known progress and at most 60 seconds; absent evidence retains the earlier 15-second failure bound.


After the two identity records, `make history-check` passed: 3,383 corrective commits, 321 recognized landing commits, and 1,077 corrections covered by immutable landing trailers. The focused identity test passed all six variants (valid record; altered tree, parents, title; invalid or missing trailer). All five original PR #909/#910 regression fields resolve against their own landing trees. The final documentation index passed again. This record adds no runtime change after the successful Apple suites and Release builds.


The first candidate gate (run 4474) caught three repository-contract defects: the extra post-staged-poll continuation reused a fence name even though the census requires one case per continuation; two already-merged Pi status headers still claimed pending merge; and documentation contained machine-specific evidence names/URLs. The continuation now has its own `recoveryEvidenceAfterStagedPoll` case with the same `.open` scope, recorded in the census. Documentation uses neutral repository infrastructure names and the Pi headers record their actual PR #889 landing. No validation rule or allowlist was relaxed. All four run-4474 start/final preflight and Python journals were retained before changing the candidate; no CI attempt was rerun in place.

Final corrected source verification: all 866 tvOS and 882 iOS tests passed again with zero failures, and both Release builds passed. The 33 affected repository-contract tests and four docs-index tests passed. The new fence is also included in the existing exact-scope, pause-survival and previous-title rejection tests. The normal pinned hook passed after the correction. PR #914 carries 15 checked regression references; its current qualification and immutable merge receipt are recorded in the PR. Physical acceptance remains open.

Main advanced through PR #915 during qualification. The candidate integrates `a3158eadc9a4f26d2d8293b4a91f23a94c9012b6`; the Apple source tree is unchanged from the fully tested candidate. The two additive catalog conflicts preserve both contributions, including the verified historical landing identities and the already-landed errata. Qualification must run again against this current base; the earlier successful component jobs do not authorize merging a different combined tree.
