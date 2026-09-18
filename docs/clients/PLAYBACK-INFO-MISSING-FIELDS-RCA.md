# Playback info — missing output facts and misleading stream dimensions

**Status:** open — RCA and proposed solution, ready for review.
**Written / revised:** 2026-09-16, after review B1/S1–S3.
**Scope:** web, Apple and Android; library/VOD,
direct files, downloads where supported, and Live TV.

Companion to the [player input contract](PLAYER-INPUT-CONTRACT.md) and
[playback reference](../PLAYBACK.md). This document explains the two
`Not reported` rows in the supplied screenshot, identifies what the code
actually collects, and proposes an implementation with acceptance criteria.
It does not change playback behavior. Review the evidence boundary in §2
before treating a statement about the inspected revision as deployed fact.

The [implementation handoff](PLAYBACK-INFO-IMPLEMENTATION-HANDOFF.md) turns
this revised proposal into Sol's package sequence, wire contract and
acceptance checks. Use that handoff for execution and this RCA for evidence.

## 1. Finding and recommended decision

The two rows have different causes:

- **Web stream format can be wrong on transcodes, as well as missing.** The
  library player trusts active hls.js level dimensions, but the server's
  master builder advertises the source file's dimensions. When that master
  describes a 4K source converted to 1080p, the row reports `3840×2160` as
  stream format. This affects the master-backed path; it is not evidence
  that every transcode uses that master. Native HLS and progressive playback
  have no hls.js level and remain unknown. Apple library playback returns a
  literal unknown value. Android's HLS dimensions need a provenance check;
  the review's analogous Android failure is not established by the pinned
  Media3 code alone (§3.2). Live TV uses server-planned output metadata.
- **Device audio output has no reporting implementation in these panels.**
  Web library, Apple and Android return a literal unknown value. Web Live
  TV omits the value and the shared renderer supplies the same placeholder.
  These paths do not distinguish an unsupported API from an unimplemented
  collector or a temporarily missing observation.
- **The presentation makes both deficiencies permanent-looking.** The
  shared contract marks both rows `always`; its fallback text carries no
  explanation of why a value is missing or whether it can become available.

The design intends to separate source, stream, picture and physical output.
The stream-format implementation violates that intent when it relabels the
master's source dimensions as delivered dimensions. Picture-size and audio
separation tests do not protect this row.

**Recommendation:** keep the two facts distinct, supply stream format from
metadata bound to the attached delivery, reject source-shaped manifest
dimensions on video transcodes, and introduce explicit availability and
provenance. Reuse the existing session codec/height producers. Report native
audio route/session facts only within their
documented meaning. Where output reporting is not implemented, say so;
where the platform cannot expose a fact, explain that separately.

This is a playback-information defect. The screenshot and source inspection
do not establish an audio failure, a wrong transcode, or a broken decoder.

## 2. Evidence is anchored to a specific source revision

### 2.1 The matching implementation is newer than the main checkout

| Item | Recorded evidence |
|---|---|
| User observation | Screenshot dated 2026-09-16 15:49:32, showing both rows as `Not reported` and their explanatory text. |
| Matching source | Commit `5d4235d5e1cfb1276aff224e6d6d8f62b40b91ed`, inspected in the clean `tmp/public-readme-refresh` worktree on branch `codex/public-readme-refresh`. |
| Main checkout at investigation | `10f2afe60b3d177866fdcc5741acd9f494525d73`, with pre-existing local changes. Its playback-info implementation predates the matching redesign. |
| Deployment identification | Not collected. The screenshot does not identify browser, device, playback transport, live/library mode, or deployed build. |
| Runtime evidence | A small execution of the actual web projection statements, plus the existing web DOM regression at the matching revision. No live device session was inspected. |
| Supplied review evidence | Reviewer reports the same code and passing projection/DOM/policy/input-contract checks at `5d4235d5` and `c9e4edf451e12247a7aa4188903e5ba36888e7e9`. These review-reported test runs are distinct from this document's local verification. |

The screenshot's explanatory text exactly matches `playbackInfoHelp` in
the web implementation. That supports identifying the web UI; it does not
prove a particular stream or transport. With a populated active hls.js
level from the source-shaped master, the row would show dimensions, possibly
wrong ones. First investigate progressive/direct playback (`p.hls == null`)
or missing level/probe metadata. Native HLS and a level not yet selected
remain alternatives; the screenshot alone cannot rank them reliably.

Safari is not synonymous with native HLS here. `preferNativeHls` and
`PlaybackPolicy.hlsTransport` select native playback when available for
copied HEVC or when hls.js is unsupported; otherwise they prefer MSE.
The review's “AirPlay only” description is too narrow. AirPlay motivates
native support but is not the sole transport-selection condition.

The source links below open repository paths for navigation. **All line
numbers and excerpts in this RCA refer to the matching commit**, not to
whatever revision a reader has checked out. Use the recorded commit when
reviewing; re-resolve symbols against the intended implementation base.
No runtime code was changed for this document.

### 2.2 Source evidence and scope

