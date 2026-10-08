# Cinema remote status — build, review, and device evidence

**Status:** three foundation packets built and reviewed; integration in progress ·
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
| B02 web semantic router | PR [#865](http://192.168.4.7:3000/noirr/plurx/pulls/865), `f9026e8e70e2` | No blocker in foundation scope | 22 remote + 7 existing keyboard checks; pinned daemon compile |
| B03 Apple navigation | PR [#863](http://192.168.4.7:3000/noirr/plurx/pulls/863), `93f821c0f320` | No blocker in foundation scope | iOS/tvOS builds, 7 focused XCTest; physical walkthrough pending |
| B04 storage/server relay | Building on reviewed B01 dependency | Pending | Exact HTTP DTOs precede client integration |
| B05 web companion | Waiting B04 | Pending | Two-client browser evidence pending |
| B06 Apple receiver/companion | Building local playback/menu integration; HTTP waits for B04 | Pending | Final native compile and lifecycle evidence pending |
| B07 Android receiver/companion | Waiting B04 | Pending | APK compile + navigation/lifecycle evidence pending |
| B08 desktop CEC | Building on reviewed B02 dependency | Pending | Distribution boundary, host/extension checks and devices pending |
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
The aggregate effort-gate scheduling is being reconciled with the batching
coordinator: the repository instructions require that gate, while the current
workflow runs only on manual dispatch and conservatively enables every
platform. No gate pass is claimed and no repeated broad matrix was launched.
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
all-target daemon check and Clippy passed. Authenticated admission belongs to
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
