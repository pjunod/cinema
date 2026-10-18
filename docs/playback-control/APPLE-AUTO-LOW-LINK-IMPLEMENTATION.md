# Apple Auto acceptance — preserve sign-in and identify acquisition failures

**Status:** open; label repair built, acquisition diagnosis incomplete ·
**Written:** 2026-10-17 · **Branch:** `effort/apple-auto-declared-costs`,
PR #1156; follow-up `codex/cq-acceptance-followup`

Companion to [continuous-quality build §10.330](CONTINUOUS-QUALITY-BUILD.md)
(the declared-cost correction) and the
[native harness](../../scripts/cq-native-shipping/README.md) (measurement and
launch restrictions). This is the remaining acquisition investigation and
the record of the sign-in incident. Preserve the failed runs before cleanup.
Do not launch the installed Release app through the shaping proxy until the
credential and server-selection incident is understood.

## 1. What the retained evidence establishes

Apple 252 on the iPhone 17 Pro Max and iPad Pro passes 15/15 manual changes
per device but holds the 28.8 Mb/s original of **Agent Zero** at 10 Mb/s,
stalls and fails its reopened item. Apple 260 at `75cba58d5` steps down to
720p during 100/10/25/10/25 Mb/s with no reported interruption. Its later
prepared acquisitions fail; the entire campaign remains failed.

The retained `run260-17pm/auto-shaper-evidence.json` and `auto.log` correct
the handoff's attribution to the 3 Mb/s stage:

| UTC, 2026-10-18 | Observation | Meaning |
|---|---|---|
| 01:57:30 | Preparation begins; incumbent runway 61.825 s | Link stage is still 25 Mb/s. |
| 01:57:36 | `stage=building`, `item=unreported`, `ready_ms=0`, overlap 5.831 s | The 6 s metadata deadline expires; no item error was reported. |
| 01:57:40 | Harness requests 3 Mb/s | The first failure preceded this cliff. |
| 01:57:44 | Preparation begins; runway 59.803 s; acquisition target advances 6 s | A second acquisition begins under pressure. |
| 01:57:50 | Same metadata deadline expires; overlap 5.834 s | A second preparation fails. |
| 01:57:51–01:58:06 | Reopen, then item readiness failure | The 15 s reopened-item bound expires. |

Two failure rows for the first acquisition do not prove two acquisitions.
The coordinator logs abandonment before disposing the item; the Auto
fallback previously logged abandonment again after disposal, producing
`ready_ms=unknown:overlap_ms=0`. The follow-up removes that duplicate.

`item=unreported` is the result of
[`preparedSuccessorErrorDetail(nil)`](../../clients/apple/Sources/PreparedReplacement.swift),
not an AVFoundation error code. Aggregate shaper bytes and client events
do not identify the successor's HTTP requests, candidate, producer state,
metadata response or acquisition latency. They cannot distinguish producer
delay, transfer contention and selection of an unsuitable successor.
Increasing a deadline alone is not a supported correction.

The production ingress's retained Docker log window, 01:57:20–01:58:15 UTC,
also shows two resurrected VOD attachments to the same rendition, producer
generations starting at entries 23 and 39, and `Replaced` terminal records
near the client deadlines. Those records do not attribute each reader or
blocked GET to a successor; `Replaced` also has fail-closed reconciliation
uses. They do not prove a wrong seek or producer restart cause. The retained
rendition directory has no remaining files. The 02:07:55–02:08:20 UTC TV
startup log window contains no 401/403 trace identifying a sign-out request.
The wider 01:52–02:05 UTC log window also contains no matching producer
failure diagnostic that explains these acquisitions.

## 2. Product bug: sign-in and server picker cannot be escaped

**Reported by Paul, 2026-10-17; unresolved.** After the harness runs, the
iPad Pro is on the server picker and the Bedroom Apple TV is on Sign in.
Paul reports that the UI will not let him leave those screens. The retained
TV UI log confirms Sign in, Use a different server and Report a problem
buttons. It does not reproduce which recovery control fails. Treat the
navigation trap as a product bug, separately from the harness incident.

**Confirmed mechanism, incident request unconfirmed.** The harness launches
`tv.plurx.app` with `-plurx.origin <proxy>`. `SettingsStore.origin` reads the
UserDefaults argument domain as well as saved preferences. Release
[`AppModel.bootstrap()`](../../clients/apple/Sources/AppModel.swift)
therefore attaches the saved bearer to that origin. A 401/403 clears the
Keychain token; another startup error may rediscover the saved instance and
persist its address. The DEBUG acceptance safeguards do not protect this
Release launch. The harness's earlier claim that nothing is persisted was
incorrect. No retained request/status trace identifies which branch ran on
either device, and no credential deletion or server change is attributed to
a particular request here.

**Acceptance checks.** Reproduce the screen trap with a disposable simulator
account, verify working server selection and sign-in recovery, and cover the
failing control with a UI regression. Before any physical proxy rerun, prove
that launch overrides cannot send the viewer's saved bearer to an unverified
server or mutate the saved origin, identity or token on failure. Use a
disposable app identity and account for investigation. Preserve existing
physical app data; never erase or reinstall it to make the test pass.

## 3. Repairs with a confirmed source cause

The method label read manual `selectedHeight` and the initial `decision`.
Auto keeps that selection nil after a prepared promotion changes the active
candidate and session height, so 1280×720 still appeared as Remux · HLS.
`onlinePlaybackMethodLabel` now uses the validated installed candidate and
installed height, retaining offline, manual, burn and overlay labels. A
rollback restores the installed identity and therefore its label.