| Evidence | Location at the matching commit | What it proves |
|---|---|---|
| E1 | [Web player](../../crates/plurxd/src/web/index.html), `playbackStatsTelemetry`, lines 14118 and 14146–14147 | Stream format uses the active hls.js level only; device audio is a constant. |
| E2 | Same file, `playbackInfoRows` near 13838 and `playbackInfoHelp` near 13875 | Always-visible rows receive the shared missing-value text; help text matches the screenshot. |
| E3 | [Apple player view](../../clients/apple/Sources/PlayerView.swift), lines 2984–2997 | Library playback measures presentation size separately but returns `Not reported` for both requested fields. |
| E4 | [Android player](../../clients/android/app/src/main/java/tv/plurx/app/player/PlayerScreen.kt), lines 2043, 2062, 2303–2306 and 2696 | Library playback supplies `playingVideo` from `player.videoFormat`; device audio remains a constant. |
| E5 | Web `liveTvStatsTelemetry`, lines 16831–16862 | Live TV stream format uses `status.delivery` or the current lease's delivery plan; no device-audio value is supplied. |
| E6 | [Apple Live TV](../../clients/apple/Sources/LiveTvView.swift), lines 923–929 | Stream format uses `plan.videoDescription`; device audio is a constant. |
| E7 | [Android Live TV](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvScreen.kt), lines 2290–2294 | Stream format uses `plan.output`; device audio is a constant. |
| E8 | [Field fixture](../../tests/playback/playback-info-fields.json), `stream_format` and `device_audio` | Both fields are `always: true` in standard, details and debug modes. |
| E9 | [Apple models](../../clients/apple/Sources/Models.swift), `Delivery`, `Decision`, `HlsStart` | Library delivery is not a complete output-format descriptor. Source metadata, dynamic range and session height exist separately. |
| E10 | [HLS server](../../crates/plurxd/src/http/hls.rs), `master_playlist_with_shape`, lines 11444 and 11526–11576 | Emits source `file.width/height` when both are positive, with no output-size correction. Codec emission is restricted to the HEVC/HDR branch and enabled shape; H.264 masters omit it. This is the source of B1. |
| E11 | [Transcode context](../../crates/plurxd/src/transcode.rs), `HlsContext` near 8299; HLS `StartResponse` near 119 | Session codec plumbing already exists; the response already carries normalized output `height` and `encoder`. `HlsContext` has no output dimensions. |
| E12 | [Live TV planning](../../crates/plurxd/src/live_tv_delivery.rs), lines 614–666 | Output width is planned from measured source dimensions and selected height, rounded down to even when scaling; it is not a decoded-picture measurement. |
| E13 | [Web transport policy](../../crates/plurxd/src/web/playback-policy.js), `hlsTransport` near 541; web `preferNativeHls` near 6912 | Native HLS covers copied HEVC and missing hls.js support when native playback is available, not merely AirPlay. |

Exact excerpts from the matching implementation:

```javascript
const level=p.hls&&p.hls.levels&&p.hls.levels[p.hls.currentLevel];
// In the returned web library telemetry object:
stream_format:level?[level.width>0&&level.height>0?`${level.width}×${level.height}`:null,level.videoCodec].filter(Boolean).join(" · ")||"Not reported":"Not reported",
device_audio:"Not reported",decode_audio:null,
```

```swift
case "stream_format", "device_audio":
    return ContractFieldValue(value: "Not reported", tone: .muted)
```

These branches establish the placeholder behavior without assuming any
hardware limitation. They do not establish that the operating system lacks
all relevant APIs.

### 2.3 Deterministic reproduction

Executing E1's projection statements produced:

| Input to the web library adapter | Stream format | Device audio output |
|---|---|---|
| No `p.hls` object | Not reported | Not reported |
| Empty levels, `currentLevel = -1` | Not reported | Not reported |
| Active level `{}` | Not reported | Not reported |
| Active level with width only | Not reported | Not reported |
| Active level with 1920×1080 and `avc1.640028` | 1920×1080 · avc1.640028 | Not reported |
| Active level with codec only | hvc1.2.4.L153.B0 | Not reported |
| Source-shaped level 3840×2160 for an actual 1080p transcode, no codec | **3840×2160 — wrong stream dimensions** | Not reported |

Reproduce without checking out another branch or modifying source:

```bash
# Execute the actual projection statements from the recorded Git object.
node <<'JS'
const assert = require('node:assert/strict');
const {execFileSync} = require('node:child_process');
const revision = '5d4235d5e1cfb1276aff224e6d6d8f62b40b91ed';
const source = execFileSync('git', [
  'show', `${revision}:crates/plurxd/src/web/index.html`,
], {encoding: 'utf8', maxBuffer: 16 * 1024 * 1024});
const lines = source.split('\n');
const level = lines.find(s => s.trim().startsWith('const level=p.hls&&'));
const format = lines.find(s => s.trim().startsWith('stream_format:level?'));
const audio = lines.find(s => s.trim().startsWith('device_audio:"Not reported"'));
assert.ok(level && format && audio);
const read = new Function('p', `${level}\nreturn {${format}\n${audio}\n};`);
for (const [name, p, expected] of [
  ['no hls.js', {}, 'Not reported'],
  ['no selected level', {hls: {levels: [], currentLevel: -1}}, 'Not reported'],
  ['empty level', {hls: {levels: [{}], currentLevel: 0}}, 'Not reported'],
  ['width only', {hls: {levels: [{width: 1920}], currentLevel: 0}}, 'Not reported'],
  ['complete level', {hls: {levels: [{width: 1920, height: 1080,
    videoCodec: 'avc1.640028'}], currentLevel: 0}}, '1920×1080 · avc1.640028'],
  ['codec only', {hls: {levels: [{videoCodec: 'hvc1.2.4.L153.B0'}],
    currentLevel: 0}}, 'hvc1.2.4.L153.B0'],
  // This asserts the current defect, not the desired post-fix answer.
  ['4K source / 1080p output', {method: 'transcode', autoHeight: 1080,
    hls: {levels: [{width: 3840, height: 2160}], currentLevel: 0}},
    '3840×2160'],
]) {
  const result = read(p);
  assert.equal(result.stream_format, expected);
  assert.equal(result.device_audio, 'Not reported');
  console.log(name, result);
}
JS
```

