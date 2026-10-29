# Remote diagnostics status — ownership, implementation and release evidence

**Status:** open — archive software is built and qualified; promotion receipt is in #1081, physical/signed sampling remains separate.
**Updated:** 2026-10-13 · **Planning baseline:** `8d7bc723e`.

The [build contract](REMOTE-DIAGNOSTICS-IMPLEMENTATION.md#current-direction--attach-a-zip-to-a-bug-report) defines the current archive-attachment release.
Hosted submission is deferred; web/phone export and native TV transfer have current software qualification. This file is the canonical execution ledger.
Credential sign-in forms omit the report action under issue #1321; connection,
setup, saved-session recovery, and signed-in reporting keep their entry points.
The historical packet receipts below describe their original source.
The [project backlog](PROJECT-BACKLOG.md) indexes the effort rather than
duplicating its evidence.

## 1. Ownership and source

| Field | Current value |
|---|---|
| User authorization | 2026-10-13: produce a diagnostic ZIP users can attach to bug/issue reports. This supersedes hosted rollout and the private-pilot approval. Normal qualification and green merge continue; no production service changes. |
| Implementation owner | GPT-6.1 Sol · chat `01a13167-0996-7723-a0fb-fa64421a384d` |
| Qualification pull request | #1081 |
| Intended effort | Archive implementation merged into `main`; TV follow-up #1081 on `codex/diagnostics-tv-archive` |
| Task branch convention | Normal commits batched into qualification follow-up PRs |
| Planning source | `8d7bc723e47f326eb213dce7857dd4b36f261867` |
| Implementation workspace/base | Independent owned clone; current main base `cd5c8ddf1`, archive landing `569678f25` (#1066). TV product commits `8c80b9a81` and `77ae61186`, native witnesses `5616f0f4b`; review correction `d0f0df4d9`; focused qualification passes and all three final-review findings are corrected. User checkouts untouched. |
| Compiler loop | Pinned local Rust/cargo 1.97.1; warm isolated target; fresh landed-main all-target check passed. |
| Plan review | Original review: 12 findings corrected; history follow-up: two findings corrected. One final TV-archive review found three issues, all corrected with focused regressions in §26; no repeated review cycle. |
| Promotion/deployment | #1037 and archive-first #1066 merged. #1066 landing `569678f2513695bb9f7b1923ed2c39af6dd19043`, eight required passes. Hosted/private service rollout cancelled by the archive direction; no production changes by this chat. |
| Current checks | #1066 merged with eight required passes. TV download source compiles on Apple TV and Android; focused single-download/expiry/cancellation tests pass. Apple TV shipped remote UI passes selected-incident download and close revocation; Android TV export/QR, D-pad, recovery and default Select-to-edit pass four tests. iOS/tvOS capture/wire suites each pass eleven cases; Android capture/wire/download suites pass twelve. One final review complete; three findings corrected and focused qualification repeated. The exact required gate and landing receipt are maintained in #1081. |
| Native file ownership | K04 receipt integration holds Session.swift, PlurxAPI.swift, Session.kt and Net.kt; qualification preserves those files. |
| Next action | Software archive work is complete. #1081 carries its authoritative required-gate and landing receipt; signed/physical sampling is separate acceptance work. Hosted delivery remains deferred; no service or fleet installation. |

Read current AGENTS and pipeline amendments. Isolate implementation from the
shared checkout and preserve unrelated `Claude outputs/` artifacts.
Planning changes comprise this file, the implementation plan and the two
index/backlog rows; the builder adopts them together in a normal commit.

## 2. Packet ledger

| Packet | Scope | State | Branch/PR | Verification and remaining evidence |
|---|---|---|---|---|
| B00 | Isolated workspace, compile baseline, plan adoption | Software verified; old gate superseded | Merged PR #1037 | Pinned Rust daemon check, iOS/tvOS builds, Android app/test APKs passed on current main |
| B01 | Schema/privacy fixtures and archive inspector | Landed; software evidence recorded | Merged PR #1037 | Owned-clone workspace Clippy, iOS/tvOS and Android compilation passed; 38 shared manifest cases retained; core/web final runtime checks passed; all 21 focused native cases passed in §22 |
| B02 | User-scoped server/worker snapshot collection | Landed; software evidence recorded | Merged PR #1037 | Pinned daemon Clippy passed; positive attribution, accepted route receipts, account-generation fencing, bounded jobs and signed peers implemented; real HTTP acceptance pending |
| B03 | Support auth, grants, uploads and durable storage | Landed; software evidence recorded | Merged PR #1037 | Independent encrypted SQLite publication, separate capture authority, provisioning/revocation, resume/refresh, quotas and external deletion/revocation fences implemented; final Clippy and composed upload/delete/missing-fence runtime checks passed; broader failure matrix remains open |
| B04 | TV handoff, mobile page, inbox and packaging | Landed; software evidence recorded | Merged PR #1037 | Phone claim/approval protocol, responsive completion page, named passkey inbox and TLS/service packaging built; composed browser races/passkeys passed; physical acceptance pending |
| B05 | Web/TV browser reporting | Landed; software evidence recorded | Merged PR #1037 | Early typed recorder, account-bound storage/recovery, worker ZIP export, Help/login/player entry points, consented upload/phone flow and bounded checks built; hook/types, focused privacy/wire/recovery/consent and composed upload passed; hardware focus remains open |
| B06 | Apple mobile/TV reporting | Landed; software evidence recorded | Merged PR #1037 | Recorder, protected account store, deterministic ZIP/share, fixed support transport, exact archive resume, report UI and local-only TV approval written; seven focused cases passed on each simulator platform; offline iPhone and Apple TV remote capture/consent/relaunch UI passed; normal hook passed; direct send and physical acceptance owed |
| B07 | Android mobile/TV reporting | Landed; software evidence recorded | Merged PR #1037 | Closed recorder, private account store, encrypted authority, resumable transport, ZIP/share and TV QR/approval dialog written; app, JVM regression sources and test APK compiled; normal hook passed; signed release and physical acceptance owed |
| B08 | Reproduction, connection checks, diagnostic adapters | Landed; software evidence recorded | Merged PR #1037 | MetricKit/ApplicationExitInfo adapters normalize supported account-attributed evidence; original reproduction identity/recovery and Android bounded checks implemented; 22-case shared event fixture; physical provider delivery remains open |
| B09 | Composed security/resource/device qualification | Browser software verified; physical open | Merged PR #1037 | Global capture limits/expiry and authority cleanup tightened; real local support/browser/passkey acceptance passed; physical matrix and native overhead measurements remain open |
| B10 | Operations, exact-candidate promotion and rollout | Merged; hosted rollout cancelled | Merged PR #1037 | Operations/runbook landed; one review and all 12 corrections complete; eight fast checks passed; merge verified at `8040f862c`; isolated qualification/read-only deployment audit active |

Replace “Not started” with claimed/building/built/software verified/physical
verified/promoted as evidence warrants. “Built” never means deployed.
Every command receipt states source commit, command, result, platform and
artifact location. Keep failure evidence and distinguish pre-existing faults
from regressions. Record test absence as absence, not a passing check.

## 3. Current archive acceptance matrix

| Surface | Implementation | Software evidence | Remaining acceptance |
|---|---|---|---|
| Web desktop/mobile | Merged #1066 | Actual anonymous/authenticated ZIP downloads, selected identity, inventory hashes and no support calls passed | Physical browser/device sampling |
| iPhone/iPad | Merged #1066 | Exact ZIP file, real system share sheet and retained incident recovery passed | Signed/physical file sharing |
| Android phone | Merged #1066 | Actual readable ZIP share URI, selected incident and recovery passed | Signed/physical recipient sampling |
| Apple TV | Local HTTP archive transfer built in follow-up | Current-source bounded download/expiry/cancellation tests and shipped remote UI pass | Promotion receipt in #1081; signed/physical QR and LAN sampling |
| Android/Google TV | Local HTTP archive transfer built in follow-up | Current-source socket/expiry/cancellation/identity tests and shipped TV UI pass | Promotion receipt in #1081; signed/physical D-pad and LAN sampling |
| Capture, privacy and recovery | Merged #1037/#1066 | Shared wire fixtures, bounded typed events, title opt-in, frozen history and deletion regressions passed | Real provider evidence and native overhead sampling |
| Hosted upload/inbox | Deferred | Historical #1037 software receipts retained | Outside current archive release; no deployment required |

## 4. Decisions and external inputs

Paul's archive direction supersedes hosted upload, inbox and hosted phone
handoff. No DNS/TLS/provider answer or service installation is needed. The TV
local transfer uses trusted Wi-Fi and an explicit, temporary unencrypted HTTP
capability. Phone-to-TV reachability is required; isolated guest networks are
not qualification fixtures. Signed/physical sampling is acceptance evidence,
separate from implementation and owned software-device proof.

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

- Historical PR state (2026-10-12): [#1037](http://forge.lan:3000/noirr/plurx/pulls/1037)
  was a draft while implementation continued. Its final review and fast lane
  had not started at that checkpoint. The app attachment tool refuses Forgejo URLs;
  the canonical link is retained here and in the local status page.

## 7. B01 — strict report format and archive inspection

The v1 manifest is closed: unknown keys, duplicate keys, nulls, noncanonical
integer/counter/UUID encodings, invalid coverage reasons and unsupported
versions are refused. Counters are unsigned 64-bit decimal strings;
measurements are bounded JSON integers. Structured event facts have a closed
enum/measurement vocabulary. User-authored description is explicit free text. A selected title is allowed
only in the closed playback projection after positive consent.
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

## 10. B05 — browser integration in progress

Early recorder/storage/hash/export assets are registered before the existing
bootstrap reporter. Raw error strings, URLs, headers, titles and credentials
never enter the event recorder. Playback samples use the existing observer
tick, with no playback retry or timer owner. Frame/drop counters retain decimal
string precision. Optional title projection requires explicit consent and
closed archive fields. Render range and browser crash/DNS/TLS visibility remain
truthfully unknown/unsupported.

The report dialog freezes before typing and is available from Help, sign-in,
boot failures and player errors. It provides portable ZIP export, local storage
fallback, account-fenced recovery, consented bounded upload/resume and a
physically approved phone flow. Network remote actions are refused in this
local reporting scope. Reproduction and checks have five-minute/fifteen-second
owners; checks cap body consumption at 2 MiB. Local enrollment is scoped to
the accepted account and installation, with explicit expiry/revocation limits.

Syntax and static type checks passed without increasing the existing baseline;
the first affected Rust compiler check passed in 56.85 seconds. Sampling,
recovery and schema refinements passed the final compiler and normal hook.
Regression fixtures are declared but not executed per packet. No browser
interaction, TV focus, hardware or end-to-end delivery pass is claimed.

Final B05 refinements preserve the selected account on reload, erase foreign
local namespaces, persist the meaningful history preference, end reproduction
on a monotonic five-minute deadline, preserve terminal events during merges
and attribute playback facts to current media attachments/authentication.
Correlations hash accepted session labels in bounded asynchronous work; raw
labels never enter reports and cannot establish server receipt authority.
The protected upload enrollment cache and sign-out revocation are bounded and
fail without claiming immediate knowledge when Cinema/support is unavailable.
No new type-error allowance is introduced. The latest exact-tree normal hook
provides the remaining embedded-Rust compiler receipt before publication.

## 11. B06 — Apple capture, report and delivery

Apple build 233 adds an off-main protected store and exact immutable archive
resume, scoped Keychain enrollment/upload/creator authority, a cookie-free
redirect-refusing support transport and real chunk/submission/handoff APIs.
Help and pre-login/server-loss paths work without an authenticated API;
ordinary library, Live TV and channel playback have report entry points.
The report sheet blocks Cinema companion navigation, and the TV approval
button remains local. iOS/iPadOS use the system share sheet with temporary
file cleanup. Missing platform evidence remains explicit: iOS not delivered,
tvOS unsupported pending B08's supported adapter work.

Reproduction has one recorder owner and a five-minute deadline; background
ends it. Guided checks have a 15-second owner, a total 2 MiB reply ceiling,
only the configured Cinema endpoint, URLSession DNS/connect/TLS observations
where exposed and explicit missing phases. They do not transfer media.
Account generation invalidates operations and playback evidence. Sign-out
clears unsubmitted local captures and authority, with a pending-report warning;
an account replacement cannot open another namespace.

Compiler receipts: `make apple-build`, Xcode 27.0, iOS and tvOS simulator
applications compiled. `xcodebuild ... build-for-testing` compiled iOS XCTest
sources; no XCTest executed. Initial sandboxed Swift macro-host failures were
host restrictions; the authorized owned-clone build passed. The tvOS form was
split and its unavailable disclosure control replaced before the clean pass.
Private receipt logs live in owned ignored evidence storage.

Declared behavioral regressions (await final check execution):

- `clients/apple/Tests/DiagnosticCaptureTests.swift::testBoundedRecorderRetainsTerminalAndExactCountersWithoutSecrets`
- `clients/apple/Tests/DiagnosticCaptureTests.swift::testRecoveredStoreCannotOpenAnotherAccountOrResurrectClearedCapture`
- `clients/apple/Tests/DiagnosticCaptureTests.swift::testArchiveHasStandardCRCAndTitleRequiresConsent`
- `clients/apple/Tests/DiagnosticCaptureTests.swift::testSupportDestinationRefusesCredentialsPathsAndUnapprovedPlaintext`

Runtime/physical share, support receipts, restart, D-pad focus, privacy/resource
measurement and the final source mapping remain acceptance work. No device,
release signature or production support deployment is claimed.

## 12. B07 — Android capture, report and delivery

Android versionCode 167 adds a closed bounded recorder with conflated writer
mailboxes, backup-excluded account files, encrypted Keystore/AAD authority,
exact immutable ZIP resume, the dedicated cookie-free redirect-refusing
support transport and the phone/TV report dialog. A narrow FileProvider
exposes only explicitly shared report files with temporary read grants.
Support and installation preferences are excluded from both backup and device
transfer. Uploads require a reviewed destination and consent; foreground
retry resumes the same capture. One-use invitation proof is persisted before
redeem and its resulting authority is tied to the chosen origin.

Help, connection/sign-in, library and Live TV playback expose reporting. Closed
network observations exclude URLs, requests and free-form errors. Owned
playback callbacks use their original authentication generation and retain
frame/drop counters as decimal strings, on the existing cadence. The report
dialog invalidates and blocks Cinema companion navigation; only a local TV
button can approve the phone's comparison phrase. The support service checks
that phrase and report revision again. QR generation uses pinned ZXing core
3.5.4 from its [upstream release](https://github.com/zxing/zxing/releases/tag/zxing-3.5.4).

Compiler receipts use local JBR 21.0.11, installed SDK and the independent
owned clone. `assembleDebug compileDebugUnitTestKotlin assembleDebugAndroidTest`
compiles the app, JVM regression sources and instrumentation APK. No JVM unit
or instrumentation suite ran per packet. Signed release/shrinker evidence,
connection/provider refinements, restart, sharing, real support receipts,
D-pad focus and physical/resource acceptance remain B08–B10 work.

Declared behavioral regressions (await final check execution):

- `clients/android/app/src/test/java/tv/plurx/app/diagnostics/DiagnosticCaptureTest.kt::boundedRecorderRetainsTerminalAndExactCountersWithoutSecrets`
- `clients/android/app/src/test/java/tv/plurx/app/diagnostics/DiagnosticCaptureTest.kt::restartStoreRefusesAnotherAccountAndCannotResurrectClearedCapture`
- `clients/android/app/src/test/java/tv/plurx/app/diagnostics/DiagnosticCaptureTest.kt::portableZipUsesStandardCRCAndRefusesTraversal`
- `clients/android/app/src/test/java/tv/plurx/app/diagnostics/DiagnosticCaptureTest.kt::supportDestinationRejectsForeignAuthorityShapesAndPlaintext`

## 13. B08 and composed candidate — 2026-10-12

Supported platform APIs are now wired. Apple consumes MetricKit diagnostic
payloads on iOS/iPadOS; tvOS reports unsupported. Delayed delivery outside one
continuous account lifetime remains unavailable with `context_unavailable`;
raw stacks, payload JSON, paths and addresses never enter an archive. Android
API 30+ consumes bounded ApplicationExitInfo rows tied to a private prior-run
PID/account marker; account replacement erases that marker. Only actual crash
exit reasons become `process_exit`; ANR and other interruptions remain distinct
normalized observations. Each available diagnostic carries its source app/build
when the provider or private run marker proves it. Missing provider identity and
clock alignment remain explicit rather than being borrowed from another run.

Native and browser reproduction retain the original incident and report ID
while the form closes; completed captures persist for recovery. Sending requires
stopping the recording. Android adds configured-endpoint checks with a 15-second
deadline and 2 MiB total reply ceiling; OkHttp phases are observed through the
dedicated client's EventListener. No media probe or playback retry is added.
Administrator snapshots have an explicit collect action and include only typed
version/build, readiness, active-producer count and hardware admission counts.
The server continues to enforce the administrator boundary.

Development receipts before exact-main merge: Apple build 234, iOS/tvOS applications compiled;
iOS declared XCTest sources compiled before the last administrator projection
change. Android versionCode 168, app/JVM regression source/test APK compiled
before that same final projection change. Web types and static contracts passed
after replacing a startup forward reference with an auth notification. A later
additive build-label field briefly failed web types and was corrected; affected
checks are rerun before commit. No native unit suite ran.

Resource fixes cover global native draft bytes/count, old namespace ring expiry,
removed-capability cleanup, Keystore key creation serialization, export abort
cleanup and authentication-generation fencing on queued native writes. These
are implementation claims awaiting composed evidence. No hardware or real
production delivery receipt exists.

## 14. Exact-main candidate and integration findings

Current main `607d3577c` is merged in `9560f443e`. Pinned workspace
`cargo check --workspace --all-targets --locked` passed on that merged base
in 2 minutes 42 seconds. Final native/type checks are repeated only for the
changed consent, clock and explicit reproduction code. Diagnostic attachments
require a per-report selection; the manifest flag exactly matches the archive
entry. Bounded round-trip clock estimates now carry ahead/behind milliseconds
and uncertainty; no global event ordering is claimed.

The real composed browser fixture reached staged upload, single phone claim,
creator approval and phone submission after the TV context closed. It exposed
a real 429 on operator sign-in because code claims and WebAuthn ceremonies
shared an IP bucket. The namespaces are now separate, preserving each limit.
The fixture loads the shipped browser recorder/export/delivery scripts into an
isolated TV context, while phone and inbox retain their actual content security
policy. This verifies browser software, never native hardware or physical TV
approval. Final passkey/inbox, review and fast-lane receipts remain pending.

## 15. Single final review — corrected findings

Exactly one adversarial agent reviewed candidate `707d8bb1b` against
`607d3577c`; no second review or routine subagents were used. It requested
changes for five P1 and seven P2 defects. The corrected candidate includes:

- Mandatory protected deletion/revocation journals at startup and database
  operations; missing fences are not silently recreated.
- Browser recovery fenced across asynchronous account transitions; destination
  and confirmation use the immutable consented origin.
- Final edited description transactionally replaces archive content, inventory,
  hash, metadata and revision before publication; growth obeys storage quotas.
- Finished reproduction ownership is released and every client has a deliberate
  new-incident action.
- Android expired rings/empty namespaces and all invitation keys are cleaned;
  the active destination folder survives pruning. Browser scans retain only
  metadata and enforce budgets across every allowed row, including rings.
- Enrollment revocation precedes bearer logout; concurrent browser snapshots
  own separate cleanup IDs/results. Apple deadline preserves partial observed
  checks with explicit timeout coverage. Creator TV status carries the submitted
  receipt reference.

The composed browser fixture is extended to assert the final ZIP description,
TV creator receipt and refusal of either missing fence journal. The actual
report functions have declared regression cases for account recovery and
destination consent races; no per-packet unit execution was added.

A synthetic Node observer measurement recorded 50,000 callbacks: p50 2.67 µs,
p95 3.38 µs, p99 4.50 µs; 7,651 events / 1,677,690 encoded bytes, with the
terminal marker retained and 42,351 samples dropped. This is a checkout-host
Node measurement, not native or hardware playback-overhead acceptance.

Final fast-lane receipts must identify the published corrected head; earlier
source checks are not silently reused when their coverage changes. Physical
mobile/TV delivery, focus, real provider delivery, signed releases, production
TLS/hosting and native device overhead remain explicitly unqualified.


## 16. Published candidate and final fast lane

Published candidate `f25cfce318975461042dedc37220e607a57fd57e` includes
all twelve findings from the one final review. Corrected iOS/tvOS application
builds, iOS XCTest source compilation, Android app/JVM test-source/test-APK
compilation, final workspace Clippy/format/catalog/93-script syntax hook and
web type/static checks passed. The final support/browser fixture passed with
receipt `C-52730cd5a155`, including edited ZIP content, creator receipt and
both missing external fences. These are local software receipts.

The final focused `make unit-diagnostics` run passed all six core diagnostics
tests (with `hiqlite-store`) and eight browser tests. Two new browser test
harnesses exposed an undeclared parser dependency; the corrected harness uses
only Node and waits for the actual recovery boundary. Both failed cases then
passed on their focused retries. No passing unit case was rerun, and no native
unit/instrumentation suite was executed.

PR #1037 is ready. Fast-lane run 4868 passed scope/mobile-version checks but
failed historical evidence: duplicate rows for earlier packet commits and
missing rows for the final three corrective commits. The ledger repair keeps
one witness per commit and adds those missing anchors. This is validation
metadata and test-harness repair; production source is unchanged. The failed
preflight and affected metadata coverage must pass on the resulting candidate
before merge. Physical native/TV/provider delivery, signed artifacts, hosting,
TLS, enrollment and native observer overhead remain open acceptance inputs.


## 17. Required preflight — integration repairs

Run 4870 on `745a695a9` passed historical evidence, scope and mobile version,
then exposed packaging/contracts integration gaps. The repair updates the
Android current build claim and all four web asset-count witnesses; regenerates
563 dependency notices from locked Cargo metadata and includes the official
MPL-2.0 text/source links; registers both browser diagnostics test files in the
existing web unit recipe; and registers only the report modal's existing
Tab/Shift-Tab focus adapter in the input fence. Application behavior is
unchanged.

The Rust ownership inventory records exactly two snapshot tasks, the fixed
retention sleep and five-second collection timeout, and one support HTTP
`response.status()` accessor. Collection and expiry are read-only and bounded
by their original job cancellation/deadlines; no producer, process, watchdog,
playback retry or decision owner is added. The status/header witnesses and
Apple fragment lifecycle headers now agree with their index rows.

Fourteen targeted failed/invalidated policy checks have applicable passes;
the document status check required one further missing-header repair and a
focused retry. Passing cases were not rerun. The new final fast-lane run must
pass before merge; no Rust/Apple/Android suite was added to this local retry.


## 18. Playback presenter contract — final integration correction

Run 4874 on `f1768b381` passed the Python policy/operations checks, adopting
358 and 1,007 attributable successes and executing only the one and ten
previously failed methods. Its Node phase exposed an added report button in
`playbackSurfaceActionHtml`, which made the rendered action count differ from
the presenter's prescribed actions. The duplicate is removed; the existing
Report a problem entry remains in player chrome and Help. No playback input,
retry, timer or decision owner changes.

The failed `node tests/playback/web-policy.test.js` gate now passes locally.
Its function-only health harness includes the actual diagnostic observer
helper, so borrowed health functions retain their served dependency without
replacing it with a fake. Web types pass against the unchanged 493/80 baseline.
The applicable policy and native compiler results remain valid; the new
corrective web head still requires its fast-lane pass before integration.


## 19. Exact-candidate native passes and final CI environment/fixture fixes

Run 4877 on `de8e35565` passed preflight, mobile version, Apple compilation and
Android compilation. Its web job reached two older logout fixtures whose
function-only environments omitted `window` and assumed bearer logout started
before the new enrollment-revocation wait. The corrected fixtures assert that
private diagnostic history is erased and that each captured account finishes
its own enrollment revocation before its bearer request; late old replies
still cannot clear a newer session. The failed page-read-budget script passes
locally, with real bounded test waits rather than scheduler-turn assumptions.

The Linux Rust job failed because its fresh Ubuntu container lacked
`openssl.pc`; the selected `openssl-sys` graph is not vendored. The existing
Rust prerequisites now explicitly install `libssl-dev`, as required by the
support service's already documented WebAuthn dependency. Rust source and
native source are unchanged. Local pinned workspace compilation/Clippy remains
applicable, and the Linux environment check must pass on the repaired workflow.
All final native/policy successes remain evidence for their unchanged inputs.


The remaining 39 required web scripts were then checked locally before the
next push: 37 passed immediately. Two function-only harnesses omitted the
actual `diagnosticDeveloperCard` and `observeDiagnosticPlayback` definitions.
Adding those served helpers made the full settings script pass 65/65 and the
single failing Shared-direct health case pass. No production callback or
assertion was weakened; the other passing scripts were not rerun.


## 20. Current-main integration after the green candidate

All eight fast-lane checks passed on `72b6e2e8e` (run 4881). Current main had
advanced to `eef013b5e` during validation. Only the Rust ownership inventory
conflicted: both branches added independently reviewed waits/accessors to the
same counters. Integration preserves both ownership explanations and their
additive counts (1,739 time constructors and 753 launch-shaped accessors).
Current main's HEVC/VOD/AniList code and its evidence are retained unchanged.

The merged Rust tree passed the fresh pinned Rust 1.97.1 workspace/all-target
check. Its focused ownership inventory test passed with the combined counts.
The normal Clippy/format/catalog/JavaScript hook remains required at commit.
Native and web application source is unchanged; its passing receipts remain
applicable. The published updated head must receive its fast merge gate before
integration; no second adversarial review is added.


## 21. Merge receipt and continued qualification — 2026-10-13

PR [#1037](http://forge.lan:3000/noirr/plurx/pulls/1037) merged at
`8040f862c3458245480065e11dec643fbfe370b3`, from qualified head
`6bc7235a07463719617957b9e372686303dab781`. Fresh Forgejo state confirmed
all eight required checks passed and the candidate was mergeable. The API
request pinned that head and supplied all 32 `Regression-Test` lines in
`MergeMessageField`; the landed commit retains all 32. No repeated review
or unchanged-suite rerun preceded the merge.

The ready-to-merge handoff was an incomplete response to Paul's authorized
merge instruction. Qualification now continues on the actual landed source.
Physical acceptance, signed releases and production deployment remain open.

Automatic approval review rejected the earlier broad production instruction
because exact targets and changes were unspecified. That rejected instruction
was not delivered and supplies no deployment authority. Read-only inspection,
isolated qualification and concrete rollout preparation continue; production
service/network changes wait for approval of the prepared exact scope.


## 22. Continued qualification and startup recovery — 2026-10-13

Qualification uses the independent clone at landing `8040f862c`; the first
follow-up commit is `05ebe4aa0`. Existing user checkouts, physical app
installations and production services have not been modified by this chat.

| Evidence | Actual result and limit |
|---|---|
| iOS focused XCTest | `DiagnosticCaptureTests` and `DiagnosticWireTests`: seven tests, zero failures on an owned iOS 26.5 simulator. |
| tvOS focused XCTest | The same two classes: seven tests, zero failures on an owned tvOS 26.5 simulator. This is simulator evidence. |
| Android focused JVM | `DiagnosticCaptureTest` and `DiagnosticWireTest`: seven tests, zero failures/skips; XML reports verified. |
| iOS shipped offline UI | `DiagnosticReportingTests::testOfflineCaptureKeepsUploadExplicitAndRecoversAfterRelaunch`: one test, zero failures after correcting the fixture's scrolling. Frozen report, available local export, disabled unconsented upload, relaunch recovery and preserved consent boundary were observed. |
| UI fixture failures | The initial unregistered iOS selection executed zero tests and is not passing evidence. The first registered run failed because SwiftUI had not created the offscreen Send row. The corrected fixture scrolls the shipped form and passed without changing production UI. |
| Linux support artifact | Source-only committed archive `05ebe4aa0`, pinned Rust 1.97.1, local linux/amd64 container: release build passed. SHA-256 `1b0f21ac23efc7da00ce70d4462131927c47c8f0fc57044d0c04dc236629ebc6`; dependencies and command startup checked with networking disabled. Not deployed. |
| Device inventory | Paired phone and TV devices are reachable, but their installed Cinema is build 232. The current feature has not been installed over those apps. Owned iOS/tvOS simulators and Android phone/TV emulators provide isolated qualification surfaces. |
| Hosting inspection | Read-only audit found no support origin, certificate or support service on the four Cinema nodes. The infrastructure repository contains local monitoring ingress, but no inspected public support hosting/TLS configuration. Real addresses and trusted certificates remain rollout inputs. |

A separate build chat reported that another process replaced two Cinema
nodes with landing `8040f862c`, after which the composed daemon crash-looped.
The diagnostics router re-enabled Axum's route checks when merged, rejecting
Plex literal-colon routes. The earlier isolated support/browser fixture did
not boot this composed Cinema router; its passing result did not cover this
startup integration.

Recovery belongs to chat `01a132c0-a6bb-7580-a670-659cca2ffbdf`, on
`codex/diagnostics-router-startup`, PR #1067. That owner reproduced the exact
pre-fix failure and reports the focused full-router regression passing on
pinned Rust 1.97.1 after the narrow repair, with one adversarial review and
normal hooks passed. PR #1067 landed at `fa464662fe0b671cec9f37a975b56727b9b26840`; actual fleet recovery is in progress. The owner directly verified the human's urgent lint/syntax validation exception before merging; local pinned regression, review and hooks passed, while Rust CI was still running. This exception is specific to that startup repair.
This chat has confirmed it owns no fleet rollout and performs no competing
router edit or production replacement. Healthy nodes are preserved by the
recovery owner. The next daemon qualification must use that landed repair. The human also identified repair chat `01a13599-851f-7f20-a123-7913595862e1`; both chats coordinated a single landing and recovery owner rather than competing patches. This qualification branch held main merges during the repair gate and retains the fleet hold while recovery runs.

The original eight-check receipt and 32 regression declarations remain true
for the original PR. They do not imply that this subsequently discovered
startup defect or physical/deployment acceptance was covered.


## 23. Native UI defects found by qualification

The Android anonymous connection report action overlapped the system
navigation area on the owned Android 36 phone. A tap on its displayed text
could enter system navigation instead of the report. The action now consumes
navigation-bar insets. The focused instrumentation regression
`DiagnosticReportNavigationTest::anonymousReportButtonClearsSystemNavigationAndOpensReport`
passed one test on that emulator, checking actual window-inset bounds and
opening the shipped report dialog. The initial hierarchy preserves the
pre-fix overlap evidence.

Both native report owners used authorization retirement when switching to a
saved incident, unnecessarily clearing local history. Resume now cancels the
previous operation, transport and handoff polling while preserving the
account's pending history. Each selected incident also owns its scroll
context, so the coverage/privacy summary opens at the start. These are
correctness fixes with no feature toggle.

The real Apple TV remote fixture exposed the retained Form scroll offset:
after selecting a saved incident, the summary could remain offscreen while
focus returned to the toolbar. Preserving history alone did not fix that
scroll state. With incident-scoped Form identity, the actual remote case
passed one test, zero failures, on tvOS 26.5. It checks the exact incident
identity from before relaunch, preserved history and disabled unconsented
upload. Fixture compile/focus failures and the pre-fix scroll failure are
retained separately; they are not passing evidence.

The final iPhone and Apple TV cases each passed one test, zero failures.
Android phone instrumentation passed the navigation-inset case and the
saved-incident/history/consent case, one test each; the separate actual
phone UI flow also recovered the exact incident across process relaunch.
Android TV D-pad qualification is active. Apple source claims build 235, with the test wired into the iOS
UI target; Android source claims versionCode 169. Physical apps are still
untouched. The five focused support regressions also passed: restart and
immutable receipt, two-phone approval/revocation restore, real HTTP origin
and listener boundaries, missing external journal rejection, and final
edited archive content. That is five tests, zero failures; broader disk-full,
provider, device and deployment acceptance remains open.


## 24. Follow-up adversarial findings and composed startup receipt

The one final review of candidate `f96eadf666` against `fa464662f` found two
P1 defects in retained native history. A row could restore its original
pre-upload snapshot, losing the current consent, archive binding and support
issuer. Deleting the selected row left the active draft alive, so Close or
an upload progress save could recreate it. Neither finding is waived.

Resume now treats the row as identity only: cancel and drain prior work,
reload the protected store under the account/work revision fence, and
refresh pending rows after persistence. Frozen archive/privacy/issuer state
cannot be replaced by an older write. Deletion clears the selected owner
before draining its work; store tombstones reject queued draft/archive
writes and secret cleanup runs after the drain. The new frozen-consent/deletion regression passed once on iOS, once on tvOS and once in Android JVM storage. The shipped Android phone owner also passed its failed-grant/frozen-privacy/deletion case (one test). Actual iOS and tvOS remote capture/consent/relaunch UI each passed again after selecting the exact saved incident ID. Those UI fixture failures are retained; selecting the newest row was not evidence of recovery.

Pinned Rust 1.97.1 all-target check and the composed-daemon regression
`an_unauthenticated_facade_request_is_counted_as_a_client_that_tried` passed
on `f96eadf666` (one test, zero failures). The exact-source Linux support
release build also passed after reducing build concurrency to fit the
container; the first attempt was killed for memory, not a compiler failure.
The recovery chat reports two nodes healthy and two repair images preloaded;
its remaining rollout is waiting for the Mac to be unlocked. This is that
chat's recovery receipt, not a deployment by this diagnostics owner.

The acceptance matrix and generated summary now name the qualified subset
and active follow-up #1066. Physical/direct-send/cross-network/provider and
production support acceptance remain open.

The final corrected Android phone and TV report owners each passed the
failed-grant/frozen-privacy/deletion instrumentation case, one test each.
The saved-incident/explicit-consent instrumentation case passed again on
both profiles, one test each. Android TV semantic instrumentation is
software evidence; it does not stand in for physical D-pad/cross-network
acceptance. iOS/tvOS real shipped UI each passed one exact-ID relaunch
case. The local status regression and four docs-index checks passed; the
infrastructure-name scrub is clean. Both native app versions remain the
qualification claims 235/169, without changing physical installations.

Current main `b2a186450` was merged normally into the qualification branch
after the review corrections. All-target pinned compilation and the
composed startup regression passed on archive-flow correction `069615e85` on the integrated current base.
The final branch and seven named regressions must still pass the required
fast gate before its green merge; that exact receipt is recorded in #1066.

The first #1066 fast lane on `c5e9fd30e` stopped at corrective-history
coverage: the two native corrective commits lacked `tests/client-fixes.toml`
witnesses. Both now have exact commit/source/test rows, and local
`make history-check` passed. These catalog entries record the real passing
regressions; they do not add or waive runtime evidence. The corrected head
must pass the gate before merge. Paul took ownership of fleet installs;
this chat keeps all fleet/network/service writes held.

## 25. Archive-first direction — 2026-10-13

Paul requested a diagnostic archive that users attach to bug or issue reports,
in place of wiring up hosted submission. This supersedes the private-pilot
approval: no service, proxy or Tailnet mapping will be installed by this chat.
The existing ZIP generators are reused; ordinary upload and invitation controls
are removed, and account setup no longer provisions support enrollments.

Web downloads and native phone share sheets are the release path. Native TVs
keep capture/history; transferring the TV archive without the hosted service
is still missing implementation, separate from physical acceptance. The original
B00–B10 delivery receipts above remain historical evidence, not completion of
the new archive scope. PR #1066 records the exact source, focused file-export
checks, required gate and landing status.

Archive qualification uses real downloaded/exported files: the shipped browser
report dialog produced anonymous and signed-in ZIPs without support requests;
selected incident IDs, summary, description and every manifest file hash were
verified. Android's actual share intent exposes a readable `application/zip`
attachment with a scoped URI grant and the selected incident, without consent,
binding, origin or enrollment. The Apple report model wrote the exact selected
archive and retained an unconsented local draft. Phone and native TV relaunch
recovery passed on owned simulators/emulators. These are software fixtures,
not physical-device or signed-release acceptance.

Android TV description traversal failed a real Down-key assertion before the
fix. The existing select-to-edit text field plus an explicit Down focus target
now reaches the privacy switch; the focused navigation and select-to-edit cases
passed two tests. An initial test-clock timeout was corrected by freezing the
caret animation clock, exposing the actual focus assertion before the product
fix. Failed and zero-test receipts are retained privately, never counted as
passes. The required gate and precise landing remain in #1066.

The actual iOS share-sheet case passed one executed test after matching the
observed Save to Files action cell. A URL-bound presentation keeps the exported
file alive until dismissal. Earlier button-selector failures and the zero-test
receipt remain failures, not qualification. This batch claims Apple236 and
Android170 above current main’s Apple235/Android169; current-base compilation
and the required fast gate remain recorded in #1066.

Final current-base receipts: iOS and tvOS capture/wire suites each passed nine
cases; Android capture/wire passed eight. Actual UI passed two iPhone cases
including the system share sheet, one Apple TV retained-incident case, three
Android phone and four Android TV cases. The iPhone generated target's missing
report-test source was caught by its zero-test result, restored normally and
rerun with two executed passes. Apple236/Android170 are valid against current
main. Pinned all-target check, full composed-router startup and normal hooks
passed; the local history audit passed all 3773 corrective commits and 565
client-fix anchors. The later playback landing `96e4c49e9` changes no native
product/test inputs; native receipts remain applicable after source comparison.
Its focused browser rendezvous checks were included in the latest-base web run.
The exact final head, fast-gate outcome and landing are maintained in #1066.

## 26. Finish the TV archive — 2026-10-13

Paul instructed the owner to continue until done after #1066's archive-first
landing. The remaining implementation exports the TV's actual selected ZIP
through an explicit, foreground-only local listener. Its QR opens a phone page
with a Download button. GET previews do not consume the single-download POST
capability. No support enrollment, external upload or service installation is
involved. The interface warns that trusted Wi-Fi is required and local HTTP is
unencrypted. Privacy choices and incident changes revoke the active link.

The endpoint binds a private IPv4 interface, has a five-minute wall/monotonic
lifetime, accepts at most 32 bounded requests with an 8 KiB header ceiling,
and closes sockets on report/background/authorization retirement. It serves
only the frozen in-memory ZIP, with attachment/no-store headers and no cross-origin referrer disclosure.
Focused tests cover exact bytes, rejected capabilities/cross-origin/body
framing, one-download use, expiry and cancellation. Final source, UI results,
review and gate receipts will be recorded before merge. Apple237/Android171
are source build claims, not physical installations.

Qualification receipts: Apple TV's actual remote-driven report interface passed
one ZIP/selected-ID/close-revocation case. iOS and tvOS capture/wire suites each
passed eleven tests. Android app and test APKs compile; capture/wire/local
socket tests passed eleven cases. Actual Android TV UI passed three cases:
QR decoding and selected report download, D-pad privacy traversal and exact-ID
relaunch recovery. Android socket close originally crashed the app's accept
thread; the normal cancellation exception is now handled and the corrected
instrumentation passes. A timestamp-only recovery selector matched two saved
incidents; it now uses the report's unique ID. Failed receipts are retained,
not counted as passes. Owned software devices prove neither signed physical
installs nor guest-network reachability.

The single final adversarial review of #1081 found three issues, all corrected:
Android dispatcher-return cancellation could discard an unowned listener;
cleanup now retains the handle in the caller frame and a forced cancellation
regression proves immediate socket retirement. Inputs remained editable during
final persistence; both native forms now lock category/description/privacy
while busy, and Android ignores edits during that window. The actual browser
POST returned 403 because no-referrer produced an incompatible Origin. Both
local pages now use same-origin referrer policy, with no cross-origin referrer
disclosure. The real compiled Android endpoint plus a phone-sized Chromium form
now returns GET200/POST200 and the exact ZIP attachment; the retained failing
GET200/POST403 receipt is not qualification. Android capture/wire/local download
now passes twelve tests, including creation cancellation. Actual Android TV UI
passes three cases including locked export input and later edit/close revocation;
Apple TV actual remote UI passes one selected archive/close case. iOS/tvOS
capture/wire suites each pass eleven on the corrected source. The local history
check caught duplicate witness rows; one witness per corrective commit is the
repository policy and the catalog is corrected before the ready gate.

The reproducible browser regression is retained at
`tests/web/diagnostics-tv-archive-browser.cjs`; compile the Android app first,
select the JDK with `JAVA_HOME`, and run it with installed Playwright and an
optional `CINEMA_BROWSER_EXECUTABLE`. It exercises the real compiled local
endpoint and a phone-sized browser form, then compares the downloaded file byte
for byte. One executed case passes on the corrected source. It is focused local
evidence; the fast lane does not run this browser/native bridge.

The first ready fast gate caught two policy mismatches, not compiler errors:
the Apple qualification fragment's index still said open while its header said
built, and the existing diagnostic description's physical-key handling lived
outside the allowed shared form adapter. The index now matches built; the
unchanged Down-to-privacy behavior is decoded inside `AuthTextField`, with a
nullable explicit focus target that preserves other forms' defaults. The fence
itself is unchanged. Its focused contract, TV D-pad and existing Select-to-edit
regressions are required before the corrected candidate is pushed. The current
source iPhone UI class executes two passes (share sheet and recovery) plus one
TV-only skip; the earlier single-method zero-test receipt is not a pass.
Android phone actual attachment and recovery execute two passing cases.

Both failed gate contracts now pass in a focused 75-test docs/build/fence run.
The corrected Android app/test APKs compile; actual Android TV qualification
passes four cases including the shared form’s default Select-to-edit behavior.
The field adapter movement changes no archive protocol, Apple source, capture,
wire or storage inputs; their recorded current-source results remain applicable.
The final required gate and exact landing are in #1081. No production service,
proxy or real-device installation is part of this archive release.

Main's qualified cluster-observation landing `201313f58` was integrated
normally before the final push. It changes documentation, the observation
script and its operation tests, with no Rust/web/native product or client-test
inputs changed. Both projects’ index/backlog rows are preserved. The source
comparison retains the applicable native/browser/archive receipts; the focused
current-base docs/fence and bounded-observation contracts run again, and the
normal pinned hook checks the exact merged source before publication.


The green candidate `575126ae1` passed all eight checks on run5046. Before
merge, main advanced to the qualified playback landing `b35eebb6b`. The
combined source preserves both regression catalogs and documentation entries;
Apple build238 replaces the colliding claim237. The new Apple and Rust inputs
were qualified before another push. Android171 and the archive protocol
source are unchanged; the existing executed Android and browser receipts
remain applicable. No hardware or fleet installation is part of this step.


The playback-base source passes 60 iOS and 54 tvOS focused tests, the pinned
all-target Clippy/format hook, 22 focused Rust playback regressions and all81
contracts. The history audit passes with both projects’ client witnesses.
A subsequent actual TV report check reproduced remote focus stopping before
ZIP export when the captured preview has many nonfocusable rows. The TV now
puts that preview after every report/history action, and its rows accept remote
focus so the content remains readable. The same populated-report download
check passes after the correction; export and relaunch recovery pass together on the corrected source. The failed run is kept
as a failure, including its zero-test runner restart.


The final TV preview correction is `f777976ce`. Its failed fixture contains
all30 preview rows. The final tvOS report-screen class executes export/close
revocation and offline same-incident relaunch recovery: two passes, one
phone-share-only skip, zero failures. The corrected source passes the normal
pinned compiler hook. Apple238/Android171 are the final claims. The13 original
landing regression fields plus the TV recovery field remain mandatory; #1081
records the exact final gate and landing. Signed/physical sampling is separate.


Before publication, main advanced again to the qualified prepared-clock
landing `f1bffab59`. Its web code, tests and bounded diagnostic-fence witness
are preserved. The diagnostic consent markers surround the shared form call;
Down-key decoding remains in AuthTextField. The obsolete key-import allowance
is removed because this screen no longer imports or decodes keys. Android
runtime behavior and all Apple inputs are unchanged by this integration.
Focused web/fence checks and the pinned hook qualify this combined source.


Main's CI-prerequisite/Android-fixture repair and final SDR evidence landings
advance the base to `cd5c8ddf1`. Their build contracts, fixture and witness are
preserved. Production archive, playback and web source files do not change
from the qualified candidate161d96f68. The fresh normal pinned hook and focused
build-contract/Android-fixture checks qualify this integration; completed
native UI, archive and web runtime results remain applicable by source digest.
The prior candidate's gate5058 and the exact final gate are recorded in #1081.
