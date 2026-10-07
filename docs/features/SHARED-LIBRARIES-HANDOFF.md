# Shared libraries handoff — what remains before completion

**Status:** implementation and qualification open · **Snapshot:** 2026-10-04
03:26 UTC · **Integrated baseline:** `746c67e5d2800fdcb8729c31b90a35ec47c63717`

Companion to [the implementation contract](SHARED-LIBRARIES-IMPLEMENTATION.md),
[the principal census](SHARED-LIBRARIES-PRINCIPAL-CENSUS.md), and
[replicated ownership evidence](SHARED-LIBRARIES-REPLICATED-PRINCIPALS.md).
This document answers what a continuing engineer or agent must do next. The
implementation document remains the acceptance contract and evidence ledger.
This is a dated snapshot: agents are still working. Re-read their branches,
logs and running commands before editing or claiming a result.

## 1. Continue from the isolated integration checkout

The requested outcome remains Cinema-to-Cinema shared libraries over
Tailscale, S1–S8, through actual playback, qualification and effort promotion.
The feature is unfinished. Passing component tests does not establish full
playback, live Tailscale behavior, hardware playback or release readiness.

Use `/private/tmp/plurx-shared-libraries-s3`, branch
`codex/sharing-s3-principals`. It was clean at the baseline above, before this
handoff document and its index row. It is unpublished integration work.

The primary checkout `~/code/plurx` is on
`codex/playback-seek-30s` with unrelated user changes. Do not switch, reset,
clean, commit or overwrite that checkout or unrelated worktrees. Preserve
agents' uncommitted changes and retained stashes. Integrate complete branch
history after qualification; do not discard an agent's merge parents.

Read [AGENTS.md](../../AGENTS.md), [the documentation index](../README.md),
[the development pipeline](../DEVELOPMENT_PIPELINE.md), and
[the compiler loop](../ci/AGENT-COMPILE-LOOP.md). Read
[the web layout map](../clients/WEB-SHELL-LAYOUT.md) before changing web assets.

## 2. Exact checkpoints and active ownership

### 2.1 Root integration contains receiver transport, End and progress

| Integrated checkpoint | What it establishes | Exact finite evidence |
|---|---|---|
| `9a9109602` | Native Source HTTP integration, explicit actual upstream driver/socket lifetime, bounded Source resource client | Source HTTP 19; Native actors 10; three-family preparation FD closure 1; resource framing/header 2; actual Hyper/socket lifetime 1; receiver ownership 8; denied Clippy and normal hook |
| `61820efe8` | B GET HLS dispatch, accepted connection deduplication and actual writer closure, guarded candidate Start response | Receiver/ingress 13, zero ignored, 0.42 s; denied Clippy 77 s; normal hook 74 s |
| `cdc567b86` | Private physical B End receipt, exact retry, original-login ordered progress, independently owned bounded Source resource opening | Receiver/ingress 15, zero ignored, 0.46 s; guarded Store matrix 1, zero ignored, 4.39 s; check 58.83 s; denied Clippy 79 s; normal hook 84 s |
| `746c67e5d` | Full `7ad15ba89` genuine pairing helper and retained Source start stages integrated onto `cdc567b86` | Source HTTP 21, zero ignored, 54.48 s; receiver/pairing 16, zero ignored, 20.64 s; check 54.39 s; denied Clippy 71 s; normal hook 83 s |

The final row is the baseline for the handoff. Root has not integrated the
later Source control/status checkpoint or the final native player checkpoint.
The candidate authenticated Start router remains uninstalled publicly.
Details still report playback unavailable; client launch gates therefore
remain closed. B status/control are not implemented. Missing physical actors
for durable `remote_source` recipes fail unavailable rather than creating a
Local producer. Remote-worker forwarding remains unqualified.

Root logs use these prefixes under `/private/tmp/`:

- `plurx-sharing-resource-native-combined-*` and
  `plurx-sharing-resource-native-final-clippy.log`.
- `plurx-sharing-relay-start-*` and `plurx-sharing-relay-adapter-*`.
- `plurx-sharing-end-progress-*`.
- `plurx-sharing-pairing-combined-*`.

### 2.2 Three agents already own parallel work

