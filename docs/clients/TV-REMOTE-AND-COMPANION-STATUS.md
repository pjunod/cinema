# Cinema remote status — build, review, and device evidence

**Status:** web, Apple, Android and server foundations reviewed; couch completion and invitations in progress ·
**Updated:** 2026-10-08 · **Integration:** `effort/cinema-remotes`.

[Implementation packets](TV-REMOTE-AND-COMPANION-IMPLEMENTATION.md) define
ownership. [Protocol](TV-REMOTE-PROTOCOL.md) defines interfaces. The parent
session manages Sol 6.1 builders and personally reviews their changes.
Main integration is handed to `01a11907-f720-71b1-8c51-89902b919e6f`.

## Packet ledger

| Packet | Build | Parent review | Evidence |
|---|---|---|---|
| Build docs | PR [#862](http://forge.lan:3000/noirr/plurx/pulls/862) | Ready; handed to batch coordinator | Documentation only; no unit suite |
| B01 wire/receiver guard | PR [#864](http://forge.lan:3000/noirr/plurx/pulls/864), `3fa57aa1ba80` | No blocker in foundation scope | 11 focused regressions; pinned core compile/Clippy |
| B02 web semantic router | PR [#865](http://forge.lan:3000/noirr/plurx/pulls/865), `502c98761f97` | No blocker in foundation scope | 22 remote + 7 existing keyboard checks; pinned daemon compile |
| B03 Apple navigation | PR [#863](http://forge.lan:3000/noirr/plurx/pulls/863), `93f821c0f320` | No blocker in foundation scope | iOS/tvOS builds, 7 focused XCTest; physical walkthrough pending |
| B04 storage/server relay | PR [#878](http://forge.lan:3000/noirr/plurx/pulls/878), `5d78eee7ace4` | Released to coordinator after exact-head review and static history pass | 16 focused checks; actual two-node signed HTTP and separate three-voter storage evidence |
| B05 web companion | PR [#883](http://forge.lan:3000/noirr/plurx/pulls/883), `f4d8ac12b502` | Released in parent review 107 | Actual browser/server pairing, explicit acquire, playback ACK, owner replacement, offline Pause/Stop, revocation and compact phone layout pass; physical CEC remains open |
| B06 Apple receiver/companion | PR [#875](http://forge.lan:3000/noirr/plurx/pulls/875), `7e66c85c6f98`, composed through [#879](http://forge.lan:3000/noirr/plurx/pulls/879) | Reviewed and integrated into coordinator candidate | iOS/tvOS builds and 2 focused follow-up XCTest; earlier 19 cases passed before the narrow correction |
| B07 Android receiver/companion | PR [#881](http://forge.lan:3000/noirr/plurx/pulls/881), `c3e2c6ab0f0e` | Released in parent review 106 | Both APKs compile; grouped 21 cases and final 2 correction cases pass; representative Router smoke passes, production native instrumentation unexecuted |
| B08 desktop CEC | PR [#872](http://forge.lan:3000/noirr/plurx/pulls/872), `08258f51cbb4` | No blocker in standalone software packet | 16 Python + 12 Node checks and production-popup Chromium smoke with native port mocked; hardware pending |
| B09 invitations | Storage/authority/admission slice built; home routes, broker and native adapters continue | Design reviewed; early restore/transaction findings corrected | Focused SQLite and actual three-voter contracts pass; provider delivery and resident-service eligibility remain open |
| B10 integration handoff | Setup/recovery guide in [#877](http://forge.lan:3000/noirr/plurx/pulls/877); complete feature handoff waits on remaining packets | Pending | Ready packets handed off individually; whole feature not yet complete |

## Baseline and review record

Source: `8e242787c5112d6ba2bd66b2d30c4dd0a0bbc2a4`.
Pinned local toolchain is installed: `rustc 1.97.1 (8bab26f4f 2026-07-14)`.
Bare Homebrew `rustc` is 1.98.0 and is not equivalent evidence.
Use `rustup run 1.97.1` for compile, lint and normal commit hooks.
Baseline `rustup run 1.97.1 cargo check -p plurxd` passed in 1m 38s.
This establishes the compiler loop; it is not implementation evidence.

The initial proposal PR is #854. Its earlier CI timeout/missing receipt
journal is a documentation-lane coordination issue, not code evidence. The
new effort is based on main containing the proposal. Four adversarial design
findings remain evidence-open, with corrections specified in implementation
§9. No root primary-worktree changes from unrelated sessions are included.

## Reviewed foundations — exact scope and remaining work

All three task heads are based on `f6de43cd9df10e20816f49ab83c5542e73f194ab`.
They retain normal commit hooks and have not merged into the effort. Parent
reviews are recorded on the exact heads: [B01 review](http://forge.lan:3000/noirr/plurx/pulls/864#issuecomment-9159),
[B02 review](http://forge.lan:3000/noirr/plurx/pulls/865#issuecomment-9161),
[B03 review](http://forge.lan:3000/noirr/plurx/pulls/863#issuecomment-9157).
Combined draft PR [#869](http://forge.lan:3000/noirr/plurx/pulls/869) preserves
all three task histories. The initial reviewed handoff was
`1041d72407d937efe265f7263926cacf332e6b22` (tree
`5cea422aacfcdb78d9385fb0451947263b1ac270`); the batching coordinator now
owns subsequent receipt-recovery/documentation composition and freezes each
current candidate during its combined Effort development gate. No gate
pass is claimed and no repeated broad matrix was launched. The first gate
stopped in history preflight: B03 lacked a required client-fix anchor. The
combined candidate adds that metadata; `make history-check` passes, and the
coordinator dispatched one corrected run against the frozen head. That run
stopped before units/compile because the first failed preflight left an
incomplete receipt. The coordinator integrated receipt recovery PR #873 and progress docs #871,
then dispatched one corrected gate. Failed attempts remain recorded; no
passes were invented.

Gate 4397 stopped in history validation before units: the recovery merge had
class-qualified Python trailers, and four local foundation integrations
lacked recognized PR subjects. The manager recorded exact tree, parent and
title identities with the existing landing metadata, preserving original
trailer checks, and two precise immutable trailer errata. Reviewed B08 is
included in corrected composition PR
[#876](http://forge.lan:3000/noirr/plurx/pulls/876), head
`29f8603bf02284eb80c7fa494effd53ae5fb5a10`, tree
`f141a366810603f9c6bf395c881c1db86201b388`. Its normal hook and
committed-head history audit pass: 3356 corrective commits, 482 client
anchors, 316 recognized landings and zero awaiting landing. The coordinator
owns the next combined gate. B06 subsequently resolved its pairing-lifetime
hold and was released separately for composition.

The foundations' combined Rust 1.97.1 daemon all-target check and complete web
static target pass.
Workspace Clippy and iOS/tvOS builds passed before a web-only generated
manifest/type-annotation correction; the relevant native sources did not
change. The follow-up [B02 review](http://forge.lan:3000/noirr/plurx/pulls/865#issuecomment-9173)
records that correction and the unchanged TypeScript diagnostic baseline.
Dependent task branches may merge these reviewed commits locally while
integration remains pending; this does not merge the shared effort.

B01 fixes bounded wire validation, grant/target/control binding, local-clock
expiry, replay protection, and acknowledgement after the synchronous effect.
It is an inert library; it does not authenticate an HTTP request or create a
receiver connection. Its focused command is
`rustup run 1.97.1 cargo test -p plurx-core --features hiqlite-store --lib remote_control::tests --locked`.
Feature-on all-target compile, denied-warning Clippy and formatting passed.

B02 registers safe browsing and playback actions, fences physical focus and
route changes, and preserves physical scrubs while cancelling only gestures
owned by the network controller. VOD and Live TV continue using their existing
playback owners. Live TV pauses synchronously on Stop and preserves its
fullscreen-before-detach ordering. Its focused command is
`node --test tests/web/cinema-remote.test.js`; the existing held-key subset,
player-input contract and web asset/layout checks also passed. Rust 1.97.1
all-target daemon check and Clippy passed. The combined static check found a missing generated jsconfig row and erased
type annotations; B02 corrected them in `502c98761f97` without changing
runtime behavior or increasing the TypeScript baseline. Authenticated admission belongs to
B05. Native selectors, reader/photo, DVR/guide and unregistered overlays stay
blocked; DOM fixtures are not a browser or CEC hardware walkthrough.

B03 owns navigation paths, bounded semantic registrations, actual confirmed
native focus, and accessible app-owned choice controls. Initial tab choice is
latched; identity reset clears routes and closures; suspended receivers clear
pending work. `make apple-build` compiled iOS and tvOS with Xcode 27.0
(27A266a); seven `RemoteNavigationTests` passed on an iOS 26.5 simulator.
Player/Live TV menus and receiver/network integration belong to B06. Expanded
library groups, preplay and other unregistered controls are not evidence of
complete couch navigation. Physical tvOS focus and lazy-grid acceptance stay
open.

## Reviewed desktop packet — source and browser evidence

B08 PR [#872](http://forge.lan:3000/noirr/plurx/pulls/872) is frozen at
`08258f51cbb488e4773a7985b12884315bc82157`, tree
`d4194e8300be9c5f21392f87c24878b9468aaa3c`. The
[parent review](http://forge.lan:3000/noirr/plurx/pulls/872#issuecomment-9195)
records no blocker in the standalone software packet. It does not claim
physical CEC or complete feature acceptance.

The source-only per-user host supports Linux kernel CEC and an original
adapter for separately installed libCEC 8.1.6. Its Chromium extension binds
one chosen origin/tab/document/window/account, uses independent local credit
deadlines, and checks actual document focus immediately before effects.
Asynchronous setup cannot replace a newer binding or follow a navigated
replacement document. Installer ownership checks preserve foreign files
and registrations.

Sixteen Python and twelve Node focused checks passed. Real Chromium loaded
the production popup, enabled the saved choice, clicked Bind, returned focus
to Cinema and delivered semantic Right through the existing app router;
reload removed the binding. Only the native port was mocked, and the fixture
origin permission was pregranted. This proves the browser path, not a native
registration, interactive permission prompt or CEC device. A real isolated
native-host subprocess also handled framed input, missing libCEC and EOF.
Normal hooks and `make history-check` passed at the exact final head.

B05 still owns the Cinema device-local Developer card and extension
integration. Physical Linux/Pi/Mac behavior, libCEC interoperability,
Windows launcher execution, native installation and held-key/offline
acceptance remain open. Original code remains Apache-2.0; no libCEC code or
binary is bundled, and combined distribution still needs a compatible
license decision.

## Reviewed Apple packet — lifecycle and control recovery

B06 PR [#875](http://forge.lan:3000/noirr/plurx/pulls/875) is frozen at
`7e66c85c6f98cf59d393d6b9b23f8ce4a0d1da84`, tree
`2b5e81fd7728beb8dadb64a3934af9cb70dc62df`.
The [parent review](http://forge.lan:3000/noirr/plurx/pulls/875#issuecomment-9209)
was followed by a [pairing-lifetime finding](http://forge.lan:3000/noirr/plurx/pulls/875#issuecomment-9213): a late approved result could reopen a closed or replaced target. The correction now binds each pairing operation to its generation, receiver and target, retires it on close/switch and saves an approved proof before selection. The [release review](http://forge.lan:3000/noirr/plurx/pulls/875#issuecomment-9218) removes that hold. A second state-null finding was [withdrawn](http://forge.lan:3000/noirr/plurx/pulls/875#issuecomment-9214) after verifying that the API explicitly uses null to mean unchanged. Generic iOS/tvOS simulator
builds and two focused pairing/null-timeout checks pass after the correction; nineteen navigation/receiver checks passed on the preceding reviewed source. Final history audit and all normal commit hooks pass on the new head.

The implementation includes scoped Keychain proofs, strict nested command
validation, local pairing approval, grant revoke/reset, companion controls,
and dispatch into the existing playback owners. Review corrections preserve
physical scrubs, suspend immediately on inactive scenes, require explicit
control reacquisition after close or renewal failure, condition sequence
recovery on the observed lease epoch, and refresh guide programme focus
identities when a time window changes. A Select still requires actual realized
focus; a recovered state response alone cannot authorize controller sends.

Live server integration, physical tvOS focus, lazy-grid boundaries, VoiceOver
and mixed Siri Remote/phone input remain acceptance work. Unowned native
presentations and expanded library groups remain restricted. The separate
couch-navigation follow-up also fences delayed TV pairing-code and approval
responses against a closed or replaced pairing surface; the released B06
head remains frozen. Background invitations belong to B09.

## Current composition and runtime acceptance

The coordinator merged reviewed desktop/metadata PR #876 at
`e457e23afbe8f31ac48f56beb9f17a860ee8ec5d`, then docs PR #877 at
`04b6e302eddeb9c5e853feba433c4e6e7d63f66d`. Forgejo could not directly merge
Apple PR #875, so the manager composed its frozen head on that exact candidate.
Only `tests/client-fixes.toml` conflicted: both complete parent catalogs were
preserved, with one exactly identical B03 row deduplicated. Apple source and
tests remained identical to reviewed B06. No native tests were repeated for
this metadata composition.

Composition PR [#879](http://forge.lan:3000/noirr/plurx/pulls/879), head
`859383df8f098993f978e54aebf9d34cce9ac964`, tree
`d7dff0ede1d0f37fd91565e4f3b47ad5f991a788`, passed normal hooks and final
history validation: 3361 corrective commits, 487 client anchors, 319 recognized
landings and zero awaiting landing. The first history invocation could not
write its receipt under the sandbox; the corrected-permission run passed.
All fourteen B06 regression fields were preserved. The coordinator reports
landing it into its candidate at `5458a0323bd98732c61a2cb1af21c4cfb669598f`.
That is candidate integration, not promotion into main. Mobile version counters
and combined gates remain coordinator-owned.

B04 PR #878 is released in [parent review 105](http://forge.lan:3000/noirr/plurx/pulls/878#issuecomment-9252).
Its final static history audit passes with 3350 corrective commits, 477 client
anchors, 309 recognized landings and three source changes awaiting their
intended landing. All twenty-two regression fields resolve. The coordinator
received the exact frozen source and evidence and merged it into the candidate
at `79a58a03378e0cf15f9693cd1b4eb56ab6dc4d13`, preserving all twenty-two
fields. No independent gate was run.

B04's isolated live fixture runs the actual Router and SQLite with synthetic
accounts and generated H264/AAC media. Its production API source hash matches
committed B04. B05's browser run exercised pairing, approval, explicit acquire,
playback acknowledgements, restricted physical dialogs, revoked proofs and
local pause/stop during an API outage, plus owner replacement followed by a
fresh explicit Use action. The final 390×844 phone viewport exposes directional
controls and Play/Pause/Stop without scrolling; secondary controls collapse. It caught and corrected a native fetch
binding error and an empty focused label that needed to be null. Separate
same-profile tabs transferred the exclusive receiver lock after actual focus
changes; Playwright's built-in focus emulation was disabled for that check.
The phone/browser counterpart uses a separate simulated device. These are
browser/runtime checks, not physical TV/CEC or native-device evidence.

Android representative item/live state and pairing/control DTOs passed against
that same server fixture. This is a schema smoke, not proof of the actual
Android OkHttp/UI path on a device. Ordinary navigation gaps are now explicit
follow-up packets in implementation §11. A connected Google TV Streamer was
found through read-only device inventory; no packet was installed or launched
on it for physical acceptance.

## Released web and Android composition

The parent released Android in [review 106](http://forge.lan:3000/noirr/plurx/pulls/881#issuecomment-9265)
and web in [review 107](http://forge.lan:3000/noirr/plurx/pulls/883#issuecomment-9273),
then handed both frozen heads to the batch coordinator. Android's final review
corrected claim-start monotonic pairing expiry and preserved an exact command
ACK across a later queued response. Web's final correction enabled trusted
assistive button activation without duplicating pointer/keyboard gestures.
No physical screen-reader qualification is inferred from that regression.

Forgejo refused direct candidate merges for both PRs. On the coordinator's
request, the manager composed them from candidate
`207447f8960d9d50048bd73459093a898582c3e3`. Merge
`ad74bac1932fda1e4ad7e977357cd9f5988d772d` preserves Android's thirteen
regression fields; merge `2dc75c392ded3030e28090b43dcca1c464db854e` preserves
web's forty-seven. Both conflicts were confined to `tests/client-fixes.toml`;
all parent rows survive unchanged as 501 distinct entries. Android, Apple,
web and desktop CEC source/tests compare identically to their reviewed heads.
Normal hooks passed (2m 10s and 32.21s); no native tests were repeated for
catalog-only integration. This is local composition, not a main landing or a
combined gate pass. Final history and candidate handoff remain coordinator
inputs.

Android's production OkHttp/Keystore/presence instrumentation compiles but
was not executed: isolated fresh-data TV and phone emulator attempts stayed
offline and were stopped. The connected physical Streamer remains untouched.
The temporary B04 live Router fixture was stopped gracefully after consumers
finished; its browser and synthetic DTO receipts remain evidence of those
specific checks.

The web couch builder starts from composed source `2dc75c392ded`; Apple couch
starts from current candidate `207447f8960d`. Frozen foundation branches stay
unchanged. Ordinary browsing gaps and locally operated CEC pairing remain
software work. The invitation builder has a fixed native API contract and a
verified storage slice; route/provider implementation is still in progress.
Its background resident polling must use no-touch authority, just like worker
admission, so polling cannot extend a dormant phone login's lifetime.

## Rust cache ownership — one worktree per target

During B04's normal hook, the shared Rust target exposed an incompatible
workspace artifact from another packet. All owners stopped using that target.
The manager copied its warm dependency cache into separate worktree targets
under the Cargo build-directory lock. Each builder removes copied workspace
package artifacts before using its own cache, verifies Rust 1.97.1 and repeats
checks against its exact source. Third-party dependencies remain cached.

Use `CARGO_TARGET_DIR="$PWD/target"` from each packet's repository root for
Cargo and normal hooks. Do not point concurrent packet worktrees at the
shared effort target. Apple and Android native build products already belong
to their individual worktrees. Final combined qualification remains owned by
the batching coordinator.

## Android build loop — source compilation is available

Before B07, the unchanged Android app and test APK compiled locally with
Gradle 9.7.1, installed SDK 37.0 and JetBrains Runtime 21.0.11. This is local
compiler evidence, not the repository's JDK 25 Docker image or device evidence.
Docker was unavailable; no CI job was used to discover compiler errors.
The direct command was:

```bash
cd clients/android
ANDROID_HOME=~/Library/Android/sdk \
JAVA_HOME=~/Library/Java/JavaVirtualMachines/jbr-21.0.11/Contents/Home \
./gradlew --no-daemon assembleDebug assembleDebugAndroidTest
```

No unit suite or instrumented test was executed by that command. B07 must
repeat the affected compile after implementing the receiver and companion.

## Physical acceptance — never infer from compilation

| Surface | Required facts | Evidence status |
|---|---|---|
| Linux/Pi | Board, OS/kernel, adapter, browser, TV/AVR, CEC offline controls | Hardware not yet established |
| Mac | Mac/OS, USB-CEC model, browser, TV/AVR, CEC offline controls | Hardware not yet established |
| Windows | Windows build, USB adapter, browser, install/uninstall | Hardware not yet established |
| Apple TV | tvOS/device/build, menus/grid/search/modal/mixed-input walkthrough | Hardware not yet established |
| Android TV | Device/OS/build, D-pad + companion + foreground lifecycle | Google TV Streamer found; no install or walkthrough performed |
| iPhone/iPad | Suspended app/APNs consent, delivery and stale tap | Provider/device evidence absent |
| Android phone/tablet | FGS eligibility, Stop, Doze/OEM, permission denial | Provider/device evidence absent |

No platform is marked supported by this ledger yet. The user was asked for
available hardware while software and documentation work continues.
