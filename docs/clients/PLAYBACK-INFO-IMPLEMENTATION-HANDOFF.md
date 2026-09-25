# Playback info build — report the attached stream and explain missing output facts

**Status:** open — build contract for Sol; implementation not started.
**Written:** 2026-09-16. **Executes:** the revised
[missing-fields RCA](PLAYBACK-INFO-MISSING-FIELDS-RCA.md), including its
B1/S1–S3 review dispositions.

Read the RCA for evidence and this document for the implementation sequence.
Build the packages in order, retaining the existing player ownership and
playback behavior. The user requested a handoff to implement these repairs;
routine implementation decisions within this contract do not need another
design round. If a collector requires changing playback routing, a custom
audio renderer, or the strict playback-control protocol, keep its explicit
unavailable state and return that expansion for separate review.

This file is sufficient to start work, but the linked RCA must travel with
it: it contains the reproducible defect and the qualified Android finding.
Neither document is a claim that code has been built, reviewed or deployed.

## 1. Deliver these outcomes

| Situation | Required result |
|---|---|
| 4K source transcoded to 1080p, master still says 3840×2160 | The stream row uses eligible output metadata, or explains its absence. It never reports the master's source dimensions as the converted stream. |
| Native HLS or direct/progressive playback | Available server or attached-item metadata can populate Stream format without an hls.js instance. |
| Known output codec and height, unknown width | Show `H.264 · height 1080`; do not invent width or a progressive/interlaced designation. |
| Device output reporting missing | Explain whether collection is unimplemented, unsupported, pending or failed. A permanent generic `Not reported` is not the entire answer. |
| Native platform reports a route | Show the route with its limits. A track's channels/codec never prove speaker or receiver output. |
| Quality change, prepared replacement or Live TV channel change | Values follow the attached media. No old response or observer can restore predecessor facts. |
| Android already reports correct sample dimensions | Preserve them and prove the behavior. Do not introduce a copy-only restriction on genuinely sample-derived dimensions. |

Keep the labels `Stream format` and `Device audio output` and their existing
internal IDs `stream_format` and `device_audio`. Keep persisted modes
`mini`, `standard`, `details`, `debug`. Details and Diagnostics retain the
two rows; Overview and Compact keep their existing essentials. This is a
data-and-explanation repair, not another panel redesign.

**2026-09-25 visible-field ruling:** the later dimensions repair gives
`stream_frame` sole ownership of displayed stream dimensions in all four
modes. `stream_format` remains the internal server descriptor name, but its
visible row contains codec, scan and cadence only. Planned or sampled
dimensions from that descriptor feed `stream_frame`, with their evidence
basis shown next to the value. This ruling supersedes dimension strings in
the visible formatting examples below; it does not rename the wire object.

## 2. Establish the base, ownership and compiler loop first

### 2.1 Build from fresh main in an isolated checkout

The document-writing checkout is dirty at `10f2afe60`; it predates the
redesign. Do not build there, reset it, stash unrelated work, or copy its
entire web file into the implementation. The evidence anchor is
`5d4235d5e1cfb1276aff224e6d6d8f62b40b91ed`. The latest **locally known**
`origin/main` when this handoff was written is
`c9e4edf451e12247a7aa4188903e5ba36888e7e9`; this is not a substitute for
fetching the current integration base when execution starts.

Use one effort because the packages share DTOs, player files and fixtures:

```bash
git fetch origin main                         # Refresh the intended base.
git worktree add -b effort/playback-info-facts \
  /private/tmp/plurx-playback-info-facts origin/main
git -C /private/tmp/plurx-playback-info-facts rev-parse HEAD
```

If that branch/worktree already exists, inspect and resume its recorded
work instead of replacing it. Subsequent commands run in that checkout.
Import only this handoff and the RCA, and merge their index rows into the
fresh index. Do not overwrite fresh main's index with the dirty old one.
Both documents and their index entries belong in the same bootstrap commit.

Read the fresh [AGENTS.md](../../AGENTS.md),
[docs index](../README.md), and
[development pipeline](../DEVELOPMENT_PIPELINE.md). Record the fetched base,
working branch, toolchain versions and exact test commands in §12 as work
progresses. The anchor's line numbers are discovery aids; re-resolve symbols
on the build base before editing.

### 2.2 Verify tools before any Rust edit

```bash
rustup show active-toolchain                   # Confirm the repository pin.
rustc --version                               # Expected pin: 1.97.1.
cargo --version
node --version
python3 --version
```

