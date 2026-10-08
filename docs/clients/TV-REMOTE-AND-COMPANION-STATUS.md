# Cinema remote status — build, review, and device evidence

**Status:** build contracts written; production implementation not started ·
**Updated:** 2026-10-07 · **Integration:** `effort/cinema-remotes`.

[Implementation packets](TV-REMOTE-AND-COMPANION-IMPLEMENTATION.md) define
ownership. [Protocol](TV-REMOTE-PROTOCOL.md) defines interfaces. The parent
session manages Sol 6.1 builders and personally reviews their changes.
Main integration is handed to `01a11907-f720-71b1-8c51-89902b919e6f`.

## Packet ledger

| Packet | Build | Parent review | Evidence |
|---|---|---|---|
| Build docs | Written | In progress | No unit tests: documentation only |
| B01 wire/receiver guard | Ready | Pending | No implementation evidence |
| B02 web semantic router | Ready | Pending | No implementation evidence |
| B03 Apple navigation | Ready | Pending | Device walkthrough pending |
| B04 storage/server relay | Waiting B01 | Pending | Pinned baseline compile passed |
| B05 web companion | Waiting B02/B04 | Pending | Two-client browser evidence pending |
| B06 Apple receiver/companion | Waiting B03/B04 | Pending | iOS/tvOS compile + device evidence pending |
| B07 Android receiver/companion | Waiting B04 | Pending | APK compile + device evidence pending |
| B08 desktop CEC | Waiting B02 | Pending | Distribution choice and devices pending |
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