| Owner | Worktree / branch | Snapshot and next work |
|---|---|---|
| Root | `/private/tmp/plurx-shared-libraries-s3` · `codex/sharing-s3-principals` | Clean baseline `746c67e5d`; integrate qualified branches, implement strict Source status/control client and B projection, enable public playback only after qualification |
| `/root/s2_validation` | `/private/tmp/plurx-sharing-receiver-real-source` · `codex/sharing-receiver-real-source-fixture` | Clean physical candidate `2b5b0f856` contains full `cdc567b86` and genuine bootstrap; normal hook passed, physical test compiled but unrun; preparing a test-only FFmpeg compiler image and source archive |
| `/root/s3_upgrade_probe` | `/private/tmp/plurx-shared-libraries-vod` · `codex/sharing-source-resource-custody` | Last clean `6b8ef28b4`; pending full merge of `61820efe8` plus owned filesystem-custody changes; session `73064` running regression/check/Clippy sequence; commit and current-base qualification pending |
| `/root/s4_catalogue` | `/private/tmp/plurx-shared-libraries-s4` · `codex/sharing-s7-native-player` | Qualified `ecebec1d6`, full `cdc567b86` included; later Shared status DTO/client draft is uncommitted in four existing Apple/Android Shared model/client files; owned Void Start join, rendering/tests and controls next |

**Root files:** receiver playback/retirement/ingress, sharing client and
playback client, B media router/accepted connection handling, B progress
adapter and integration ledger. Coordinate narrow edits to
[http/mod.rs](../../crates/plurxd/src/http/mod.rs) with S2's test module row.

**S2 files:** new
[shared_receiver_fixture.rs](../../crates/plurxd/src/http/shared_receiver_fixture.rs),
cfg-only Source fixture accessors, its catalog row, and the approved opt-in
classification in `validation/known-red.toml` and its explicit ID test.
Source helper changes stay separate from S3's production actor/adapters.

**S3 files:** Source actor/control/status/resource custody; narrow Source-only
VOD delivery/read seams; their actual runtime tests; later Source HTTP
`/control` and `/vod-status` adapters. Approved narrow pure helpers in
`http/hls/control.rs` and `http/hls.rs` preserve Local behavior.

**S4 files:** existing Apple Shared models/client/context, controller/view,
AppModel/browser and corresponding tests; existing Android Shared data
models/client/context, controller/screen, view model/browser and tests.
Do not weaken Local `PlurxAPI`/`PlurxApi` guards or introduce numeric Local
file/item sentinels. Coordinate new files and manifests before adding them.

## 3. Finish the immediate integration in this order

### 3.1 Qualify genuine B-to-Source transport first

S2's new test is
`sharing_receiver_real_pinned_source_h1_b_h1_h2_start_resources_and_confirmed_end`.
It is compiled but **has not run** at this snapshot. It uses a real Source
`LiveNodeTls`/`SharingTlsListener` and production peer router, a real B
`serve_http` listener and production media router, a test-only candidate Start
installation, authenticated B details and the returned signed locator. It
covers pinned Source HTTP/1, B HTTP/1 and HTTP/2, Copy playlist/init/segment
bytes, public End 204 and exact retry. It currently consumes complete bodies
before End; queued B writer pressure is outside that test's evidence.

S2's physical candidate `2b5b0f856` passed its normal hook. Archive that
committed source, prove byte equality
and absence of `.git`/credentials, then execute the ignored exact test in the
CGNAT Docker fixture. The pinned compiler base image lacks `ffmpeg` and
`ffprobe`; S2 is preparing a separate test-only public Debian FFmpeg derivative
image. Verify both executable prerequisites and Rust 1.97.1 in that derivative;
do not replace the compiler base image or production environment. Require one executed test, zero ignored, and an actual
result. Diagnose failures; do not replace the physical actor, manufacture a
locator, write readiness rows or alter authorization to make it pass.

**Next extensions:** actual queued/backpressured B HTTP/1 and HTTP/2 writers;
End/revoke/logout/off while writes are parked; encoded and Native text lanes;
positive ordered progress through the real endpoint; exact retries after
response loss; bounded memory and actual FD/process cleanup observations.