If Rust cannot run on the checkout host, establish the
[source-only compiler loop](../ci/AGENT-COMPILE-LOOP.md) now. Use committed
`git archive` source; never transfer `.git` or a repository credential.
Keep the compiler's target directory warm. After integration onto a changed
base, archive the exact intended branch and repeat the required checks.

For native work, locate the existing Xcode and Android SDK/JDK environments.
Use the pinned Media3 dependency rather than updating it. At the evidence
anchor it is 1.10.1. A missing physical device does not prevent the server
and formatter packages, but it must remain an explicit limitation on any
route or input-format claim that needs that device.

### 2.3 Packages run sequentially with explicit file ownership

| Package | Owner surface | Shared files it may change |
|---|---|---|
| M0 — baseline | Handoff record and fresh-base discovery | This document; docs index only for importing the two documents |
| M1 — fact contract and failing regression | Shared field fixture, web pure resolver/formatting tests | Field fixture, contract generator, generated web table and input-contract documentation |
| M2 — server descriptors | HLS start/status, direct decision, transcode and cached-VOD models | Server DTOs, focused Rust tests, API documentation |
| M3 — web consumers | Library/Live TV telemetry, attach/replace lifecycle, existing panel | Web player, web policy/helper modules and web regressions |
| M4 — Apple | Models, player controllers, shared info panel and Live TV adapter | Apple tests and generated project inputs if required |
| M5 — Android | Models, controllers, shared info panel, Live TV and offline adapters | Android tests; existing integration points only |
| M6 — integration | Mixed-version proof, device evidence, final docs and release counters | Shared contract reconciliation, this ledger, current behavior docs, required native version records |

Task branches use `codex/playback-info-m1`, etc., based on the current
effort; their PRs target the effort. Do not use independent main-bound
branches: these packages have overlapping files. These are work packages
for Sol, not instructions to create extra agent tasks or parallel writers.

## 3. Fix the provenance rules before filling empty fields

### 3.1 Mandatory corrections from review

1. The server's inspected master builder publishes source dimensions when
   present. An active hls.js level is therefore **not inherently output
   metadata**. On converted or unknown video action, reject these dimensions
   even when nonempty.
2. The review's identical Android failure is unproven. Pinned Media3 has a
   single-variant/sample merge path that retains sample dimensions. Trace
   actual `player.videoFormat` provenance and add a real 4K→1080p case.
3. Native HLS is not AirPlay-only. The current policy includes copied HEVC
   and missing hls.js support. Preserve that policy.
4. Live TV's calculated width is server-planned output, derived from source
   aspect and selected height. Label it as planned, and check that it agrees
   with the encoder's actual scaling and rounding.
5. Reuse existing session codec and height producers. Do not fix the info
   row by changing HLS `RESOLUTION`, `CODECS` or `VIDEO-RANGE` attributes.

### 3.2 Resolve a coherent observation

```text
 attached item/rendition identity matches?
      no ──▶ discard observation
      yes
       │
       ▼
 sample-derived format, verified for this collector?
      yes ─▶ eligible player-input observation
      no
       │
       ▼
 source-shaped manifest dimensions + verified video copy?
      yes ─▶ eligible manifest declaration
      no
       │
       ▼
 matching server output descriptor available?
      yes ─▶ eligible server metadata; planned dimensions labeled
      no  ─▶ explain missing output metadata
```

Apply the test to each candidate's field provenance. The same sample-format
object may contain dimensions copied from a manifest and a codec obtained
from the media. Either select one coherent candidate or retain provenance
per component. Never combine rendition A's dimensions with rendition B's
codec. Never treat the requested quality, source size, or display size as
the missing output answer.

## 4. Client observation and display contract

Implement a small local value type in each client; do not build a general
telemetry framework. The RCA's proposed observation model supplies the
shape. The following decisions make it executable:

| Property | Build rule |
|---|---|
| `state` | `known`, `pending`, `unavailable`, or `not_applicable` |
| `value` | Present only for `known`; at least one valid, eligible fact required |
| `provenance` | `player_input`, `manifest`, `server_delivery`, `platform_route`, or `platform_session`; never inferred from the UI label |
| `reason` | Required without a current value: `not_implemented`, `unsupported`, `metadata_missing`, `source_metadata_only`, `not_attached`, `refreshing`, `query_failed`, `permission_required`, `no_audio` |
| `attachmentKey` | Existing local attachment identity plus session/item and rendition where applicable; not a new server protocol |
| `observedAtMonotonicMs` | Optional local read time; do not present it as the server's probe time |
| dimension basis | Preserve whether dimensions came from samples, an eligible declaration, or a server recipe; recipe dimensions display as planned |

