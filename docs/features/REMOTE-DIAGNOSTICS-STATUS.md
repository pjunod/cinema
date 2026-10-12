# Remote diagnostics status — ownership, implementation and release evidence

**Status:** planned; GPT-6.1 Sol build requested · **Updated:** 2026-10-12 ·
**Planning baseline:** `8d7bc723e`.

The [build contract](REMOTE-DIAGNOSTICS-IMPLEMENTATION.md) defines the first
release: web, Apple and Android, including both native TV handoffs and the
independent upload inbox. This file is the canonical execution ledger.
The [project backlog](PROJECT-BACKLOG.md) indexes the effort rather than
duplicating its evidence.

## 1. Ownership and source

| Field | Current value |
|---|---|
| User authorization | 2026-10-12: detailed build plan and a GPT-6.1 Sol session to build it |
| Implementation owner | GPT-6.1 Sol chat; launch reference added by the planning chat |
| Intended effort | `effort/remote-diagnostics` |
| Task branch convention | `codex/remote-diagnostics-bNN` |
| Planning source | `8d7bc723e47f326eb213dce7857dd4b36f261867` |
| Implementation workspace/base | Managed worktree `remote-diagnostics/plurx`; current main `1f02b7938a2f3924de6a3969483e38ffe3e8bd6f` (18 commits since planning); private absolute path in chat attachment |
| Compiler loop | Local Rust/cargo 1.97.1 verified via rustup toolchain bin; isolated warm target in temporary storage; daemon baseline passed |
| Plan review | No independent review claimed |
| Promotion/deployment | Not started |

Read current AGENTS and pipeline amendments. Isolate implementation from the
shared checkout and preserve unrelated `Claude outputs/` artifacts.
Planning changes comprise this file, the implementation plan and the two
index/backlog rows; the builder adopts them together in a normal commit.

## 2. Packet ledger

| Packet | Scope | State | Branch/PR | Verification and remaining evidence |
|---|---|---|---|---|
| B00 | Isolated workspace, compile baseline, plan adoption | Software verified; task gate pending | `codex/remote-diagnostics-b00` | Pinned Rust daemon check, iOS/tvOS builds, Android app/test APKs passed on current main |
| B01 | Schema/privacy fixtures and archive inspector | Not started | — | Cross-language and hostile-archive evidence owed |
| B02 | User-scoped server/worker snapshot collection | Not started | — | Ownership, peer loss and cleanup evidence owed |
| B03 | Support auth, grants, uploads and durable storage | Not started | — | Restart, quota, revocation and receipt evidence owed |
| B04 | TV handoff, mobile page, inbox and packaging | Not started | — | Claim race, physical approval and operator flow owed |
| B05 | Web/TV browser reporting | Not started | — | Offline/boot/browser/focus evidence owed |
| B06 | Apple mobile/TV reporting | Not started | — | Both builds, share and physical TV handoff owed |
| B07 | Android mobile/TV reporting | Not started | — | Release/build, share and physical TV handoff owed |
| B08 | Reproduction, connection checks, diagnostic adapters | Not started | — | Stop/expiry/platform-limit evidence owed |
| B09 | Composed security/resource/device qualification | Not started | — | Matrix and overhead measurements owed |
| B10 | Operations, exact-candidate promotion and rollout | Not started | — | Runbook, gates, receipt and deployment inputs owed |

Replace “Not started” with claimed/building/built/software verified/physical
verified/promoted as evidence warrants. “Built” never means deployed.
Every command receipt states source commit, command, result, platform and
artifact location. Keep failure evidence and distinguish pre-existing faults
from regressions. Record test absence as absence, not a passing check.

## 3. First-release acceptance matrix

| Surface | Implementation | Software evidence | Physical/real delivery | Remaining |
|---|---|---|---|---|
| Web desktop/mobile/all layouts | Missing | None | None | Entire report flow |
| Browser TV | Missing | None | None | Remote focus + phone handoff |
| iPhone/iPad | Missing | None | None | Capture/share/send/recovery |
| Apple TV | Missing | None | None | Direct send + phone on separate network |
| Android phone/tablet | Missing | None | None | Capture/share/send/recovery |
| Android/Google TV | Missing | None | None | Direct send + phone on separate network |
| Cinema snapshots / multiple workers | Missing | None | None | Own-user evidence + unavailable peer behavior |
| Support upload service | Missing | None | None | Durable/resumable bounded upload |
| Phone completion page | Missing | None | None | QR/manual claim, TV approval, submission |
| Operator inbox | Missing | None | None | Authenticated read/download/delete/retention |
| Privacy/resource/retention | Missing | None | None | Canaries, bounds, overhead and deletion |

## 4. Decisions and external inputs

The plan supplies build defaults so implementation can proceed. Production
support hostname/hosting, TLS, operator identity enrollment and real
publisher credentials remain deployment inputs, not permission to invent or
publish them. Physical devices and signed release artifacts are acceptance
inputs. Use isolated synthetic/local service fixtures until those exist.

No first-release scope has been deferred. Record proposed deviations here
with the affected contract, reason, replacement evidence and whether a user
decision is actually necessary. Routine module/file choices belong to the
builder; weakening authorization or dropping a required client does not.

## 5. Evidence and handoff history

- 2026-10-12: planning chat inspected existing client-log, playback telemetry,
  cluster support export, native reporting hooks and current workflow.
  Created the implementation contract and this empty evidence ledger.
  No production code, Rust compilation or device acceptance is claimed.


## 5. Development receipts — source and limits

- 2026-10-12 · base `1f02b7938`: planning patch applied with three-way
  preservation of newer index/backlog entries. Shared checkout untouched.
- Rust toolchain: `rustc 1.97.1 (8bab26f4f 2026-07-14)` and
  `cargo 1.97.1 (c980f4866 2026-06-30)` verified locally. Shell PATH must
  select the rustup toolchain bin ahead of Homebrew Rust 1.98.0.
- Apple: `make apple-build`, Xcode 27.0 (27A266a), iOS and tvOS simulator
  compilation succeeded. No XCTest or hardware acceptance claimed.
- Android: local JBR 21.0.11 and installed SDK,
  `clients/android/gradlew --no-daemon -p clients/android assembleDebug
  assembleDebugAndroidTest`; passed (72 tasks, 1 minute 8 seconds). The pinned CI image uses JDK
  25; local compilation is development evidence, not release qualification.
- Rust: `cargo check -p plurxd --locked --all-targets`, passed in 3 minutes 17 seconds.
  All builds use the isolated managed worktree. Private command logs are in
  temporary `cinema-remote-diagnostics-*-baseline.log` files on the owner host.