**Acceptance:** B End cannot precede actual B writer/read-job closure and
actual Source settlement. Record Source and B build revisions, actual bytes,
known lineage, accepted transport closure and Source `Some(Ok(()))` settlement.

### 3.2 Integrate Source control/status and filesystem custody

Full-merge `6b8ef28b4` or its qualified descendant, preserving `9ab93d6c1`
controls and the full `9a9109602` merge. Its finite evidence is status 7 in
27.60 s (six physical actor cases plus the strict client case), controls 6 in
27.63 s, Local controls 2 and Local status 1; check 54.81 s; denied Clippy
94 s; normal hook 111 s. Existing `/status` remains the complete live Start
envelope used by Root publication and renewal.

S3's uncommitted custody slice owns `source_actor.rs`, new
`transcode/source_resource.rs`, actual actor tests, `vod/serve/delivery.rs`,
`vod/serve.rs` and its catalog row. Its new Source-only read context retains
the real counted response guard inside a blocking no-follow cache-file
open/metadata job. Cancelling the HTTP waiter cannot release that job's file
descriptor or custody. A late joined read must not renew authority or activity.

The custody filter passed five tests in 15.81 s, zero ignored, before the
remaining suite/check/hook. Three are actual init/media/cancellation/deadline
FD cases; two are strict resource-client tests. Logs:
`/private/tmp/sharing-source-resource-{copy,encoded,native,status,controls,local-etag,local-wait,local-init,check,clippy}.log`.
Do not claim a clean checkpoint until the current command and normal hook
finish. Merge the current Root base and requalify that exact tree.

**Acceptance:** cancellation and deadline tests retain actual FD/job custody
until join, close the descriptor before physical settlement, and leave viewer
activity unchanged after the request deadline. Local resource/control/status
behavior remains covered by its affected existing regressions.

### 3.3 Build Source adapters and B status/control translation

Re-verify these interfaces against the current code before implementing:

| Operation | Fixed Source path / body | Required response |
|---|---|---|
| Existing live status | `/sharing/v1/items/{item}/files/{file}/sessions/{SourceReq}/status` | Existing complete Start envelope and actual response guard; leave this protocol unchanged |
| New VOD observation | Same prefix ending `/vod-status`; original whole `{reference,session,incarnation_id,session_id,control_epoch}` | Exact full Source echo plus `status: SourceVodStatus`, guarded through the accepted writer |
| New control | Same prefix ending `/control`; same original envelope plus `control: ControlRequestV1` | Exact full Source echo plus bounded complete `response: ControlResponseV1`, guarded through the accepted writer |

S3 owns Source adapters and pure projection from the actual retained normalized
`SessionRequest`/Start/route. Never serialize `LocalControlResult` or raw
`VodSessionInfo`: numeric Source file IDs and raw failure prose do not belong
in the Shared status/control wire.

Root implements strict bounded clients, actual upstream socket custody,
immutable lineage checks and independent ownership of sent controls. Validate
B's actual session/incarnation/epoch before translating to the received Source
session/incarnation/epoch. Keep sequence, client identity, capabilities and
frozen original desired selection. Observe original B authorization and exact
binding before and after parked Source IO. Rebind the accepted response and
ownership fields to B; Source URLs or prepared sessions cannot escape through
an unqualified successor response. Reject malformed/duplicate/foreign fields.

Agreed B status contract at `GET /api/v1/hls/{Bsid}/status`:

```json
{
  "subject": "shared",
  "reference": {
    "item": "<full SharedReference object>",
    "file_id": "<canonical SourceId string>",
    "revision": "<64 lowercase hex FileRevision>",
    "lifecycle_generation": "<positive integer>"
  },
  "session_id": "<canonical B UUID>",
  "incarnation_id": "<canonical B UUID>",
  "control_epoch": "<positive integer>",
  "status": "<bounded SourceVodStatus object>"
}
```

Placeholders above describe types, not literal wire values. Native validation
binds every outer field to its retained `SharedStartedPlayback`. Metrics omit
`file_id`, `id`, `producer_failed` and raw prose. The outer `file_id` is the
opaque Source string already present in the file context, not a Local ID.

**Acceptance:** actual Copy/encoded/Native pause/play/seek and same-sequence
retry through B, wrong B/Source lineage refusal, authorization loss during a
parked exchange, honest safe status metrics, and no activity mutation from
status alone. Apply renderer controls only after server acceptance.