Normalize missing/empty codec text, nonfinite numbers, negative values and
zero dimensions to unknown. A dimension pair requires two positive values.
A positive height alone is still useful and is labeled as height. Do not
silently discard known height because width is unknown. Pixel dimensions
must be integers. Unknown enum values degrade locally; they never prevent
playback or activate a recovery action.

| Normalized observation | Visible value / note |
|---|---|
| Eligible codec and pair | Stream frame `1920×1080`; Stream format `H.264`; each keeps its own provenance note. |
| Codec plus height only | Stream frame `Height 1080`; Stream format `H.264`; width stays unknown. |
| Codec only | Stream frame `Unavailable`; Stream format `HEVC`. |
| Pair only | Stream frame `1920×1080`; Stream format `Not reported`. |
| Planned server dimensions | Stream frame uses the same value formatting with `Planned output`. |
| Attach or bounded collection outstanding | `Waiting for stream` or `Refreshing output information` |
| Only source-shaped dimensions on a transcode | `Output metadata unavailable`; `The available dimensions describe the original file` |
| Collector not built | `Not available in this version`; collector-specific explanation |
| API cannot expose requested fact | `Unavailable in this player`; precise platform limitation |
| Platform query failed | `Output information unavailable`; no playback-error styling |
| Confirmed audio-free stream | `No audio stream` |
| Route known, format unknown | `HDMI`; `Output format unavailable` |

`pending` is allowed only while real work is outstanding. A callback or
query that completes without metadata becomes unavailable. No indefinite
spinner or automatic metadata retry loop. A paused player is not an
audio-free stream. A remembered route with invalid current evidence must
not be presented as a current observation.

For audio, keep route and session facts separate internally when their
timestamps or validity differ. If showing a session channel count, label
it as session channels, not a 5.1/7.1 receiver layout. No Atmos, passthrough,
receiver codec or spatial-audio claim may be inferred from track metadata.

## 5. Server wire contract — additive metadata, unchanged control protocol

### 5.1 Reuse these existing owners

Re-verify these symbols on the build base:

| Source | Existing fact / responsibility |
|---|---|
| [transcode.rs](../../crates/plurxd/src/transcode.rs), `StartInfo` | Actual created session, normalized `target_height`, `kind`, encoder and VOD state |
| Same file, `HlsContext`, `transcoded_hls_codecs`, `copied_hls_codecs` | Existing session codec producers; some values are recipe-derived, not freshly inspected |
| [http/hls.rs](../../crates/plurxd/src/http/hls.rs), `StartResponse` | Session-start wire response; `height` and `encoder` already exist |
| Same file, start/replay and staged-successor `StartResponse` construction | Canonical response JSON retained for replay and recovery |
| Same file, `status_local_before_with_relay` | Existing status request, route-authority recheck and attempt-publication guard |
| transcode `SessionInfo`, `HlsSessionInfo`, `hls_session_status_publication` | Rolling and cached status publication |
| [vodserve.rs](../../crates/plurxd/src/vodserve.rs), `VodSessionInfo` | Cached output's status facts |
| [http/stream.rs](../../crates/plurxd/src/http/stream.rs), `DecisionResponse`, `DeliveryPlan` | Direct/progressive decision metadata and execution plan |
| [live_tv_delivery.rs](../../crates/plurxd/src/live_tv_delivery.rs), `LiveDeliveryPlan.output` | Existing Live TV codec/dimensions/action; reuse its planned output |

Do not reimplement a codec calculator in the HTTP layer or each client.
The session codec string may contain both video and audio entries. Extract
the eligible video entry using the existing codec vocabulary; do not turn
an arbitrary first token into a verified video codec. Audit the copied-codec
fallbacks identified by the RCA before exposing them. Unknown remains
unknown; do not repeat an AAC default as a measured audio fact.

### 5.2 Add the same optional descriptor to existing responses

Use `stream_format` on these response envelopes:

- `StartResponse` for the session actually created, including the existing
  canonical replay response and staged successor records.
- Existing rolling/cached HLS status JSON, inside the same publication
  boundary as the other status facts.
- `DecisionResponse` for verified direct/progressive video delivery only.
  A transcode preflight decision has no created output and omits it.

Use this shape, with fields omitted or null when unknown:

```json
{
  "stream_format": {
    "video_codec": "avc1.640028",
    "width": null,
    "height": 1080,
    "video_action": "encode",
    "dimensions_basis": "recipe"
  }
}
```

