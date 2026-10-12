# Remote diagnostics status — ownership, implementation and release evidence

**Status:** building; GPT-6.1 Sol implementation active · **Updated:** 2026-10-12 ·
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
| Intended effort | Superseded by `codex/remote-diagnostics`, targeting `main` |
| Task branch convention | Normal packet commits on `codex/remote-diagnostics` |
| Planning source | `8d7bc723e47f326eb213dce7857dd4b36f261867` |
| Implementation workspace/base | Independent clone in temporary owner storage; base includes current main `15d78f208`; B00 committed source `3f92afc9d`. Former managed worktree archived after verified preservation. |
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
| B00 | Isolated workspace, compile baseline, plan adoption | Software verified; old gate superseded | Batched PR #1037 | Pinned Rust daemon check, iOS/tvOS builds, Android app/test APKs passed on current main |
| B01 | Schema/privacy fixtures and archive inspector | Built; compiler verified | Batched PR #1037 | Owned-clone workspace Clippy, iOS/tvOS and Android compilation passed; historical 35-case four-language fixtures passed before ruling; three extra timestamp fixtures await final checks |
| B02 | User-scoped server/worker snapshot collection | Built; compiler verified | Batched PR #1037 | Pinned daemon Clippy passed; positive attribution, accepted route receipts, account-generation fencing, bounded jobs and signed peers implemented; real HTTP acceptance pending |
| B03 | Support auth, grants, uploads and durable storage | Built; compiler verified | Batched PR #1037 | Independent encrypted SQLite publication, separate capture authority, provisioning/revocation, resume/refresh, quotas and external deletion/revocation fences implemented; pinned final workspace Clippy and dependency policy passed; runtime acceptance pending |
| B04 | TV handoff, mobile page, inbox and packaging | Built; compiler verified | Batched PR #1037 | Phone claim/approval protocol, responsive completion page, named passkey inbox and TLS/service packaging written; browser races/passkeys/physical acceptance pending |
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

## 6. Maintainer workflow ruling — 2026-10-12

Paul explicitly directed: “Use the latest workflow; replace those earlier
requirements.” This replaces the prior focused-test-per-push, task effort
gates and full promotion qualification for this project. No clarification
is pending. Keep normal hooks, pinned compilation and formatting before
push. Batch normal commits into one main-bound PR. Obtain one adversarial
agent review only when that PR is ready, address its findings, then run the
fast lane. Every required check needs one applicable pass. Rerun failures
and checks whose coverage a later code change actually invalidates; record
that impact. Another process owns full-unit failure batches.

No feature scope is reduced. Physical-device and deployment evidence remain
separate, and the software cannot claim either without the actual receipt.

The user requires an independent clone. Source, status and build outputs now
belong there; the shared checkout is neither a workspace nor a build source.
B00 commits and all nine new B01 source files were verified preserved before
archiving this chat's managed worktree. No credentials were copied. The
provided deploy SSH key was refused by Forgejo Git; HTTPS cloning used the
provided token through an ephemeral askpass helper, without storing it in
Git configuration or a remote URL.

Old draft #1035 is superseded by batched main-bound PR #1037 and closed. Its body was
updated with owner/chat, source, checks, workflow change and next step.