### 3.4 Integrate native checkpoint and enable the public journey

Full-merge qualified `ecebec1d6` or its descendant. Do not repeat the known
Apple presenter-test race: the pure test now constructs AppModel without
starting unjoined discovery/bootstrap services; production defaults remain
unchanged and original-account guards stay strict.

After the physical relay and wire acceptance above, install the candidate
Start route, replace unconditional unavailable detail delivery status with
fresh supported-launch availability, and run the complete browser/native
journey. Availability is launch capability, not fabricated producer readiness.

**Acceptance:** settings → invitation/approval → assignment → browse → fresh
details → Start → real media → controls/status → ordered final progress → End.
Account replacement, stale detail and unsupported selection fail explicitly;
no Local fallback, fake IDs, saved-choice override or implicit autoplay.

## 4. Finish the remaining server behavior, not only the first Copy lane

| Remaining work | Required implementation/evidence |
|---|---|
| Direct and progressive delivery | Complete Shared file/prefix relay with actual Source custody, conditional/Range semantics and 416 behavior; public routes must stay within the signed B context |
| Directed quality/audio/subtitle replacement | Source-owned prepared successor and complete B successor mapping; exact cancel/commit/replay, stale prepared response refusal, shared budget/cap accounting, source-position preservation |
| Burned subtitles and artifacts | Actual font/attachment/bitmap/text ownership and cleanup; prove burn output and safe artifact serving rather than inferring support from metadata |
| HDR/Dolby Vision | Actual source/output behavior, capability policy and SDR tone-map cases; initial non-null HDR/DV asks are currently refused, so these paths remain open |
| Failed or ambiguous Source Start | Private per-invocation negative-admission factory; exact owned task/join and g0/g1 cleanup proofs. `31848626f` retains task stages but does not turn errors into NoAdmission |
| Fresh first-dispatch proof | Root creates a new private Source request UUID and forbids a second dispatch. If used for negative admission, mint a private proof for that actual fresh invocation; never reconstruct it from a durable recipe or adopted claim |
| Crash/restart settlement | Prove actual child/FD/socket cleanup across process death and restart; persist or otherwise establish genuine completion. Row absence, expired lease, reservation deletion, lost cache or `kill_on_drop` assumptions are insufficient |
| Source worker forwarding | Non-owner ingress authenticates the sharing principal and reaches the actual assigned worker with exact admission/lineage and physical proof |
| B ingress/owner transition | Relay to the actual receiver owner; eligible placement, secret unwrapping/Tailscale readiness, exact takeover epoch and same upstream reconciliation; no SQL-only physical actor adoption |
| Endpoint/pin changes | Qualify authenticated endpoint updates and allowed rotation, preserve cleanup reachability, reject arbitrary replacement pins and unauthenticated hints |
| Cluster revocation and limits | Real three-voter stale/minority/quorum/member-loss cases; four per-grant/eight Source-wide slots include preparation; memory/body/request limits cover all lanes and nodes |
| Catalogue/history/artwork completion | Audit the existing implementation against the full S4 matrix: huge IDs, same IDs across sources, live keyset paging/sort movement, deletion/epoch changes, cursor substitution, cache denial/bounds, global progress ordering, manual unwatched and next-episode reauthorization |
| Upgrade/restore | Historical actual binaries, active sessions and interrupted upgrade/restore; preserve supported coordinated-upgrade boundaries and disable/re-pair after restore/clone. Existing receipts cover bounded Store/empty-workload cases |

Audit the existing cross-platform fixture
`tests/sharing/protocol-cases.json` against every §13.1 case in
[the implementation contract](SHARED-LIBRARIES-IMPLEMENTATION.md). The file
exists; its existence alone does not establish complete Rust/web/Swift/Kotlin
parity or coverage of the new wire.

**Acceptance:** each observable fix carries a focused regression that fails
on the prior behavior, executes a nonzero count and is recorded in the task PR
and landing commit. Keep physical proof, metadata authority and wire-shape
validation distinct in every receipt.

## 5. Finish clients and actual watching

### 5.1 Native finite evidence is available; wire and devices remain open