The example is an illustrative planned descriptor, not a measured stream.
`video_codec` accepts the existing validated video codec family or RFC 6381
token; it never accepts policy words such as `source` or `server_selected`.
`video_action` is `copy` or `encode`, from the actual created recipe.
`dimensions_basis` is `recipe`, `source_copy`, or `sample`:

- `recipe`: output dimensions the effective encoder recipe establishes.
- `source_copy`: source dimensions explicitly retained by this delivery's
  unchanged video selection.
- `sample`: an existing media-format observation establishes coded size;
  do not add a media probe to populate it.

If `video_action` is unknown, omit it and do not authorize source-shaped
manifest dimensions. If a dimension basis is unknown, the descriptor cannot
supply a trusted dimension; known codec may remain useful. The entire object
may be absent. Native decoders must tolerate an absent, null or malformed
optional object without rejecting the enclosing playback response. Handle
invalid individual fields locally; do not loosen validation of unrelated
control/session fields or disable strict decoding globally.

The nested `height` uses the same effective value as existing top-level
`StartResponse.height`, not a second calculation. A zero top-level height
normalizes to absent nested height. A disagreement for the same created
response is a server test failure. Until fixed, clients must not select
one conflicting value by guesswork.

For direct/progressive delivery, set `copy/source_copy` only after the
server confirms that the selected delivery preserves video dimensions.
Audio conversion alone does not imply video encoding. A source codec token
whose representation changes during remux may be omitted if no appropriate
output token is already known; dimensions can still be eligible.

### 5.3 Prepared handoff uses existing status, not a control-v1 extension

`ControlResponseV1`, `EffectiveSelection` and control actions are strict
contracts. `EffectiveSelection.codec` accepts policy values `source` and
`server_selected`; it is not a media codec string. **Do not add
`stream_format` to these types or reinterpret that field.** No protocol
version, capability negotiation or strictness change belongs in this work.

Store any known descriptor with the successor's existing start-response
record, but do not assume the client receives that record in a prepare
action. While preparing, keep the candidate's facts separate. After commit,
the new attached player may report input metadata; the existing status
request for that successor supplies server metadata when available.
Before that, explained missing output metadata is correct. Do not retain
the predecessor tuple just to avoid a blank row.

This choice intentionally avoids a new endpoint or metadata polling loop.
If a client/path has no existing status samples, use its attached-player
collector and honest partial/missing values; record the limitation. Do not
add status polling just because the info panel opened.

### 5.4 Preserve publication, replay and cached-output boundaries

Populate descriptors from already-owned recipe/session/cache facts. Status
metadata must travel through the existing owner/attempt publication guard.
Do not make a separate unguarded store lookup after the status was admitted,
or bypass authorization to read media metadata.

Capture the descriptor in the existing canonical start response when known.
Replaying an idempotent create returns that same response. Later status can
carry an improved observation from the same current session; do not mutate
the durable replay answer just because inspection completed later.

Cached VOD must describe the cached output artifact/recipe, not today's
source-file dimensions. Staged placeholders that have not created an
encoder may expose only explicitly planned facts they actually know.
An encoder name such as `transcode` does not prove a particular codec.
Old durable records without the descriptor remain readable. Use existing
JSON extension points; no new database table or schema migration is needed.

Do not derive output width from assumed 16:9. Recipe-known width is eligible
only when its measured aspect, sample-aspect handling, scale/crop operation
and rounding agree with the output recipe. Keep unknown width absent.
Live TV already has such a planned width calculation; verify encoder
agreement and label the plan rather than converting it into a measurement.

## 6. Package instructions and acceptance

### 6.1 M0 — baseline and fresh-base map

**Work:** import the two documents, resolve the symbols in §5 and the client
map below, and establish the compile environment. Inspect current native
JSON decoding and server response relays for additive-field tolerance.
Record every start, replay, cached and replacement response producer.

Run the RCA's seven-case reproduction. Its final `3840×2160` output proves
the defect remains reproducible; it is not the desired acceptance result.
If fresh main already fixes a finding, verify it and omit redundant code.

**Acceptance:** exact base recorded, compiler works before Rust edits, dirty
original checkout preserved, and a producer/consumer map covers each §5.2
response. Any changed upstream behavior is recorded before implementing
against stale assumptions.

### 6.2 M1 — shared contract and behavior-first regression

**Work:** add a pure stream-format resolver and small display formatter in
the existing web policy/helper structure. Feed it normalized candidates,
video action and attachment identity; keep it independent of DOM access or
network calls. Add equivalent test vectors to the shared playback fixtures.
Use a narrowly named fixture under `tests/playback/` if the field-list
fixture cannot express values without changing its established grammar.