- Active PR: [#1037](http://forge.lan:3000/noirr/plurx/pulls/1037), owned by
  this chat/model, draft while implementation continues. Final review and
  fast lane have not started. The app attachment tool refuses Forgejo URLs;
  the canonical link is retained here and in the local status page.

## 7. B01 — strict report format and archive inspection

The v1 manifest is closed: unknown keys, duplicate keys, nulls, noncanonical
integer/counter/UUID encodings, invalid coverage reasons and unsupported
versions are refused. Counters are unsigned 64-bit decimal strings;
measurements are bounded JSON integers. Structured event facts have a closed
enum/measurement vocabulary. User-authored description is the only free text.
Archive projections contain normalized typed events, never general log dumps.

The archive inspector checks the central directory count/names/extent before
allocating ZIP metadata, rejects symlinks/encryption/traversal/nested or unknown
entries, enforces actual and declared sizes, then verifies every inventory
hash and the generated readable summary. It does not extract or execute data.
The manifest is omitted from its own checksum inventory. The shared fixture
has 38 cases; 35 were exercised across all languages before the workflow
ruling. Three additional malformed timestamp cases are retained for the final
checks. Historical archive tests cover tampering, traversal, symlinks,
duplicates and a compressed oversize entry.

Owned-clone development compiler receipts (no unit suites executed):

- `cargo clippy --workspace --all-targets -- -D warnings`, pinned Rust
  1.97.1: passed in 3 minutes 42 seconds.
- `make apple-build`: iOS and tvOS simulator compilation passed, Apple
  build 232. No XCTest/device pass is claimed for this owned-clone candidate.
- Local Android `assembleDebug`: passed in 1 minute 33 seconds,
  versionCode 166. CI's pinned JDK 25 and signed release remain separate.

Logs and historical source-only evidence are retained in the owned clone's
ignored evidence directory. Final source SHA is recorded by the packet commit
and PR body. Future changes invalidate only affected compiler/check coverage.

## 8. B02 — attributed snapshot owner

The daemon observes closed typed playback facts through the existing telemetry
owner, including when telemetry persistence is disabled. Trusted route-cache
publication records positive local-user ownership and participating workers.
Client-provided session labels cannot create receipts or select peers. The
projection retains bounded recent events, with priority for terminal/action
facts, and reports only the requesting account’s known losses.

Snapshot jobs are account- and authentication-generation-bound, limited to two
per user, 32 per node and 2 MiB each. The owner expires/erases them after fifteen
minutes without requiring polling. Cancellation drops bounded peer futures;
collection has a five-second total deadline and two-second peer deadlines.
Peer requests and responses use the existing exact signed transport and
revalidate account generation. No general-log fallback or playback control
operation is introduced. Administrator readiness is a typed availability fact.

Pinned Rust 1.97.1 `cargo clippy -p plurxd --all-targets -- -D warnings` passed
in two minutes after compiler/lint repairs. A separate clean corrected log is
retained in owner evidence storage; earlier failures are also preserved. No
unit suite or real HTTP multi-node pass is claimed for this packet.

## 9. B03–B04 — independent support boundary

The support binary has distinct loopback ingress/operator listeners and fixed
HTTPS origins. Its SQLite publication boundary retains encrypted chunks,
archive, manifest, descriptions and recoverable handoff offers. Upload and
creator management authority are separately bound to one capture; a publisher
credential cannot recover management access to an already reserved capture.
An approved phone receives its own short-lived authority and can finish after
the TV disappears. No preview or report identity is released before approval.

Cinema’s dedicated support transport reads only the protected publisher file,
refuses redirects, bounds replies and provisions/rotates/revokes user-scoped
installation enrollment. Cached upload-only authority has a disclosed maximum
24-hour revocation delay. Account authentication is required for direct grants.
No Cinema token or cookie is forwarded to support.

Named operators use upstream WebAuthn passkey ceremonies on the separate
operator origin. The browser inbox provides exact reference search and safe
field filters, coverage/timeline text, verified download, labels, retention and
deletion. Passkeys and report content are encrypted. Sessions are fifteen
minutes, HttpOnly/Secure/SameSite Strict, with separate CSRF proofs.

Safe resource ceilings are configurable downward. Archive inspections and
requests have independent admission, body/decompression/row caps and expiry.
External fsynced deletion/revocation journals replay against restored SQLite
snapshots; missing or mismatched key/generation refuses startup. Namespace
renewal and current external fencing records are required if those records
cannot be recovered; in-place encryption/key migration is not implemented.

Pinned workspace compiler check passed in 2 minutes 27 seconds. Corrected
workspace Clippy passed; final quota/trust/packaging source passed again in 1.23 seconds
before commit. Dependency bans and licenses passed with only the named
WebAuthn MPL/OpenSSL exceptions. Both support scripts passed Node syntax checks.
Declared restart/receipt/privacy, claim/approval/restore and real HTTP boundary
regressions have not been executed under the latest workflow. No physical,
TLS deployment or real passkey/browser acceptance is claimed.