The last case reproduces the wrong value using the level shape produced
from E10's master. The server tests
`native_hls_master_advertises_selection_language_names_and_forced_metadata`
and `hdr_master_declares_the_range_and_exact_session_codecs` corroborate the
source-sized resolution and H.264 codec omission. Their source was inspected;
the Rust tests were not executed in this documentation task.

This is a narrow projection check. It does not exercise browser attachment,
HLS selection events, platform audio APIs or actual playback.

## 3. Why the gaps survived the redesign

### 3.1 The redesign separated labels without validating every producer

The redesign separated original media, delivered stream metadata, the
player's picture dimensions and physical output. The intent is to prevent
source properties becoming output claims. However, the stream-format row
accepts a manifest field without checking what the server used to produce it.

Its implementation record, `PLAYBACK-INFO-REDESIGN.md` in the matching
snapshot, explicitly describes unsupported output values and missing Apple
stream dimensions. Commits `104c8d0ce`, `daf9e789b` and `b66f31816` record
the redesign, review corrections and visual verification. This RCA does
not claim those commits first introduced every underlying data gap.

Apple library has no stream-format collector; web library's sole collector
can carry source dimensions through the manifest; none of these panels
collect audio-output facts. Always-visible labels expose the missing
collectors, while plausible nonempty values conceal the provenance defect.

### 3.2 The web defect is established; Android requires a narrower finding

The master builder takes a `MediaFile` and a dimension-free `HlsContext`.
It emits a single video variant with positive source dimensions. The
vendored hls.js parser reads `RESOLUTION` into level width/height and
`CODECS` into codec fields. E1 then displays that level unchanged. Thus a
source-sized master can yield a wrong stream-format row even while the
actual decoded picture is correct. No API failure or missing probe is
needed for this defect.