Add a regression using the production telemetry/adapter path: source and
level 3840×2160, attached created output height 1080, action encode. Expected
output must exclude source dimensions. Demonstrate failure against the
pre-fix code before wiring the resolver. A test of a new helper alone does
not prove the shipped adapter stopped trusting the level.

Represent states/reasons in the canonical
[field fixture](../../tests/playback/playback-info-fields.json) or a linked
value fixture; update the
[generator](../../scripts/player-contract-table) only as needed. Regenerate
the web embedded table and input-contract documentation. Preserve existing
row IDs, mode membership and diagnostic reachability.

**Acceptance:** wrong-provenance, unknown, partial-height and stale-identity
cases have executable assertions; code consumes them; generated copies
agree. The focused regression rejects the original bug and passes with the
resolver connected. Do not leave an intentionally failing commit as a
mergeable package.

### 6.3 M2 — server descriptor producers and compatibility

**Work:** implement §5 using a shared descriptor builder near the existing
session metadata owners. Add it to all mapped ordinary and staged
`StartResponse` constructors, canonical replay serialization, rolling and
cached status, and eligible direct/progressive decisions. Add optional
fields with appropriate serialization defaults to readers of those records.

Exercise these cases in focused Rust tests:

- 4K source, actual 1080p created transcode: output height follows the
  created session; source dimensions do not fill unknown output width.
- Auto/fallback changes the created route: descriptor follows `StartInfo`
  rather than the requested or preflight route.
- Copy video plus converted audio: action is copy, and video facts remain
  eligible without claiming audio output.
- Cached output, replay, old saved JSON and staged successor: correct
  artifact/session binding; absent old fields remain valid.
- Status publication crosses an owner/attempt change: stale metadata is
  rejected by the existing guard along with the rest of the response.
- Policy codec strings never become the reported media codec.
- Master playlist output is unchanged by this repair.

**Acceptance:** the pinned Rust package check, Clippy, format check and
focused regressions pass before push. There is no new request, probe,
control-v1 field, schema migration, or change to encoding/manifest policy.

### 6.4 M3 — web library, direct and Live TV consumers

**Entry points:** [web player](../../crates/plurxd/src/web/index.html),
`playbackStatsTelemetry`, `liveTvStatsTelemetry`, `playbackInfoRows`,
`playbackInfoHelp`, ordinary attach code, `commitPreparedReplacement`, and
its rollback/retirement path. Read the existing attachment predicate before
adding state; do not replace it with `video.src != ''` alone.

**Work:** retain response descriptors with the same player/session record
as the attachment. Normalize them into §4 observations. Use source-shaped
level dimensions only after an authoritative copy/direct check. Unknown
action is not copy. On transcodes, use eligible sample/server metadata even
when the level contains apparently complete dimensions.

On initial direct/progressive playback, retain the eligible decision
descriptor. When that delivery changes to HLS or is replaced, invalidate
the old descriptor. On prepare, hold candidate data separately; on commit,
switch metadata identity with the attached player; on rollback, restore
the predecessor's metadata with its actual player state. Existing status
callbacks must capture identity at request start and reject a result after
that identity changes.

Live TV reuses `delivery.output` and action, with the planned-dimensions
note. Validate session/lease and channel identity on late status responses.
Do not modify live lease renewal or add a second polling interval.

For web device audio, implement honest availability now. No permission
prompt, output-device picker, `setSinkId`, Web Audio graph, or route mutation.
If the app already retains a permitted route label, it may be shown as a
route-only fact; discovering new device labels is not required for this
package. Missing collector and unsupported format-reporting API are distinct.

**Acceptance:** production-adapter tests cover hls.js, no hls.js, copy,
transcode, direct→HLS, prepare/commit/rollback, late status and Live TV switch.
Panel open/close adds no network traffic or new session/tuner action.

### 6.5 M4 — Apple attached-item format and route/session facts

**Entry points:** [Models.swift](../../clients/apple/Sources/Models.swift),
[PlayerController.swift](../../clients/apple/Sources/PlayerController.swift),
[PlayerView.swift](../../clients/apple/Sources/PlayerView.swift),
[LiveTvView.swift](../../clients/apple/Sources/LiveTvView.swift), and the
redesign's shared `PlaybackInfoPanel.swift`. The panel exists in the fresh
redesign tree; do not recreate it from the old main checkout.

**Work:** add tolerant optional descriptor decoding to `HlsStart`,
`PlaybackSessionStatus` and eligible decision models. Use the same
normalization and formatting for library and Live TV. Replace the constant
stream-format branch with an attached-item collector and server fallback.