`refreshMissingAutoCosts` becomes `refreshUnmeasuredAutoCosts`: declared
costs already exist; the bounded refresh seeks complete measured outputs.
The ledger and test commentary now name Agent Zero (review F7).

Prepared failure diagnostics capture the deadline cause, elapsed acquisition
time, item/player status, validated candidate identity, route, height and
automatic/voluntary intent before disposal for acquisition failures. After
exposure, missing prepared state remains unknown rather than implying that
an overlap deadline expired. Diagnostics omit URLs, credentials and free-form
error descriptions. These observations do not repair acquisition.

Focused production-helper regressions are in
[`AutoDeclaredCostTests`](../../clients/apple/Tests/AutoDeclaredCostTests.swift):
`testAutoMethodLabelFollowsInstalledRemuxEncodeAndOriginalRoutes`,
`testAutoMethodLabelUsesInstalledHeightAndPreservesOverlay`,
`testAutoMethodLabelRejectsUnboundAndInvalidCandidateIdentities` and
`testAutoMethodLabelPreservesManualAndSubtitleBurnLabels`.

**Local validation, 2026-10-17.** iOS and tvOS `build-for-testing` succeed
with Xcode 27.1 and the retained DerivedData. The selected iOS run executes
18 `AutoDeclaredCostTests` and
`PreparedOfferOwnershipTests.testAnOfferThatRunsOutOfReadinessOwesExactlyOneReopen`.
Sixteen Auto cases and the ownership case pass. Two inherited Auto fixtures
incorrectly bind transfer samples to literal candidate `c` while the verdict
expects the installed canonical candidate ID. Correct only those fixtures;
their two reruns pass, as does
`PreparedReplacementCoordinatorTests.testAFailureAfterReadinessDoesNotStopTheAsking`
(the first command used a nonexistent class name for this case, so it had
not run). All 20 distinct cases have passing evidence. The four new label
tests pass on their first run; no full suite or physical campaign runs.

Commands run from the branch checkout:

```bash
# Build both platforms separately, using the retained DerivedData directory.
xcodebuild -project clients/apple/plurx.xcodeproj -scheme plurx-iOS \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' \
  -derivedDataPath <derived-data> CODE_SIGNING_ALLOWED=NO build-for-testing
xcodebuild -project clients/apple/plurx.xcodeproj -scheme plurx-tvOS \
  -destination 'generic/platform=tvOS Simulator' \
  -derivedDataPath <derived-data> CODE_SIGNING_ALLOWED=NO build-for-testing
# First selection: retain successful cases, then rerun only the two failures.
xcodebuild -project clients/apple/plurx.xcodeproj -scheme plurx-iOS \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' \
  -derivedDataPath <derived-data> CODE_SIGNING_ALLOWED=NO \
  -parallel-testing-enabled NO \
  -only-testing:plurx-iOSTests/AutoDeclaredCostTests \
  -only-testing:plurx-iOSTests/PreparedOfferOwnershipTests/testAnOfferThatRunsOutOfReadinessOwesExactlyOneReopen \
  test-without-building
python3 scripts/paperwork --base 622141077 --merge-target 622141077
```

Paperwork passes 14/14; the harness suspension returns 2 before argument
parsing or any launch, and shell syntax validation passes. Apple build 261
is claimed above the task base; no physical installation is made.

## 4. Remaining milestones and acceptance

1. **Preserve the incident and restore safe operation.** Archive evidence,
   identify any retained request/status traces, reproduce navigation with
   disposable state, and prove launch failure cannot mutate saved login.
   Acceptance: saved origin/instance/token remain identical across timeout,
   401, 403 and wrong-instance cases in both Release and DEBUG behavior.
2. **Measure successor acquisition.** Record monotonic request start/header/
   completion timings, response status and byte counts by successor session,
   candidate and object class, with query strings and credentials omitted.
   Correlate producer readiness, item status and the original deadline.
   Acceptance: one failed acquisition is explained by the request and item
   timeline, including the failure before 3 Mb/s.
3. **Correct the measured cause.** Keep incumbent ownership and deadlines
   unless evidence and review justify a contract change. Add the focused
   regression which fails before the correction. Acceptance: the repaired
   candidate passes the previously failed stages on an isolated app/account;
   then qualify the shipping path safely.
4. **Promote and install.** Run the required fast lane once on the integrated
   candidate, rerun only failures, satisfy the promotion gate and qualification
   receipt, then merge and install from main. Preserve prior manual, Chrome,
   Android TV and Safari macOS evidence within their recorded source scope.

Firefox needs a quiet browser host on the designated Mac or `lab4`; the contended `lab3`
run is invalid. Safari iOS awaits Web Inspector and Remote Automation.
Neither is a substitute for the Apple successor investigation.

## 5. Evidence and cleanup ownership

The private Mac evidence remains in `/tmp/cc-cq-20261017/run*/`; it includes logs, shaper JSON and
xcresults. Last handed-off harness commit `622141077` is present on Forgejo
as of 2026-10-17. No low-link implementation plan existed there before this
follow-up; `fable-rca` contains build logs and DerivedData only.

Keep the temporary campaign clone and `/tmp/plurx-physical-release` until
source and evidence are archived. Remove the owned `lab4` Firefox lab and
retire only the owned `lab3` ADB server after archiving its receipts. Revert
the explicitly enabled authentication-free automation mode. Cleanup is
pending. The authentication-free automation-mode revert was attempted with
noninteractive sudo; it refused because a password is required. The operator
must run `sudo automationmodetool disable-automationmode-without-authentication`
in an authenticated terminal. No device reauthentication, physical proxy launch, new acceptance
campaign, merge or main installation is claimed by this document.