The final native code bytes are archive `1d5354f81`; `ecebec1d6` adds its
qualification receipt. Archive:
`/private/tmp/plurx-sharing-s4-native-final-source.tar`, SHA-256
`8316e1e948d19f28fb666ebe403f19ec32e775b6aaece39f72d6067dc18fd79f`.

| Evidence | Result / log |
|---|---|
| Android main/unit/instrumentation Kotlin compilation and focused tests | 47 passed; zero failure/error/skipped; lint passed in 5 min 52 s; `/private/tmp/sharing-s4-native-final-android.log` |
| Combined iOS tests | 359 passed; zero failures; `/private/tmp/sharing-s4-native-player-combined-ios-fixed.log` |
| Combined tvOS tests | 349 passed; final `TEST SUCCEEDED`; `/private/tmp/sharing-s4-native-final-tvos.log` |
| iOS/tvOS Release simulator compilation | Both `BUILD SUCCEEDED`; `/private/tmp/sharing-s4-native-final-ios-release.log`, `/private/tmp/sharing-s4-native-final-tvos-release.log` |

These are compilation/simulator/protocol receipts, not physical watching.
The tvOS wrapper completed after its verified idle diagnostic collector was
stopped; test results were not altered. Root must integrate the actual branch
and qualify affected behavior on the final intended base.

Remaining S4 client work:

1. Shared-only status DTO, original-account client and safe metric rendering
   using the agreed outer grammar; never decode Local status with fake IDs.
2. Owned Void Start-task join to remove the new non-Sendable result warning,
   without unchecked Sendable claims or abandoned Start ownership.
3. Server-accepted current-rendition seek/pause/play with exact B tuple,
   ordered control sequence/client ID and frozen raw desired selection. Auto
   raw height must survive; Manual raw 144 cannot be replaced with delivered 72.
4. Directed quality/audio/subtitle/recovery after server successors qualify.
   They remain explicitly unavailable; default renderer controls are hidden
   while their server wire is unfinished.
5. Complete web control/status and actual player journey using Shared file
   contexts, scoped history and Continue Watching, preserving Local callers.
6. Browser, iPhone/iPad, Android phone, physical Apple TV and physical Google TV
   playback, subtitle/seek/control/stop/account-loss observations.

**Acceptance:** actual supported playback on the named devices and build
revisions, no Source token/URL exposure, no Local ID/history/recovery paths,
ordered exact progress retries and actual stop cleanup.

### 5.2 Run the complete live S2/S8 matrix

The user has not supplied the two Cinema server addresses or the Apple/Android
hardware targets. A question is pending asking which two servers/devices to
use and whether both servers already run Tailscale. Do not invent endpoints,
request credentials in chat, or claim real NAT/DERP/device evidence from Docker.

Run the complete §13.3 matrix, including:

- Two separately NATed homes, direct and forced-relay paths, supported shared
  or restricted common-tailnet account arrangements and reciprocal grants.
- Source/recipient Tailscale stop, unrelated tailnet user, and recipient device
  without a Cinema credential; wrong pin/port/route, redirects and LAN denial.
- Linux bare-host Serve and Docker 28+ bridge/loopback/host egress, startup
  ordering, node-key expiry, certificate renewal/expiry and firewall isolation.
- Standalone A/B, clustered A, clustered B, both clustered, serving-node loss,
  quorum/minority loss, authenticated endpoint addition and eligible takeover.
- A revoke/library removal, B assignment removal/logout/user deletion and
  feature off during long direct responses and HLS playback.
- High-bitrate direct/relay playback, bounded slow-reader memory, CPU and active
  encoder counts, aggregate slot/preparation limits across nodes.

Measure 10-minute sustained playback per transport path: actual bitrate and
throughput, first frame, stalls, seek drift, Source/B bytes, peak relay buffers,
CPU and active encoders. Each revocation drill records commit time, authority
lease expiry, last new A byte, last new B byte and cleanup time. Both serving
bounds must be at most 30 seconds; already-buffered playback is excluded.

**Acceptance:** sanitized reproducible receipts name Source/receiver/client
builds, topology, media, transport, commands and results for every required
matrix cell. Unsupported platforms remain explicitly unqualified.