The bounded collector candidate is the attached item's enabled video track,
its `assetTrack`, and asynchronously loaded video `formatDescriptions`.
Extract coded dimensions and codec from a valid format description. Check
the deployment target's supported loading API; do not synchronously block
the UI. If several descriptions cannot be bound to the current selected
format, use the server descriptor rather than picking the first blindly.
Keep `presentationSize` as the separate Player display size measurement.

For device audio, read the existing `AVAudioSession` route and supported
session-output properties. Observe route changes while the collector is
needed; maintain exactly one observation owner. Do not call `setCategory`,
`setActive`, output override or preferred-channel setters from this feature.
Use route-only output when a session value cannot be meaningfully scoped.
No channel count is a receiver-layout or Atmos assertion.

Fence asynchronous loads and route callbacks to controller lifetime and
item/route generation. Cancel or ignore late completions on stop, replace
and live channel change. An invalidated route cannot retain a current-value
badge. Cover iOS and tvOS separately, including unavailable APIs and AirPlay.

**Acceptance:** both native targets compile; focused tests cover absent and
malformed optional metadata, value formatting, item replacement and observer
cleanup. Physical evidence verifies any route/session or HLS sample-format
behavior claimed as supported. If sample collection is inconclusive, ship
the verified server fallback and explicitly record the collector limitation.

### 6.6 M5 — Android provenance proof and bounded output collection

**Entry points:** Android data `Models.kt` and API configuration,
`player/Controller.kt`, `player/PlayerScreen.kt`, `PlaybackInfoPanel.kt`,
`livetv/LiveTvScreen.kt`, `LiveTvPlayer.kt`, and offline player adapter.
Keep their existing architecture and pinned Media3 version.

**Work:** decode the descriptor defensively, connect existing start/status
responses and lifecycle identity, and implement the §4 formatter. Preserve
eligible `videoSize` for measured Stream frame. Player display size remains
unavailable where the platform does not report it. Trace `videoFormat` from the real
input-format callback: a selected track-group declaration is not automatically
the same object or provenance. Test 4K source→1080p output with a source-sized
master, then retain sample-derived dimensions or fence manifest-derived
ones according to the observed path. Do not force a pre-fix failure when
the pinned path is already correct. Add the regression either way.

For audio, inspect the current Media3 integration for access to the actual
playing sink's routing observation. Use `getRoutedDevice` only when valid
for that sink; do not substitute preferred-device, connected-device lists,
supported channel counts or `player.audioFormat`. Do not add a custom audio
renderer solely for this panel. If actual sink access is unavailable within
the existing integration, ship `not_implemented` with a recorded reason.
Where collection works, invalidate it when paused/inactive or rerouted as
the API requires, and remove observers on teardown.

Downloads continue to work offline. A local file may provide sample format
without a server descriptor; no HTTP request is permitted to fill its row.
Do not route offline sample metadata through the HLS manifest-copy fence.

**Acceptance:** JVM tests and application compile pass; physical/emulator
playback confirms the actual format path. Any claimed physical route
support has physical-device evidence. Library, Live TV and offline use the
same meaning for a value and the same missing-reason vocabulary.

### 6.7 M6 — integrated review, documentation and delivery

**Work:** integrate packages, reconcile generated contracts, and run §9's
cross-surface scenarios on the exact candidate. Update
[API.md](../API.md), the
[input contract](PLAYER-INPUT-CONTRACT.md), and current behavior docs only
where this implementation changes their claims. Keep the RCA as an evidence
record; add implementation links/status without rewriting history as if
the original defect never existed.

Advance native release counters using the repository's current release
tooling/validation, once at the appropriate integration boundary. Do not
copy the anchor's counter values. Required per-build documentation must
have its proper index treatment. Keep new verification records in the
subject/evidence locations required by the docs index convention.

**Acceptance:** all mandatory rows in §9 have pass or explicit bounded
unsupported results; no unverified platform capability is advertised.
Exactly one adversarial code review examines the integrated candidate;
address its actionable findings before the main-bound lane. Follow §10 for
required gates. Deployment is a separate action, not a consequence of
document completion or a successful merge.

## 7. Attachment state must change atomically with the player

