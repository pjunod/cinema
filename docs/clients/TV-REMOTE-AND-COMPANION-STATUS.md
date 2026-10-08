# Cinema remote status — build, review, and device evidence

**Status:** foundations and desktop reviewed; Apple follow-up in progress; end-to-end integration in progress ·
**Updated:** 2026-10-07 · **Integration:** `effort/cinema-remotes`.

[Implementation packets](TV-REMOTE-AND-COMPANION-IMPLEMENTATION.md) define
ownership. [Protocol](TV-REMOTE-PROTOCOL.md) defines interfaces. The parent
session manages Sol 6.1 builders and personally reviews their changes.
Main integration is handed to `01a11907-f720-71b1-8c51-89902b919e6f`.

## Packet ledger

| Packet | Build | Parent review | Evidence |
|---|---|---|---|
| Build docs | PR [#862](http://192.168.4.7:3000/noirr/plurx/pulls/862) | Ready; handed to batch coordinator | Documentation only; no unit suite |
| B01 wire/receiver guard | PR [#864](http://192.168.4.7:3000/noirr/plurx/pulls/864), `3fa57aa1ba80` | No blocker in foundation scope | 11 focused regressions; pinned core compile/Clippy |
| B02 web semantic router | PR [#865](http://192.168.4.7:3000/noirr/plurx/pulls/865), `502c98761f97` | No blocker in foundation scope | 22 remote + 7 existing keyboard checks; pinned daemon compile |
| B03 Apple navigation | PR [#863](http://192.168.4.7:3000/noirr/plurx/pulls/863), `93f821c0f320` | No blocker in foundation scope | iOS/tvOS builds, 7 focused XCTest; physical walkthrough pending |
| B04 storage/server relay | Building on reviewed B01 dependency | Pending | Exact HTTP DTOs precede client integration |
| B05 web companion | Building against stable B04 DTOs and reviewed B02/B08 | Pending | Two-client browser evidence pending |
| B06 Apple receiver/companion | PR [#875](http://192.168.4.7:3000/noirr/plurx/pulls/875), `8a6246043194` | Integration held for pairing-close lifetime correction | iOS/tvOS builds and 19 focused XCTest at reviewed head; correction validation pending |
| B07 Android receiver/companion | Building against stable B04 DTOs | Review in progress | Incremental Kotlin compile and focused wire checks pass; full navigation/lifecycle evidence pending |
| B08 desktop CEC | PR [#872](http://192.168.4.7:3000/noirr/plurx/pulls/872), `08258f51cbb4` | No blocker in standalone software packet | 16 Python + 12 Node checks and production-popup Chromium smoke with native port mocked; hardware pending |
| B09 invitations | Waiting B04/B06/B07 | Pending | Provider and service eligibility pending |
| B10 integration handoff | Waiting all | Pending | No ready implementation handoff yet |

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
reviews are recorded on the exact heads: [B01 review](http://192.168.4.7:3000/noirr/plurx/pulls/864#issuecomment-9159),
[B02 review](http://192.168.4.7:3000/noirr/plurx/pulls/865#issuecomment-9161),
[B03 review](http://192.168.4.7:3000/noirr/plurx/pulls/863#issuecomment-9157).
Combined draft PR [#869](http://192.168.4.7:3000/noirr/plurx/pulls/869) preserves
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
incomplete receipt. The coordinator owns supported receipt recovery; the
coordinator has now integrated receipt recovery PR #873 and progress docs #871 into the candidate and dispatched one corrected gate. Failed attempts remain recorded; no passes were invented. Gate4397 then stopped in history validation before units: the recovery merge had class-qualified Python trailers, and four local foundation integrations lacked recognized PR subjects. The manager records exact tree/parent/title identities with the existing landing metadata, preserving original trailer checks, and two precise immutable trailer errata. Reviewed B08 is included in the corrected composition; B06 remains held for its pairing-lifetime correction. A new workflow must wait for local history validation of the committed candidate. The final combined
Rust 1.97.1 daemon all-target check and complete web static target pass.
Workspace Clippy and iOS/tvOS builds passed before a web-only generated
manifest/type-annotation correction; the relevant native sources did not
change. The follow-up [B02 review](http://192.168.4.7:3000/noirr/plurx/pulls/865#issuecomment-9173)
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

B08 PR [#872](http://192.168.4.7:3000/noirr/plurx/pulls/872) is frozen at
`08258f51cbb488e4773a7985b12884315bc82157`, tree
`d4194e8300be9c5f21392f87c24878b9468aaa3c`. The
[parent review](http://192.168.4.7:3000/noirr/plurx/pulls/872#issuecomment-9195)
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

B06 PR [#875](http://192.168.4.7:3000/noirr/plurx/pulls/875) is frozen at
`8a62460431943e0c022419ff65d4ff02663098dc`, tree
`35955b5117234ee606e6dd4ffa50287298dccc89`.
The [parent review](http://192.168.4.7:3000/noirr/plurx/pulls/875#issuecomment-9209)
was followed by a [pairing-lifetime finding](http://192.168.4.7:3000/noirr/plurx/pulls/875#issuecomment-9213): a late approved result could reopen a closed or replaced target. The coordinator is holding this packet for a narrow correction and repeat review. A second state-null finding was [withdrawn](http://192.168.4.7:3000/noirr/plurx/pulls/875#issuecomment-9214) after verifying that the API explicitly uses null to mean unchanged. Generic iOS/tvOS simulator
builds and nineteen focused XCTest checks pass; final history audit and all
normal commit hooks pass.

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
presentations and expanded library groups remain restricted. Background
invitations belong to B09.

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
ANDROID_HOME=/Users/pjunod/Library/Android/sdk \
JAVA_HOME=/Users/pjunod/Library/Java/JavaVirtualMachines/jbr-21.0.11/Contents/Home \
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
| Android TV | Device/OS/build, D-pad + companion + foreground lifecycle | Hardware not yet established |
| iPhone/iPad | Suspended app/APNs consent, delivery and stale tap | Provider/device evidence absent |
| Android phone/tablet | FGS eligibility, Stop, Doze/OEM, permission denial | Provider/device evidence absent |

No platform is marked supported by this ledger yet. The user was asked for
available hardware while software and documentation work continues.