The review cites Media3's `deriveFormat`, which does copy playlist video
dimensions. But the repository pins Media3 1.10.1: its single-variant
track-building branch and `readData` use `withManifestFormatInfo` instead.
That method preserves sample width/height. Therefore `deriveFormat` alone
does not prove the same wrong dimensions reach `player.videoFormat` in
this single-variant case. See the pinned
[HLS wrapper](https://raw.githubusercontent.com/androidx/media/1.10.1/libraries/exoplayer_hls/src/main/java/androidx/media3/exoplayer/hls/HlsSampleStreamWrapper.java)
and [format merge](https://raw.githubusercontent.com/androidx/media/1.10.1/libraries/common/src/main/java/androidx/media3/common/Format.java).

Keep an Android 4K→1080p regression and verify the actual input-format
callback path before claiming Android is affected or discarding valid
sample dimensions. Manifest-derived fields require a copy-only fence;
proven sample-derived dimensions remain eligible on transcodes. Device
execution is still required, including chunkless preparation and any
multi-variant path the application actually uses.

### 3.3 Different availability states collapse into one string

The same text represents all of these situations:

- The platform adapter has not been implemented.
- The selected playback transport has no collector.
- A player or active rendition has not attached yet.
- A manifest has omitted optional metadata.
- A platform query cannot expose the requested fact.

Waiting longer can help only some of these cases. The existing UI gives
the viewer no way to distinguish them.

### 3.4 Existing tests do not validate stream-format provenance

The inspected [web DOM regression](../../tests/web/player-dom.test.js)
checks unknown picture dimensions, source/stream audio separation, and
reachability of diagnostic fields. It has no targeted `stream_format` or
`device_audio` value assertion. It passed at the matching revision.
The [Android contract test](../../clients/android/app/src/test/java/tv/plurx/app/player/PlaybackInfoContractTest.kt)
checks field labels, placement and missing player dimensions. These are
useful protections, but their passing does not demonstrate collection of
stream metadata on native HLS, correctness of transcoded stream dimensions,
or any audio-output observation.

**Escape mechanism:** the reviewed contract permits honest unknowns; the
inspected tests verify their presentation without requiring a capability
matrix and a correctly sourced producer for each field/transport pair.
The original six-case harness also assumed its level dimensions were
truthful; the added seventh case exposes that assumption.
This is a conclusion from the inspected tests, not a claim that every test
in the repository has been exhaustively audited.

## 4. Proposed contract — facts carry meaning and availability

### 4.1 Keep the existing concepts separate

| Fact | Permitted meaning | Never substitute |
|---|---|---|
| Original video/audio | Catalog or probe metadata for the source asset | A claim about the current delivery or physical output |
| Stream format | Video format of the attached stream, with validated sample/manifest provenance or explicitly labeled server-planned delivery metadata | Display resolution, source-shaped manifest dimensions after conversion, or the requested quality rung |
| Playing resolution | Positive picture dimensions observed on the attached player | Source, manifest or delivery-plan dimensions |
| Stream audio track | Selected delivered/decoder-input audio metadata, with its provenance | Receiver format, speaker layout or successful passthrough |
| Device audio output | Platform-reported route and any precisely scoped output/session facts | Track codec, route capabilities, maximum channel counts or a preferred route |

The output row must not promise to discover the format a downstream receiver
ultimately decodes. Route, audio-session configuration and receiver output
are distinct facts, even when their values happen to agree.

For Live TV, `delivery.output.width` is a planned value derived from measured
source aspect and the selected scaling height (E12). Display it as
`Server delivery metadata (planned)`; do not imply that it was measured from
decoded frames. The same label applies to any recipe-derived dimensions.

### 4.2 Introduce a client-side observation model

The following is a **proposed interface**, not an existing server schema.
Use equivalent native types in Swift and Kotlin. Keep existing field IDs
and persisted mode values so this can integrate with the current panels.

```typescript
type Availability = 'known' | 'pending' | 'unavailable' | 'not_applicable';
type Provenance = 'player_input' | 'manifest' | 'server_delivery'
  | 'platform_route' | 'platform_session';
type MissingReason = 'not_implemented' | 'unsupported'
  | 'metadata_missing' | 'not_attached' | 'refreshing'
  | 'source_metadata_only' | 'query_failed' | 'permission_required'
  | 'no_audio';

type InfoObservation<T> = {
  state: Availability;
  value: T | null;
  provenance: Provenance | null;
  reason: MissingReason | null;
  attachmentKey: string;
  observedAtMonotonicMs: number | null;
};

type StreamFormat = {
  videoCodec: string | null;
  codedWidth: number | null;
  codedHeight: number | null;
};

type DeviceAudio = {
  routes: Array<{kind: string; label: string | null}>;
  sessionOutputChannels: number | null;
  sessionSampleRateHz: number | null;
};
```

**Invariants:** `known` requires at least one valid value. Empty codec text,
zero/negative dimensions and unknown sentinels are not values. Print a
dimension pair only when both dimensions are positive. A codec alone or a
dimension pair alone is useful; a known height alone may be labeled as
height without inventing width. Missing subfields do not erase good ones.
`pending`, `unavailable` and `not_applicable` carry no current value and
require a reason. `not_applicable` is reserved for positive knowledge, such
as a stream with no audio; a temporarily absent track is not enough.

The containing observation's provenance applies to all its populated
fields. Do not combine a codec from rendition A with dimensions from
rendition B. If multiple observations are shown together, retain a source
and attachment identity for each component. Route and session observations
should remain separate internally if their validity or timestamps differ.

Use `pending` only while an actual attachment or query is outstanding.
Completion with no usable metadata becomes `unavailable/metadata_missing`;
query failure becomes `unavailable/query_failed`. Do not add an endless
spinner, a retry loop, or a new network request to every panel refresh.

### 4.3 Render an explanation the viewer can act on

| State/reason | Proposed visible value | Supporting explanation |
|---|---|---|
| Stream known | `1920×1080 · H.264` | `Active stream metadata` or `Server delivery metadata`, according to provenance |
| Stream known, codec only | `HEVC` | `Stream dimensions unavailable` |
| Pending attachment | `Waiting for stream` | `Available after this stream attaches` |
| Metadata absent | `Metadata unavailable` | `The active stream did not supply this information` |
| Only source-shaped metadata after conversion | `Output metadata unavailable` | `The available dimensions describe the original file` |
| Collector missing | `Not available in this version` | `Output reporting is not implemented for this player` |
| API unsupported | `Unavailable in this player` | `This player cannot report the device output format` |
| Route only | `HDMI` | `Output format unavailable` |
| Session facts observed | `HDMI · 2 session channels · 48 kHz` | `Audio session report; receiver format is not verified` |
| Query failed | `Output information unavailable` | `The platform query did not return a usable result` |
| No audio | `No audio stream` | No suggestion of an output fault |

The examples are illustrative, not collected device evidence. Preserve
unknown fields in Details and Diagnostics, where their explanations help.
Compact and Overview should keep their existing essentials; this proposal
does not add a permanent unavailable-output headline. Diagnostics may show
the reason, provenance and local observation age. Do not expose opaque
device identifiers or URL tokens as user-facing values.

## 5. Stream format — complete collection without inventing a measurement

### 5.1 Select metadata for the attached delivery

Resolve sources in this order, but only when the source describes the
attached item/rendition:

1. **Validated sample/input-format observation.** Verify the origin of each
   field; a player API may merge manifest declarations into its input
   format. Use sample-derived dimensions for the attached stream. Do not
   grant all fields `player_input` provenance merely because the containing
   object came from a player API.
2. **Selected manifest rendition with verified unchanged video.** For the
   source-shaped masters inspected here, accept dimensions only when the
   attached session confirms video copy/direct delivery. Reject them for
   transcoded or unknown video actions, even when nonempty. Do not choose
   the first or highest rendition without knowing the attached selection.
3. **Server delivery descriptor.** Use metadata for the actual attached
   session/variant. Label it as server metadata, not as a decoded fact.
4. **Explicit unavailable state.** Explain missing metadata or a missing
   collector. Original source metadata stays in its existing source row.

This hierarchy chooses a coherent observation; it is not permission to
merge unrelated partial values. A requested rung or preflight decision is
not proof of the session created after fallback or of a later replacement.
For a video transcode, bypass source-shaped dimensions and use a validated
sample observation or matching delivery descriptor. If neither exists,
show explained missing information rather than resurrecting the source
size. A codec may remain useful on its own when its origin is independently
verified; it does not make adjacent manifest dimensions trustworthy.

### 5.2 Client changes

| Client/path | Proposed change | Boundary |
|---|---|---|
| Web with hls.js | Fence source-shaped level dimensions to verified video copy/direct sessions. On transcodes or unknown video action, skip those dimensions and use the attached delivery descriptor or independently validated sample metadata. | A nonempty level is not evidence of output dimensions; pending levels must not replace the current rendition. |
| Web native HLS | Consume a descriptor retained with the attached session; apply the same provenance and copy-only fence to retained source-shaped manifest dimensions. | Do not add per-refresh playlist fetching or parse an arbitrary rendition as the active one. |
| Web direct file | Use an explicit descriptor for the selected direct delivery. | A verified unchanged direct stream can carry source-derived metadata with server provenance; the UI must not infer this from `method` alone. |
| Apple library/VOD | Investigate attached `AVPlayerItem.tracks[].assetTrack` format descriptions; otherwise use the attached delivery descriptor. | Validate active-track selection and format availability before shipping; keep coded dimensions separate from presentation size. |
| Android library/downloads | Trace and test `player.videoFormat` fields at the pinned Media3 version. Preserve proven sample dimensions; fence manifest-derived dimensions to unchanged video and use delivery metadata otherwise. | Do not assume `deriveFormat` governs the single-variant input path. Offline playback remains local; invalid or absent dimensions stay explicit. |
| All Live TV clients | Normalize existing `delivery.output` values and label calculated dimensions as server-planned metadata. | Match the active lease/session and generation; channel-switch responses must not restore predecessor values. |

The concrete Apple spike starts with the attached item's enabled video
track, its [`assetTrack`](https://developer.apple.com/documentation/avfoundation/avplayeritemtrack/assettrack),
and loaded [`formatDescriptions`](https://developer.apple.com/documentation/avfoundation/avassettrack/formatdescriptions).
Inspect a video format description with
[`CMVideoFormatDescriptionGetDimensions`](https://developer.apple.com/documentation/coremedia/cmvideoformatdescriptiongetdimensions%28_%3A%29).
Use the supported asynchronous loading API for the deployment target and
fence completion to the same item/track. Test HLS startup, track changes and
replacement on iOS and tvOS. This names a concrete collection candidate;
it does not claim its reliability across the supported fleet is proven.

### 5.3 Expose existing session codec and height facts

E9 is the reason a client-only fallback is insufficient for complete library
coverage, but most of the producer data already exists. E11's `HlsContext`
carries session RFC 6381 codec strings; `StartResponse` already returns
normalized output `height` and `encoder`, mirrored in Apple's `HlsStart`.
The [client remediation plan §8.3](CLIENTS-REMEDIATION-PLAN.md#83-housekeeping-notes--record-dont-refactor)
records the `hls_codecs` plumbing. Reuse these owners instead of inventing
a parallel codec calculator or treating the requested quality as output.

The narrow addition is exposure of the session's validated video codec,
reuse of its existing output height on start/replacement paths, and width
only where the effective recipe establishes it. Preserve the full codec
string internally; format a friendly video-codec label without treating
the accompanying audio codec as physical-output information. Audit existing
fallback codec labels before exposing them: §8.3 also records a copied-audio
fallback that mislabels some codecs. Do not assert every internal string
has been verified against emitted media.

The Live TV delivery plan already provides output metadata and should be
adapted rather than replaced. When output width is unknown, retain known
height as a partial fact, such as `H.264 · height 1080`, with width marked
unavailable; do not fabricate `1920×1080` or a progressive-scan claim.

**Proposed wire addition, subject to API review:** an optional
`stream_format` object on the relevant library response that identifies
the selected direct delivery or the session actually created:

```json
{
  "stream_format": {
    "video_codec": "avc1.640028",
    "width": 1920,
    "height": 1080
  }
}
```

This object is an output descriptor, not a fresh runtime measurement.
The example is illustrative. A nested object groups values for consumers;
it must reuse the same height producer as the existing top-level field.
API review may choose additive codec/width fields beside the existing
height instead, but must retain one authoritative value for each fact.
Members may be absent or null when unknown. Its enclosing response must
provide the current file/session identity, and a session with several
variants needs per-variant descriptors plus an identified active variant.
One session-wide tuple must not be presented as the selected ABR rendition.
If selection is unknown, show the descriptor separately as planned/server
delivery information or leave the active stream row unavailable.

Populate the descriptor from the same effective output recipe and validated
format metadata that create the media, including copy, audio-only remux,
video transcode and cached VOD paths. For copy/direct delivery, source
metadata is eligible only when the server verifies the selected video is
unchanged. For transcodes, never reconstruct width from a requested height
and an assumed aspect ratio. A server recipe may calculate planned width
from measured source dimensions and the aspect/scaling rule the encoder
actually applies; preserve its exact rounding and sample-aspect treatment.
Live TV already calculates an even width from measured source width/height
and selected output height. Validate agreement with the encoder recipe and
label this as planned metadata, not a pixel measurement. If the recipe
cannot establish a dimension, omit it until authoritative information exists.

Do not add probes or delay playback startup just to complete this object.
Expose currently known facts; if exact codec inspection occurs later,
propagate its result through an existing scoped response when practical,
otherwise retain an explained partial descriptor.

Bind cached descriptors to the cached output artifact and its recipe.
Return created-session metadata after fallback selection, not the earlier
decision's requested values. Cover replacement attachment responses as well
as initial creation. Do not overload playback-control `DeliveryView` with
this field: that type is a different control/status contract.

Before coding, map the exact response producers and prepared-replacement
messages on the intended base. Check every consumer for strict decoding
and unknown-field behavior; “optional” does not by itself prove mixed-version
compatibility. Old servers yield explained missing metadata. Old clients
must continue playing when new servers include the descriptor.

## 6. Device audio — report only what the platform actually knows

### 6.1 Web should explain the limitation first

Replace the hardcoded string with an explicit observation. Initial scope
may report `not_implemented` for the device-output collector. If a browser
exposes only a default or selected sink, do not describe that as an HDMI
codec or speaker layout.

The [W3C Audio Output Devices API](https://www.w3.org/TR/audio-output/)
defines `sinkId` as a device identifier, with an empty string representing
the browser default. It does not provide a negotiated receiver-format
report. A later route-name enhancement may use already-permitted device
information. Opening playback info must not prompt for access, call
`setSinkId`, or switch devices. Keep any useful route fact separate from
the unavailable output-format fact.

### 6.2 Apple can investigate route and session facts

Apple documents the current route and the audio session's output-channel
count through [`AVAudioSession`](https://developer.apple.com/documentation/avfaudio/avaudiosession/currentroute)
and [`outputNumberOfChannels`](https://developer.apple.com/documentation/avfaudio/avaudiosession/outputnumberofchannels).
The proposed adapter reads existing session state and refreshes after route
changes. It must not activate, reconfigure or override the audio session
for diagnostic purposes.

Validate iPhone/iPad and tvOS separately. A route label such as HDMI or
Bluetooth is useful without a codec. A session channel count is not proof
of the receiver's final channel layout, Atmos rendering or bitstream
passthrough. Do not substitute the maximum supported channel count. Apple
library and Live TV should use the same collector and formatting rules.
Platform/API availability and AirPlay behavior remain device-spike items.

### 6.3 Android needs access to the actual playing audio sink

Android's [`AudioRouting.getRoutedDevice`](https://developer.android.com/reference/android/media/AudioRouting)
reports current routing only while the underlying track is playing;
`getPreferredDevice` is not guaranteed to be the actual route. Device
capabilities such as [`AudioDeviceInfo.getChannelCounts`](https://developer.android.com/reference/android/media/AudioDeviceInfo)
describe supported configurations, not the currently used channel count.

Determine whether the repository's pinned Media3 integration exposes the
actual sink/routing observation without replacing its audio renderer.
Until that is established, classify the collector as unimplemented, not
the operating system as incapable. Do not use `player.audioFormat` as
physical-output evidence. If collecting the route requires a new custom
audio sink, return that larger change for review instead of silently
expanding this fix. Paused/inactive tracks must clear or explicitly mark
historical route observations when the platform cannot provide a current
one.

## 7. Attachment changes must invalidate stale information

Reuse the playback owner's existing attachment/session/generation identity;
do not create a new playback authority for the info panel. A local
`attachmentKey` may be composed from those identifiers without sending
credentials or device IDs into diagnostic output.

| Event | Required behavior |
|---|---|
| Initial attach | Begin with the new identity and pending facts; promote only matching observations. |
| Prepare successor | Store candidate metadata separately; keep displaying the predecessor while it is still the attached player. |
| Commit replacement | Switch identity and facts together; clear any observation that belongs only to the predecessor. |
| Same-session quality change | Track rendition identity as well as session identity; do not retain the old tuple under the new rendition. |
| Audio-track change | Refresh stream-track metadata; independently refresh output/session facts if the platform reports a change. |
| Live channel change | Fence both delivery-plan/status responses and player observations to the new lease. |
| Route change | Invalidate old route/session facts; refresh without changing playback configuration. |
| Stop/detach | Clear current facts and release observers. Reopening the panel must not revive a prior session's values. |

Use existing refresh cadence for display and event-driven updates for
metadata changes. Closing the panel must dispose panel-owned work; any
controller-owned observer must follow controller lifetime. A local sample
timestamp means “read by this client,” not “freshly probed by the server.”
No new periodic server polling, tuner renewal or media probe belongs in
the panel renderer.

## 8. Implementation packages and acceptance checks

### 8.1 P1 — agree on semantics and explain unavailable values

Update the shared field fixture, generated field tables and client adapters
with the availability/reason contract. Preserve field IDs and stored modes.
Document the supported collector for each path. Replace generic permanent
unknowns with accurate copy; do not claim platform impossibility where
collection is merely missing.

**Acceptance:** fixture-driven tests cover each reason, partial values and
existing mode compatibility. Details and Diagnostics retain the two rows.
No missing-output state is styled as a playback error.

### 8.2 P2 — reject source-shaped dimensions and expose output metadata

First add a web regression with a source-sized 4K level and a 1080p created
session; it must fail against the current projection. Fence source-shaped
dimensions to verified unchanged video. Map and expose existing session
codec/height facts, reuse Live TV output, and bind every result to attachment
and rendition identity. Test Android's actual input-format path before
classifying its dimensions; preserve valid sample reporting.

**Acceptance:** each scenario in §9 reports either valid scoped metadata or
a specific justified unavailable state. A native-HLS library session with
a known server output no longer remains unknown merely because hls.js is
absent. Old/new client-server combinations still attach and play.

### 8.3 P3 — qualify native format and audio collection

Perform short Apple and Android feasibility spikes against the actual
player integration. Record the returned values, API/OS versions, paused
behavior and route-change behavior. Implement only observations whose
meaning and lifecycle have been demonstrated. Ship explicit unsupported or
unimplemented reasons for remaining facts.

For Apple, exercise the attached asset-track format-description candidate.
For Android, record `videoFormat`, picture size, manifest dimensions and
the attached server descriptor for an actual 4K→1080p transcode. If the
reported Android dimensions are already correct, retain that behavior and
its regression; do not manufacture a failing pre-fix test or a copy-only
restriction on proven sample dimensions. If wrong, capture the failing
case and apply the provenance fence before qualification.

**Acceptance:** physical-device evidence verifies route/session facts on
supported paths, with no source-track substitution or playback changes.
If Android requires a renderer replacement, keep accurate unavailable copy
and obtain a separate design review for that expansion.

### 8.4 P4 — qualify the integrated change on the intended base

Follow [the development pipeline](../DEVELOPMENT_PIPELINE.md). If this is
executed as a multi-task effort, use the repository's effort-branch and
promotion convention. Establish the pinned Rust compiler loop before any
server edit; compile and test the exact integrated branch again when its
base changes. Documentation approval is not implementation or deployment
approval, and this document does not claim qualification has happened.

**Acceptance:** focused regressions, affected platform builds, mixed-version
checks and the required branch gate pass for the exact candidate. Record
physical evidence separately from compilation and fixture rendering.

## 9. Validation must test the missing paths and the truth boundaries

| Scenario | Required assertion |
|---|---|
| hls.js complete, partial and empty levels | Only dimensions with eligible provenance survive; nonempty source-shaped dimensions on a transcode are rejected. Missing selection is explained. |
| Native HLS with known delivery format | Reports the attached server descriptor without depending on an hls.js instance. |
| Direct play and video-copy remux | Metadata remains explicitly tied to unchanged selected video; audio conversion does not become an output-route claim. |
| Web 4K source converted to 1080p with source-sized master | Before the fix the projection reports 3840×2160 (B1). The new acceptance assertion fails there; after the fix it reports attached output metadata or an explicit partial/unavailable value, never source dimensions. |
| Android 4K source converted to 1080p | Exercise the pinned player's real format callbacks; compare sample/input fields with the source-sized master and delivery descriptor. Preserve correct sample dimensions or capture and fix the failing provenance path. Do not assume the web result proves Android failure. |
| Known codec and height, unknown width | Display codec and explicit height without inventing a width or presenting the original source width. |
| Live TV planned scaling width | Preserve measured-aspect calculation and even rounding, verify encoder agreement, and label the result server-planned metadata. Playing resolution remains player-observed. |
| Cached VOD and server fallback to a lower rung | Descriptor follows the actual artifact/session, never the abandoned requested quality. |
| Prepared replacement and late old callbacks | Predecessor remains visible until commit; old callbacks cannot overwrite successor facts. |
| Same-session ABR change | A stale level's codec/dimensions cannot be mixed into the new rendition. |
| Live TV channel switch | No previous-channel format after the new lease attaches; missing older-server plan is explained. |
| Android offline playback | Local format collection works without a server or new network request. |
| Browser with absent/restricted output APIs | No permission prompt or routing mutation; explicit reason instead of a permanent generic unknown. |
| Apple speaker/Bluetooth/HDMI or AirPlay, as supported | Route and session values remain accurately labeled; no receiver/Atmos assertion is inferred. |
| Android playing, paused and route changed | Actual route only when observed; preferred device and supported-channel arrays never become current output facts. |
| No audio versus temporarily unavailable audio | Only a confirmed audio-free stream says `No audio stream`. |
| Mixed client/server versions | Optional fields do not break decoding or playback; absent fields get an explained fallback. |
| Repeated panel open/close | Observer counts remain bounded; no extra session, lease renewal, route change or repeated manifest fetch. |

Extend the existing web and native test locations identified in §3.4 with
behavioral adapter cases, not only string-presence checks. Add focused
server tests for actual response producers if P2 changes Rust. Compile iOS,
tvOS and Android for affected adapters; use physical devices to validate
route claims that simulators cannot establish.

Existing focused commands, run from the implementation checkout after its
base includes the matching redesign:

```bash
node tests/web/player-dom.test.js                 # Panel semantics and reachability.
node tests/playback/web-policy.test.js           # Existing web policy/field contracts.
node tests/playback/player-input-contract.test.js # Shared fixture/document agreement.
python3 -m unittest discover -s tests/operations -p test_docs_index.py
```

For contract edits, regenerate rather than hand-edit the embedded copies:

```bash
scripts/player-contract-table --write  # Refresh generated documentation.
scripts/player-contract-table --embed  # Refresh served contract tables.
```

Choose exact native test targets and focused Rust commands after mapping
the changed modules on the implementation base. Record them in the task PR;
do not substitute a future CI pass for the local compile loop.

## 10. Non-goals and review decisions

**Non-goals:** changing codecs or quality policy; altering HLS manifests to
make an info row look complete; negotiating routes; adding receiver control;
proving physical display resolution; discovering downstream Atmos decoding;
introducing general telemetry infrastructure; or rewriting player ownership.
These changes would expand a reporting repair into playback behavior.

The [client remediation plan §10](CLIENTS-REMEDIATION-PLAN.md#10-non-goals-and-guardrails)
records the native-master compatibility experiments reverted in
`97176881e`, including CODECS and resolution/range declarations. Preserve
the inspected revision's existing manifest behavior, including its later
HDR-specific declarations: do not reintroduce CODECS on SDR masters or
change RESOLUTION to repair this info panel. Whether the source-shaped
RESOLUTION should be corrected or removed is a separate playback-behavior
decision requiring AVPlayer re-qualification. The older plan's description
of wholly dormant codec plumbing is historical; E10 shows that the newer
snapshot does consume it for HDR.

Reviewers should resolve the following decisions before implementation:

1. **Approve explicit missing reasons.** This makes unimplemented reporting
   distinguishable from platform limitations and temporary startup state.
   The accepted cost is a small shared observation model and more precise
   wording across clients.
2. **Approve an optional attached-delivery descriptor for library playback.**
   This closes native-HLS/progressive gaps without inventing player evidence.
   Review its exact response locations, mixed-version decoding and variant
   identity before coding; the illustrative wire shape is not yet an API
   commitment.
3. **Approve route/session facts as partial audio-output information.**
   Useful route information may ship while receiver format stays unknown.
   Decide whether the visible label should remain `Device audio output` or
   become `Audio route & output`; retain the internal field ID either way.
4. **Keep collection read-only.** Panel visibility must not prompt, select
   an output, alter an audio session, probe media or renew a live lease.
   This preserves playback behavior while improving its explanation.
5. **Require attachment and rendition fencing.** It adds tests and adapter
   work, but prevents a more misleading result than an honest unknown:
   correctly formatted information about the stream that just stopped.

Remaining evidence gaps are the screenshot's deployed build and transport,
Apple input-format behavior on the supported OS/device set, Android sink
access and input-format provenance through the pinned Media3 integration,
and the final server response
map for replacement/cached sessions. None prevents the static RCA; all
must be settled where relevant before claiming the proposed collectors work.

## 11. Verification record for this document

On 2026-09-16, the matching worktree was clean at the commit in §2.1.
The original six-case projection check passed, and
`node tests/web/player-dom.test.js` passed there. These reproduce the
current formatter and verify existing presentation contracts only.

After review, the expanded seven-case reproduction passed, including the
known-wrong 4K value for a 1080p output. This is reproduction of B1, not a
passing fix acceptance test. Source comparisons also confirmed that
`playbackStatsTelemetry`, `master_playlist_with_shape` and `hlsTransport`
are identical between the anchor and `c9e4edf4`. This task did not rerun the
reviewer's three JavaScript suites against that later commit.

The four documentation-index tests passed against the **dirty, uncommitted
main working tree**, with the RCA untracked and the index already modified.
They are not evidence of a committed or releasable candidate. A
separate check verified this new, not-yet-tracked document's local links,
its index entry and the embedded reproduction command; the tracked-file
sweep alone would not cover a new untracked document.

No proposed collector, wire field, Rust change or native build was
implemented or qualified in this documentation task. Review and execution
remain open.

## 12. Review disposition — corrected findings and remaining evidence

| Review item | Disposition |
|---|---|
| B1, web provenance | Accepted. Headline, E10, reproduction, collection hierarchy, web adapter and P2 now address wrong transcode dimensions, not only missing values. The claim is scoped to source-shaped master metadata reaching the level. |
| B1, Android inference | Qualified. Keep the provenance requirement and physical 4K→1080p regression. The pinned Media3 single-variant/sample merge path prevents treating the review's `deriveFormat` inference as an established runtime defect (§3.2). |
| B1, test overstatement | Accepted. §3.4 explicitly states that existing panel tests do not validate either field's values or stream-format provenance. |
| S1, existing descriptor inputs | Accepted. Reuse `HlsContext.codecs` and existing `StartResponse.height/encoder`; add only missing exposure and recipe-known width, with partial-height presentation. Preserve current masters and the documented compatibility boundary. |
| S2, screenshot diagnosis | Partly accepted. Direct/progressive or missing level/probe data are first checks; no deployed transport is proved. The “AirPlay only” claim is corrected using the actual HEVC/fallback policy. |
| S3, Live TV width | Accepted. Calculated output width is server-planned metadata; measured-aspect derivation is allowed only when it agrees with the encoder's scaling and rounding. |
| Apple collector candidate | Accepted as a spike target, not a device-verified promise. Named item-track format descriptions and coded-dimension extraction in §5.2. |
| Dirty-checkout verification | Accepted. §11 explicitly distinguishes working-tree checks from committed-candidate qualification. |

This revision addresses the review; it does not assert reviewer approval.