| Event | Stream observation | Audio observation |
|---|---|---|
| New attach | New local identity; retain only matching decision/start data | Clear prior current-route claim until valid for this session |
| Prepare successor | Candidate storage; predecessor still displayed | No candidate route claim while predecessor owns playback |
| Commit successor | Swap identity and eligible facts with the player; status fills later gaps | Revalidate route/session facts without reconfiguring audio |
| Rollback | Restore the actual predecessor and its matching facts | Restore only facts whose route validity still holds |
| Same-session rendition change | Invalidate old rendition dimensions/codecs together | Stream audio and device route remain separate |
| Channel change | Invalidate prior lease/channel callbacks | Clear or refresh current audio observation |
| Route change | Stream metadata remains about the attached media | Invalidate old route/session observation and refresh read-only |
| Stop/controller destruction | Clear values and cancel pending collection | Remove observers; late callbacks cannot publish |

Capture the local identity when a query starts, not when its response
arrives. Check both request/session identity and the current attachment
generation. A timestamp alone is not an identity fence. No descriptor may
change transport choice, recovery classification, quality selection,
subtitle behavior, loaded-buffer values or playback-control acknowledgments.

## 8. Focused commands and evidence rules

Run the smallest meaningful regression after the relevant change. Passing
formatters alone is insufficient for an adapter repair; the test must reach
the production adapter or its real event integration.

```bash
# Existing web contract checks; run from the implementation checkout.
node tests/web/player-dom.test.js
node tests/playback/web-policy.test.js
node tests/playback/player-input-contract.test.js

# Regenerate after canonical fixture changes, then check agreement again.
scripts/player-contract-table --write
scripts/player-contract-table --embed

# Documentation index, including the handoff/RCA once tracked.
python3 -m unittest discover -s tests/operations -p test_docs_index.py

# Rust compiler loop; also run the named focused regressions from M2.
cargo fmt --all -- --check
cargo check -p plurxd --all-targets --locked
cargo clippy -p plurxd --all-targets --locked -- -D warnings
```

Add targeted behavioral tests with a common discoverable name such as
`playback_info_stream_format`, then verify the filter executes the expected
tests rather than zero cases:

```bash
cargo test -p plurxd --bin plurxd --locked playback_info_stream_format
```

That name is a proposed test prefix, not a currently existing test. Run the
broader package loop required by the repository workflow before final
qualification; do not represent a filter-only run as the unit suite.

```bash
make apple-build                               # Existing iOS/tvOS compile target.

# On a host with the configured Android SDK/JDK, from clients/android/:
./gradlew --no-daemon :app:assembleDebug
./gradlew --no-daemon :app:testDebugUnitTest \
  --tests 'tv.plurx.app.player.PlaybackInfoContractTest'
```

Add new Android behavioral classes to the focused command if their names
differ. Use the repository's configured Android build image when local SDKs
are unavailable; do not install an arbitrary replacement toolchain.

Apple tests live under the existing iOS/tvOS test targets in
[project.yml](../../clients/apple/project.yml). Select an available simulator
and run the relevant XCTest methods using `-only-testing`; record the full
resolved command. Do not guess a simulator UUID, claim simulator routing
proves HDMI output, or build the same target repeatedly without a change.

For each proof record: candidate SHA, command, platform/OS, fixture or media
identity, actual result, and limitation. Strip session-capability URLs and
opaque device identifiers from retained evidence. Reuse existing evidence
facilities; this task does not add a telemetry service or logging of private
route names to the server.

## 9. Integrated acceptance matrix

| Case | Must establish |
|---|---|
| Web 4K→1080p with source-sized master | Old production projection fails the desired assertion; new adapter rejects 3840×2160 and shows only eligible output facts. |
| Native HLS, hls.js unavailable, progressive/direct | Missing hls.js is no longer the sole cause of unknown stream format when an eligible descriptor exists. |
| Copy video / converted audio | Eligible copied video dimensions survive; stream audio is not device output. |
| Unknown action and old server | Do not treat missing action as copy. Honest unknown/partial output; playback still starts. |
| Codec-only / height-only / pair-only / invalid values | Correct partial display; no fabricated width, `0×0`, policy codec or nonfinite dimensions. |
| Auto fallback and cached VOD | Descriptor follows the actual created or cached output, not the requested rung or source. |
| Idempotent replay and old saved response | Stable saved answer, no decoding failure, no new schema requirement. |
| Prepared commit and rollback | Candidate data stays hidden until attached; no predecessor facts after commit; rollback restores correct ownership. |
| Late status / owner transition / rendition change | Stale values cannot overwrite the new attachment, and status still uses the existing server publication guard. |
| Strict v1 peers | Control response/action/selection bytes and validation remain unchanged by metadata work. |
| Live TV switch and planned width | Old channel cannot reappear; planned scaling is correctly labeled; no extra lease work. |
| Apple HLS format descriptions | Actual collector is qualified or server fallback is explicit; picture size remains a separate measurement. |
| Android single-variant transcode and offline file | Correct sample dimensions are preserved; manifest contamination, if observed, is fenced; offline works without network. |
| Device route and pause/reroute | Only valid route/session observations are current; no preferred/capability/track values masquerade as receiver facts. |
| Old client/new server and new client/old server | Optional metadata cannot reject playback, break relay decoding or change recovery. |
| Malformed optional descriptor | Enclosing start/decision/status stays usable; only invalid diagnostic fields degrade. |
| Repeated info open/close, all modes | Bounded observers, no added requests, no focus regression, both detailed rows reachable. |
| Source/stream/picture disagree legitimately | All remain separately labeled; no fallback conceals the disagreement by copying a neighboring value. |