## 6. Compiler and resource discipline

Use separate target directories for concurrent agents. Root's loop is:

```bash
cd /private/tmp/plurx-shared-libraries-s3
export PATH=~/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH
rustc --version # Must report 1.97.1, not Homebrew 1.98.
export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2
export CARGO_TARGET_DIR=/private/tmp/plurx-shared-libraries/target
cargo check -p plurxd -p plurx-core --all-targets \
  --features plurx-core/hiqlite-store,plurx-core/hiqlite-contract-tests
cargo clippy -p plurxd -p plurx-core --all-targets \
  --features plurx-core/hiqlite-store,plurx-core/hiqlite-contract-tests -- -D warnings
```

Focused commands; choose the affected filter, then inspect its actual count:

```bash
cargo test -p plurxd --bin plurxd http::shared_receiver_
cargo test -p plurxd --bin plurxd http::shared_source_playback::
cargo test -p plurxd --bin plurxd source_copy_
cargo test -p plurxd --bin plurxd source_encoded_
cargo test -p plurxd --bin plurxd source_native_
cargo test -p plurxd --bin plurxd source_resource_
cargo test -p plurxd --bin plurxd source_peer_guard_survives_body_eof_and_drops_after_actual_socket_close
cargo test -p plurxd --bin plurxd source_preparation_closes_actual_parent_descriptors_before_settlement
cargo test -p plurx-core --features hiqlite-store,hiqlite-contract-tests --lib \
  sharing_receiver_source_binding_publication_and_renewal_are_guarded_and_exact
cargo test -p plurx-core --features hiqlite-store,hiqlite-contract-tests --lib \
  sharing_receiver_pending_retirement_requires_exact_claim_and_absent_resources
cargo test -p plurx-core --features hiqlite-store,hiqlite-contract-tests \
  --test store_contract \
  sharing_receiver_pending_retirement_three_voters_refuses_takeover_and_ignored_writes
python3 -m unittest tests.operations.test_docs_index
make validation-lint
```

The daemon test binary emits the known Darwin oversized unwind-table linker
warning. It is not a denied Rust Clippy warning or a failed regression; record
actual exit status/count instead of silently treating warnings as failure.

Commit normally with `git -c core.hooksPath=scripts commit ...`. The tracked
hook checks catalog, formatting, workspace Clippy and served JavaScript
syntax. It does not replace focused tests or compile-only effort evidence.
Do not edit Rust while a compiler/hook is qualifying its frozen tree.
Requalify the exact intended base after integration.

**Docker:** coordinate S2/S4 exclusive memory-heavy compilation windows. Do
not reserve an idle window before a committed archive is ready. Leave the
unrelated `plurx-jellyfin-j0-reference` container untouched.

- Rust image `codex/plurx-sharing-compiler:1.97.1`; verify its compiler pin.
  CGNAT network `plurx-sharing-s4-352b-cgnat`, subnet `100.127.89.0/29`.
- Proven Linux memory wrapper:
  `/private/tmp/plurx-sharing-linux-memory-wrapper.sh`; previous runner
  `/private/tmp/plurx-sharing-b-alias-memory.sh`. Inspect and adapt the exact
  new committed test/archive; an older mock fixture is not physical evidence.
- Compile source-only with a warm target; never transfer `.git` or repository
  credentials. Preserve OOM/memory event receipts and nonzero test count.
- Android image `plurx-android-build:latest`, own public cache volume
  `plurx-sharing-s4-native-gradle`, container
  `plurx-sharing-s4-native-android`, 4 CPUs/6 GiB, linux/amd64. The previous
  batch is released; S2 has reserved the next physical CGNAT window. Recheck
  before using it.

S4's exact Android command was:

```bash
./gradlew --no-daemon --max-workers=2 \
  :app:compileDebugKotlin :app:compileDebugUnitTestKotlin \
  :app:compileDebugAndroidTestKotlin :app:testDebugUnitTest \
  --tests 'tv.plurx.app.data.Shared*' \
  --tests 'tv.plurx.app.data.PlaybackFileContextTest' \
  --tests 'tv.plurx.app.player.PlayerPolicyTest' \
  --tests 'tv.plurx.app.player.PlayerSurfaceAndClampTest' :app:lintDebug
```

Apple DerivedData: `/private/tmp/plurx-sharing-s4-apple-derived`.
Generated `clients/apple/plurx.xcodeproj` is ignored. Use current available
simulators rather than assuming a saved UUID exists on another machine.
Previous iOS destination was `77C904FB-2DF2-4959-A37A-A97F6659E168`; tvOS was
`C39F9DCC-4EC5-4806-A5D9-5D13710F7F2F`. Schemes `plurx-iOSTests` and
`plurx-tvOSTests`, Debug, jobs 2, `CODE_SIGNING_ALLOWED=NO`; Release used each
platform's generic simulator destination. Preserve retained stash
`d74840972663dbdb0f1337372caaa496e30135e3`: it was already applied, so do not
pop or drop it as cleanup.

## 7. Finish review, operator documentation and promotion

S1 landed into the effort through PR #746; its landing receipt names
`971265536a`. S2 draft PR #759 is
`http://forge.lan:3000/noirr/plurx/pulls/759`; its previously observed gate
passed, but live Tailscale/hardware qualification remained open. Do not infer
merge permission or current gate success from that old observation.

The local `effort/shared-libraries` ref at handoff is
`9a719fcb73da195676a6dea5378871d64547bb8b`; it may differ from remote/current
main. Refresh actual refs and review context before porting unpublished work.
Task branches must target the current effort. This project has overlapping
files, so the disjoint-file exception for direct-main task branches does not
apply.

Remaining release work:

1. Port each reviewable task to the current intended effort base, resolve
   integration conflicts without dropping either receipt, and requalify that
   exact tree before pushing. Use `fix(` or `perf(` for observable behavior.
2. Open/update task PRs with the executed focused commands and one
   `Regression-Test: <path>::<test name>` per regression. Wait for the blocking
   Effort development gate. Preserve those lines in landing commits; Forgejo
   API merge requires `MergeMessageField` explicitly.
3. Audit/update operator/API/security/client/features documentation as behavior
   becomes implemented. Cover network setup separately from Cinema pairing,
   rotation/revocation, readiness/error diagnosis, restore/re-pairing, resource
   limits and the actually supported upgrade procedure.
4. Keep the Developer switch user-controlled with advisory readiness throughout
   development. When fully active and tested, graduate it according to
   [the Developer lifecycle](SETTINGS-NAVIGATION-AND-DEVELOPER-STATUS.md).
5. Freeze task merges when the effort is complete, merge current main into the
   effort, and qualify the new candidate. Open effort → main only with the
   complete qualification receipt and passing current Main promotion gate.
   Any base movement invalidates the old promotion receipt.

**Acceptance:** all S1–S8 obligations have implementation and appropriately
scoped evidence, the current candidate passes its required gates, regressions
are recorded in task and landing history, and operator docs describe actual
behavior. Do not mark the feature complete because a task gate passes.

## 8. Proof boundaries to preserve during every remaining change

- No Local user/file/item sentinel, Source-principal impersonation, forged
  signed locator, synthetic producer, manual readiness row or DDL shortcut.
- No Source credential in browser/native state, URLs, ordinary cluster RPC,
  logs, snapshots, handoff text or compiler archives.
- No physical settlement from body EOF, requested abort, SQL absence/ended
  state, lease/TTL expiry, cache eviction, reservation deletion or restart.
  Retain unresolved ownership until actual independently owned joins and
  the exact physical Source receipt justify retirement.
- No abandonment of an already-sent Source Start/control/open when the B HTTP
  waiter cancels or original authorization disappears. Seal new admission,
  keep custody, join, then settle.
- No durable-recipe adoption as a physical handle during failover. Reconcile
  the actual original producer and full lineage or remain unavailable.
- No capability/clock extension before immutable request/route validation;
  parked IO requires re-observation. Status must not mutate viewer activity.
- No broad shared library fallback through Local, legacy/Plex, admin or
  cluster routes. Preserve complete compound references and original login.
- No claim of real Tailscale/NAT/DERP, hardware, HDR/audio or production upgrade
  evidence from codec labels, simulator builds, Docker addresses or mocks.

These boundaries explain why the remaining qualification is substantive.
They are not optional cleanups to postpone until after promotion.