Minimum physical evidence for supported native claims: iOS plus tvOS for
Apple route/session reporting; an Android playback path for format provenance;
and an actual Android output route only if that collector ships. Test
available speaker, Bluetooth, HDMI/AirPlay routes appropriate to each device.
Record unavailable routes rather than fabricating broad fleet coverage.

Unsupported audio format reporting is an allowed final result. A missing
stream descriptor on a path the new server can describe is an implementation
gap, not a platform limitation. Sol must distinguish those in the final report.

## 10. Review, merge and scope boundaries

Use normal commits and the tracked hook. Compile and run focused behavior
tests before pushing; a hook or remote lane does not replace this evidence.
The supplied contributor convention requires blocking effort-development
evidence and final main-promotion qualification/receipt. Preserve those
requirements; use existing manual workflows where automation no longer
starts them. Do not mark them passed because a PR allocated no jobs.

The pipeline header at locally known main `c9e4edf4` is newer than the dirty
checkout's copy: a main-bound PR opens as draft, receives one adversarial
review, and starts the fast lane when marked ready. **There is no
`fast-lane` label step.** Returning it to draft cancels the lane. Effort
compiler workflows and full qualification are manual. Read the fresh
workflow files before executing; do not create CI workflows or use old
label-based instructions to work around a missing required check.

After all task packages integrate, freeze task merges, merge fresh main
into the effort, and reverify the exact candidate. Open the final draft PR,
review, address findings, mark ready, and require the applicable current
checks plus the contributor-required qualification evidence before merge.
If the base/candidate changes, old evidence does not qualify the new tree.
An unavailable required gate or unresolved governance conflict blocks merge,
not the remaining local implementation work.

**Out of scope:** changing HLS manifest semantics, codecs or transcode
quality policy; replacing Media3's renderer; negotiating audio routes;
adding permission prompts, control protocol fields or a database schema;
building a telemetry framework; adding CI; and production rollout without
separate authorization. The
[historical native-master compatibility ruling](CLIENTS-REMEDIATION-PLAN.md#10-non-goals-and-guardrails)
explains why this reporting fix leaves current master behavior intact.

## 11. Completion report for the user

Provide the implemented behavior and limitations, task/final PR links,
candidate SHA, focused regressions and native compile results. Identify
physical-device evidence separately. State whether each client now reports
stream format, a route, session-output facts, or an explicit unavailable
reason. Report any collector deliberately left unimplemented and why.

Do not say “all fixed” merely because the placeholder wording changed.
Completion requires the wrong-provenance repair, descriptor wiring,
lifecycle protection and compatibility proof, together with honest platform
coverage. A merge does not imply an image or native app was deployed.

## 12. Execution ledger

Update this table in the implementing branch; record commands/results below
the relevant row when work starts. Each package is currently unbuilt.

| Package | State | Base/candidate | Focused evidence | Remaining limitation |
|---|---|---|---|---|
| M0 | Not started | — | — | Fresh base and compiler environment to establish |
| M1 | Not started | — | — | Production regression and normalized value fixture |
| M2 | Not started | — | — | Response/relay/replay coverage and local Rust proof |
| M3 | Not started | — | — | Web lifecycle and Live TV integration |
| M4 | Not started | — | — | Apple compile and device observations |
| M5 | Not started | — | — | Android provenance and route feasibility |
| M6 | Not started | — | — | Integrated review, qualification and final report |

**Document preparation:** repository source and locally known workflow
contracts were inspected; no implementation, Rust compiler run, native
build, PR, merge or deployment was performed by the documentation task.
The four documentation-index tests passed in the dirty working tree.
Separate checks verified both new documents' local links, index entries,
code-fence balance and whitespace, since the tracked-file sweep does not
yet include these untracked documents. These checks validate the handoff,
not an implementation candidate.
