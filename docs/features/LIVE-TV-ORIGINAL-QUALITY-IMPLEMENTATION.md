# Live TV original quality — copy the broadcast, convert only what cannot play

**Status:** in progress · **Executes:** Paul's source-quality and ungated
delivery decisions of 2026-09-10 · **Written:** 2026-09-10

Build one bounded improvement to the existing tuner engine: preserve the
broadcast, including 2160p, whenever the playback path supports it. Convert
only the incompatible tracks or the quality characteristics an actual
constraint requires changing. Deliver server, web, Apple, and Android support
together in the three work packages in §9. No preliminary research programme,
new playback framework, or second planning phase is required.

Companion to [the original tuner plan](HDHOMERUN-LIVE-TV-PLAN.md),
[its hardware record](HDHOMERUN-LIVE-TV-STATUS.md),
[native layouts](LIVE-TV-NATIVE-LAYOUTS-IMPLEMENTATION.md), and
[the development pipeline](../DEVELOPMENT_PIPELINE.md). This plan supersedes
the tuner's mandatory 720/1080p H.264/AAC normalization contract. It owns the
4K and audio-preservation work excluded from the layouts plan; that exclusion
still bounds the layouts task. Preserve existing ownership, authentication,
cleanup, guide, and presentation behavior. Re-verify source anchors against
the integration base; the working tree used for this investigation contained
other work in progress.


## Execution ledger — one page carries the work to main

Updated 2026-09-10. This table is the live status page for the effort; update it
with the behavior commit that changes a row so the branch and its record never
diverge.

| Work package | State | Evidence / next action |
|---|---|---|
| Isolated clone and pinned compiler | done | Forgejo main `94b46515`; Rust 1.97.1 baseline `cargo check -p plurxd --locked --all-targets` passed. |
| Server observation and planner | active | Implement source facts, independent track actions, packaging, and source-aware admission. |
| Public and internal wire contracts | queued | Add bounded v1 playback envelope and negotiated internal v2 start. |
| Web, Apple, Android clients | queued | Send concrete live caps, describe actual delivery, and retry once without overlap. |
| Developer settings | queued | Original default, optional ceiling, and advisory route readiness; no gate. |
| Documentation and measurements | queued | Update maintained references and record only evidence actually observed. |
| Main promotion | queued | Sync current main, exact-tree compile, one adversarial review, fast lane, merge. |

## 1. Ship original quality as ordinary behavior

**Default:** Original quality / Auto. This means the least conversion needed
for this source, player, and explicitly established limit. It does not mean
"assume every decoder supports everything" or "always encode to 4K."

Every upgraded first-party client uses this policy immediately. Do not add a
feature flag, environment switch, build feature, account allowlist, experiment,
qualification bit, percentage rollout, or fleet-wide readiness requirement.
Keep the existing Live TV Enable/Disable control in Settings → Developer;
do not add a second switch for original quality. Requirements and met/unmet
observations may appear there as advice. They must not disable the control,
hide Live TV, or prevent attempting a route that can work.

Authentication, owner fencing, bounded resource admission, unavailable tuner
data, DRM, and an unavailable required decoder are real operation constraints.
Return an accurate error when an operation cannot succeed. A missing encoder
cannot block a copy session; an untested device cannot be classified as
ineligible merely because no acceptance record exists for it.

### 1.1 Quality is more than output height

| Source / constraint | Required result |
|---|---|
| 2160p HEVC Main 10, compatible display and player | Copy video at its original dimensions, frame rate, bit depth, and supported color/HDR representation. Repackage for the player. |
| Compatible video, unsupported AC-3 or other audio | Copy video; encode only audio into a supported format. Preserve channels if the output route supports them. |
| Unsupported video codec, but supported source dimensions | Encode into a supported codec at source dimensions; do not automatically reduce to 1080p. |
| Interlaced video without a usable client deinterlacing path | Deinterlace on the server and encode; preserve spatial resolution unless another constraint requires reduction. |
| Confirmed decoder, presentation, or explicit bandwidth limit | Change only what that limit requires, recording the reason. |
| User selects a lower maximum resolution | Apply a ceiling, never an exact resize target. |
| SD or 720p source with a 2160p ceiling | Keep source dimensions; never upscale. |
| HDR source and SDR-only presentation | Use an existing verified tone-mapping route if available; never relabel HDR as SDR or treat pixel-format conversion as tone mapping. |

Transcoding is lossy even at the same dimensions. "Original" describes a
preference; claim **Original video** only when the compressed video is copied.
A source that says 4K in the lineup is not proof that its actual stream is
2160p. The recorded antenna samples prove MPEG-2 and 1080-line HEVC Main 10;
they do not establish a received 2160p broadcast.

### 1.2 Scope stays small

Do not build DVR, rewind, shared channel ingest, multiple bitrate renditions,
seamless prepared switching, a new bandwidth estimator, decoder benchmarking,
a background channel scanner, or a general tuner-provider abstraction. Retain
one upstream connection per viewer session. Keep VOD session machinery and
its cache/index namespaces separate. Reuse its capability observations and
encoder helpers where appropriate, without fabricating a media-library file
record to make a live broadcast fit the VOD planner.

Do not make this depend on the layouts, Library channels, or playback-control
roadmaps. Coordinate shared DTO and Info changes directly. Unexpected needs
for a new HDR pipeline or player engine become separately scoped issues;
implement the applicable copy routes and report unsupported conversion
honestly rather than silently lowering quality or extending this effort.

## 2. The current implementation fixes the output before it sees the source

| Source anchor | Current behavior | Change |
|---|---|---|
| [LiveTvConfig](../../crates/plurxd/src/live_tv.rs), `from_snapshot` / `validate_static` | Defaults to 720; accepts only 720 or 1080. | Introduce an optional ceiling and stop treating the legacy setting as an automatic cap. |
| Same file, `run_live_session_inner` | Obtains encoder admission before opening the tuner. | Observe source, resolve the route, then reserve only needed processing resources. |
| Same file, `LiveTvTranscodePlan` / `live_ffmpeg_command` | Always software-decodes, filters, and encodes video. | Resolve independent video/audio actions and a container before command construction. |
| Same file, `live_video_filter` / `LIVE_HLS_OUTPUT_ARGS` | Exact-height scaling, 8-bit output, AAC stereo, forced keyframes. | Attach filters and codec arguments only to encoded tracks. |
| Same file, `parse_segment_name` / resource serving | Numeric `.ts` resources and MPEG-TS content type. | Add bounded fMP4 init/media resources without loosening resource authorization. |
| [TranscodeManager](../../crates/plurxd/src/transcode.rs), `admit_live_tv` | Reserves an estimated 2160p HEVC/HDR workload for every viewer. | Use measured source and resolved output workload; copy needs no video encoder permit. |
| [Public Live TV API](../../crates/plurxd/src/http/live_tv.rs), `start_session` | Accepts no client capabilities. | Accept an optional, bounded playback request. |
| [Internal Live TV API](../../crates/plurxd/src/http/internal_live_tv.rs) | Strict signed request shapes, existing activation and fencing. | Add a separately discoverable request version; preserve exact-body signatures. |
| [Core capabilities](../../crates/plurx-core/src/playback/caps.rs), `DeviceCaps` | Existing video/profile/presentation/audio observations. | Reuse, with live transport-specific limits below. |
| [Settings API](../../crates/plurxd/src/http/system.rs) | Stores output height in the generation-fenced tuple. | Add source-preserving policy semantics to that same tuple. |

The command currently selects 8,000 kb/s for 1080 and 4,000 kb/s for every
other height. Adding 2160 to the dropdown alone would therefore select the
wrong bitrate as well as retaining unnecessary video conversion.

## 3. Resolve one explicit delivery plan

Introduce a small pure live-delivery resolver, preferably in a narrow module
under the existing [Live TV module](../../crates/plurxd/src/live_tv.rs).
These are proposed interfaces, not existing Rust declarations:

```rust
fn resolve_live_delivery(
    source: &LiveSourceFacts,
    client: &LivePlaybackCaps,
    policy: &LiveQualityPolicy,
    available: &LiveExecutionSupport,
) -> Result<LiveDeliveryPlan, LiveDeliveryError>;
```

The plan freezes `video_action` (`copy` / `encode`), `audio_action` (`copy` /
`encode`), output codec and presentation facts, container (`mpegts` / `fmp4`),
required filters, execution workload, and a nonempty explanation for every
conversion or reduction. Command construction consumes this plan; it must
not independently choose a different encoder, size, channel count, or grade.

```text
 one authenticated tune
          │
          ▼
 observe source from the one bounded ingest
          │
          ▼
 intersect source, player transport support, explicit limits
          │
          ├── video + audio supported ──▶ copy both + package
          ├── audio unsupported ───────▶ copy video + encode audio
          ├── video unsupported ───────▶ encode video + resolve audio separately
          └── no workable route ───────▶ typed failure + complete cleanup
```

### 3.1 Source facts and capability facts are different inputs

Observe selected stream indices, codec/profile/level, width/height, pixel
format/bit depth, field order, rational frame rate, sample aspect ratio, color
primaries/transfer/matrix, audio codec/sample rate/channel layout, and available
HDR metadata. Include observed bitrate only with its measurement basis; a
short prefix does not establish the channel's peak rate. Unknown facts remain
unknown. Persist nothing to the library catalog.

Use [Apple Caps](../../clients/apple/Sources/Caps.swift),
[Android Caps](../../clients/android/app/src/main/java/tv/plurx/app/data/Caps.kt),
[Android CapsPolicy](../../clients/android/app/src/main/java/tv/plurx/app/data/CapsPolicy.kt),
and the web's existing capability probes. Check support in the actual live
player path: AVPlayer, Media3, or the selected browser HLS path. A codec name
alone does not prove that a transport, profile, level, bit depth, frame rate,
interlaced stream, or multichannel audio will work.

Add a live envelope around `DeviceCaps` rather than changing the meaning of
existing VOD fields. It carries supported HLS packaging/codec combinations,
per-codec frame-rate and interlace limits, and audio channel/output-route
limits. Do not take independent "supports HEVC" and "supports TS" claims to
mean HEVC-in-TS is supported. Missing claims cannot authorize copy, but they
also cannot disable the feature: resolve a conservative compatible route and
explain `client_capability_unknown` where it caused conversion.

Use source height and verified decoder limits, not the screen's CSS size or
an arbitrary TV/phone classification, to decide spatial reductions. Keep
frame rate rational through planning. For interlaced sports, server
deinterlacing should preserve field motion with a frame per field when the
client can sustain that output; record any frame-rate reduction separately.

### 3.2 Container choice is part of compatibility

Use MPEG-TS HLS for compatible H.264 and for MPEG-2 only where the actual
player path explicitly supports it. Use fMP4 HLS for HEVC on Apple and as the
common HEVC delivery route on other clients that support it. Apple requires
HEVC in fMP4; its HLS video list does not include MPEG-2 video. Android and
browser decode support is device/runtime dependent.

References: [Apple HLS authoring specification](https://developer.apple.com/documentation/http-live-streaming/hls-authoring-specification-for-apple-devices/),
[Media3 supported formats](https://developer.android.com/media/media3/exoplayer/supported-formats),
and [HLS.js supported formats](https://github.com/video-dev/hls.js/).
Recheck the repository's shipped versions when implementing.

### 3.3 Conversion preserves what it can actually preserve

Copy routes retain compressed elementary-stream content, with only required
container/bitstream representation changes. An audio mismatch never forces a
video encode. Video encoding does not automatically force AAC stereo.

For encoding, reuse existing encoder selection and verified color/quality
helpers. Preserve source dimensions up to actual decoder and explicit limits;
choose the highest supported presentation within those limits. Derive bitrate
or quality control from codec, output size, frame rate, and existing quality
policy rather than the two fixed live presets. Budget 2160p encoding as a real
2160p workload. Avoid upscaling, double deinterlacing, or invented HDR metadata.

Encoder scarcity returns the existing retryable capacity error. It does not
authorize an unrequested 4K-to-720p downgrade. A software fallback must fit the
existing workload admission rules. If a required HDR conversion has no proven
implementation, return a precise unsupported-route error; copying HDR to a
compatible client must still work.

For this delivery, bandwidth reduction follows an explicit saved/requested
limit or an already available reliable constraint. Do not infer a network
ceiling from "remote", Wi-Fi, tuner rate, one failed fetch, or one startup
stall. A new continuous adaptive-bitrate controller is out of scope.

## 4. Observe once, then feed the same bytes into the producer

Retain the pinned, proxy-free, redirect-free Reqwest tuner GET. Never give a
tuner URL to FFmpeg/ffprobe and never open a second GET to discover codecs.
The implementation should use a bounded replay prefix: collect initial bytes,
inspect that finite local sample, then feed those same bytes followed by the
remaining response into the selected producer.

**Initial engineering bounds:** at most 8 MiB retained, at most 3 seconds of
collection after the first byte, and at most 2 seconds for one local ffprobe
invocation. Stop collection earlier at the byte bound; do not wait to fill
8 MiB. Bound probe JSON to 256 KiB. These are ceilings to verify with the
focused cases in §10, not performance results or new settings. Keep the
existing 15-second no-data and 30-second feeding startup deadlines as total
outer bounds; observation, admission, and first publication all spend them.

Keep one owner of the HTTP response through observation and pumping. Join
probe tasks on cancellation and remove any finite sample file during normal
session cleanup. Either use a bounded memory prefix or a scratch file counted
in the session budget. Never accumulate a separate unbounded spool while
probing or waiting for encoder admission. Keep consumer backpressure bounded
and spend no extra tuner connection to compensate for a slow observer.

If facts are incomplete, no copy decision may rely on guessed profile or
color information. Prefer a conservative usable route when the necessary
source facts are known. If dimensions/presentation cannot be established for
safe conversion either, report `source_probe_incomplete`; do not quietly
declare the result original or select 720p. Decoder execution and packet
parsing errors remain distinct from no-data, DRM, and reception failures.

The acceptance case must demonstrate that the complete retained prefix is
replayed in order exactly once and that cancellation releases the tuner in
every observation state. Avoid a full new transport parser: use the installed
FFmpeg toolchain, with a small adapter and its established process lifecycle.

## 5. Keep live HLS bounded with copied keyframes and fMP4

Retain the existing six-entry live playlist and cleanup model. Keep the
four-second steady segment target, but accept source-keyframe-aligned variable
durations on copy routes. The present one-second forced startup keyframes
apply only to encoded video. A copy command must contain no video filter,
video encoder initialization, forced-keyframe argument, or video encode rate
control. Start from a usable random-access point; discard undecodable leading
video rather than publish it as independently decodable.

`independent_segments` is a claim about actual random access, not a way to
create keyframes. Verify closed/independent boundaries where the codec needs
more than a generic keyframe flag. Do not use `split_by_time` to manufacture
short copy segments. If no usable publication arrives inside the existing
startup deadline, report the reason or use the bounded compatibility recovery
in §7; do not lengthen every user's timeout to hide a bad stream.
See [FFmpeg HLS documentation](https://ffmpeg.org/ffmpeg-formats.html#hls-2).

Extend existing resource types to distinguish playlist, init segment, and
media segment. A session has a fixed packaging plan. Authorize only its fixed
init name and canonical numeric media names; retain regular-file checks,
bounded reads, exact peer authentication, deletion-lag authorization, and
same-origin client URLs. An `EXT-X-MAP` must name that session's authorized
init file. Reject external URIs, traversal, unexpected tags with resources,
and any init/media combination that does not match the session plan.

Use the appropriate MPEG-TS or MP4 content type. Count the init file, temporary
file, replay sample, listed media, and deletion lag in the same session byte
ceiling. Keep the 128 MiB ceiling initially: at 25 Mb/s, 24 seconds of media
is roughly 75 MB before overhead; long GOPs or higher rates can exceed it.
Measure high-rate 2160p fixtures and adjust only the named finite bound if
needed, with the actual calculation and no unbounded retention. Do not claim
all 4K bitrates fit a resolution-based estimate.

A material in-stream format change cannot reuse an incompatible init segment
or stale source claims. Detect it from producer diagnostics/output inspection
and terminate with `source_format_changed`; keep any recovery within §7's
single replacement allowance. Seamless mid-channel format switching is not
part of this delivery.

## 6. Extend wire contracts without a fleet activation barrier

### 6.1 Public request and delivery description

Keep `POST /api/v1/live-tv/channels/{channel}/sessions`. Permit an empty body
for existing clients. A new optional JSON body has this proposed shape:

```json
{
  "playback": {
    "v": 1,
    "caps": { "v": 2, "video": [], "audio": [], "containers": [], "transports": [] },
    "hls_formats": [],
    "video_limits": [],
    "audio_limits": [],
    "max_height": null,
    "max_bitrate_bps": null,
    "compatibility": null
  }
}
```

The arrays are empty only to illustrate the envelope: they claim no support.
Use the following typed entries, intersecting them with `DeviceCaps` rather
than letting them widen its codec/profile/presentation claims:

| Field | Entry shape and meaning |
|---|---|
| `hls_formats` | `{ "container": "fmp4", "video": "hevc", "audio": "ac3" }` claims this complete packaging/codec combination in the active player. Use normalized FFmpeg codec names. |
| `video_limits` | `{ "codec": "hevc", "profile": "main10", "max_width": 3840, "max_height": 2160, "max_frame_rate": { "num": 60000, "den": 1001 }, "interlaced": false }` describes a verified decoding path. The rate denominator must be positive. `interlaced: true` claims usable deinterlacing in that path. |
| `audio_limits` | `{ "codec": "ac3", "max_channels": 6 }` describes decode/render support on the current audio route, not just a codec installed on the device. |
| `compatibility` | Null on the first start; otherwise `{ "failed_video": false, "failed_audio": true, "failed_container": false }`. These are observed failed-route hints, not positive capabilities. At least one must be true. |

Document one real request per client in the API reference and share fixture
examples across the client tests. `null` on request ceilings means no explicit
request ceiling; positive values are ceilings. Combine a saved administrative
ceiling and a request ceiling by taking the smaller non-null limit. A request
cannot relax an explicit saved cap. Reject zero/negative values and malformed
versions. Unsupported positive format claims are ignored rather than enabling
a route the server cannot produce. Bound each array to 32 entries and each
codec/profile string to 32 bytes; normalize duplicates before planning.
Bound the full signed internal request to its existing 16 KiB allowance;
bound entry counts and string lengths so one client cannot enlarge peer work.

Add `delivery` to start, status, and Activity responses. Include observed
`source`, actual `output`, independent video/audio actions, packaging, and
structured `reasons` with stable codes and human-readable explanations.
Unknown source fields are null. Keep existing fields for older clients,
populated with truthful actual output where they can represent it. `encoder`
is absent/empty on video-copy routes; it must not pretend an encoder ran.

Reason codes cover at least video/audio incompatibility, unsupported
interlacing, display/HDR limits, explicit resolution/bitrate ceilings, unknown
client capability, and compatibility fallback. Container-only remuxing is
explained without calling it a quality reduction. Never infer source facts
from output metadata.

### 6.2 Internal transport and mixed versions

Current `LiveTvStartRequest` denies unknown fields. Add a v2 internal start
endpoint with the playback envelope, using the existing signed transport and
provisional/activate lifecycle. Advertise support as descriptive owner metadata
in an existing read response; it is transport negotiation, not a new feature
eligibility check. An absent advertisement means the old contract.

Never probe protocol support by sending a tuner-start mutation and then
retrying with another request shape. Select the endpoint before starting.
Keep request identity, source/user/owner fencing, idempotent recovery, and
body signatures intact. The same request ID with a different playback body
is a conflict; it cannot change an already admitted session's plan.

| Combination | Required behavior |
|---|---|
| New client / new ingress / new owner | Full negotiated delivery. |
| Old client / new owner | Empty-body compatibility route; explain absent capabilities rather than assuming source support. |
| New client / old server | Use the existing endpoint; if `delivery` is absent, report legacy delivery and keep playback working. |
| New ingress / old owner | Send the old signed shape; keep legacy output and report the owner version limitation. |
| Old ingress / new owner | Old internal endpoint remains supported; use its conservative legacy contract. |

Old clients can contain hardcoded H.264/AAC labels and cannot safely receive
arbitrary formats. Keeping that legacy route during mixed deployment is not
an experimental gate on new clients. Do not raise the existing cluster-wide
Live TV protocol requirement to activate v2. Unrelated voters must not prevent
a capable ingress/owner pair from using it. Preserve the existing ownership
and credential safety invariants; do not turn this into a cluster rewrite.

## 7. Recover once without overlapping tuner allocations

Extend the existing client lease/controller with one automatic compatibility
replacement per user tune. On a definite decode/container incompatibility,
retain the channel and presentation, stop the old player, and obtain confirmed
server cleanup before starting the replacement. Use a normal new start with
an explicit `playback.compatibility` hint identifying the failed video/audio/
container route. The owner selects the next usable plan; the client does not
construct FFmpeg arguments. Record the hint and resulting reason in delivery.

If failed audio is identifiable, preserve copied video on retry. Otherwise
use a conservative supported plan, retaining source dimensions where possible.
Do not retry authorization, DRM, tuner no-data, capacity, or arbitrary network
errors as codec problems. Do not persist a permanent device blacklist from
one failure. An in-session source-format change may spend the same allowance,
not a second independent retry budget.

Persist the retry-spent state with the existing interrupted-start barrier so
reload/reconnect cannot create an automatic loop. A fresh explicit user tune
resets it. Preserve the existing rule that an ambiguous start or unconfirmed
stop blocks another allocation until reconciled; a timeout is not proof of
cleanup. The old stream and replacement never hold tuner GETs concurrently.
This bounded stop/start can interrupt picture; do not build prepared switching.

If the replacement fails, show the specific failure and an ordinary Retry
action. Keep Stop available throughout. Test rapid channel changes and route
navigation against the existing serial/lease ownership behavior.

## 8. Migrate settings and show delivered quality honestly

### 8.1 Retire the forced height without inheriting an accidental cap

Add `live_tv.max_output_height` with public field `live_tv_max_output_height`.
Stored `0` means Original / no administrative ceiling; missing means `0`.
Allow explicit ceilings 480, 720, 1080, and 2160. Validate them in the existing
generation-fenced settings tuple, and apply changes on subsequent tunes using
the existing drain/configuration semantics. They must never upscale.

Keep the old `live_tv.output_height` key and public field solely for old-owner
and old-client compatibility during mixed deployment. New negotiated routes
ignore it. Do not copy its value into the new ceiling: the stored value does
not distinguish the old default from a deliberate quality preference.
Existing deployments therefore get Original without a migration job or
per-account activation. Old routes retain their old profile until upgraded.

Legacy writes still update the legacy field, not the new policy. New clients
write only the new ceiling for this setting; when connected to an old server,
display its actual legacy 720/1080 choice instead of pretending Original is
active. Add a concise migration note: previous forced heights apply only to
legacy playback; users wanting a ceiling should explicitly select one again.

Replace the current Developer output-height picker with **Maximum resolution:
Original · 2160p · 1080p · 720p · 480p**. Keep Original selected by default.
This is a quality preference, not permission to access original streaming.
No extra original-quality enable control is needed. A bandwidth override may
be carried by an existing player quality setting/API, but building a new
bandwidth settings panel is outside this delivery.

### 8.2 Requirements describe routes instead of refusing the whole feature

Reuse Settings → Developer readiness rows. Report tuner connectivity,
scratch availability, inspection/remux support, and available conversion
routes separately. "H.264 encoder unavailable; compatible streams can still
play at original quality" is useful. "Live TV unavailable" based only on a
failed encode probe is wrong. Do not open tuner streams for settings checks.

Show no build numbers, compiler checks, acceptance status, or fleet rollout
controls as product eligibility requirements. A missing implementation test
is an engineering fact, not something the viewer must enable around.

### 8.3 Use the existing Info and Activity surfaces

Replace hardcoded H.264/AAC text in
[Apple LiveTvView](../../clients/apple/Sources/LiveTvView.swift),
[Android LiveTvScreen](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvScreen.kt),
and [web](../../crates/plurxd/src/web/index.html). Consume one delivery record;
do not add metadata polling per layout. Coordinate its source fields with
the layouts plan instead of creating two competing channel metadata models.

Examples of user-facing output:

- **Original video · 2160p HEVC · HDR10 · AC-3 5.1** — both tracks copied.
- **Original video · 2160p HEVC · Audio converted to AAC stereo** — video
  copied, client cannot render the source audio layout.
- **Video converted · 1080p H.264** — source is MPEG-2; this player cannot
  decode it. Resolution is unchanged.
- **Reduced to 1080p · Your maximum resolution setting** — actual spatial
  reduction, with an attributable reason.

Do not label a remux "Direct play" if the server is packaging HLS. Use
**Original video**, **Audio converted**, and **Video converted**, with details
for packaging and individual reductions. Show source and output separately
when they differ. Unknown color/layout data gets no badge.

## 9. Deliver in three work packages, then one main promotion

Use one temporary `effort/live-tv-original-quality` integration branch. Task
branches use the repository's `codex/` prefix and target the current effort.
Keep one owner responsible for the integrated wire contract. This is a
sequencing plan, not a requirement for parallel agents or extra reviews.

### 9.1 Server: observation, plan, and actual bytes

Implement §§3–7 together: bounded source observation, independent track
actions, source-aware admission, copy/remux and encoding commands, fMP4
resource handling, additive public responses, and versioned internal starts.
Keep the existing tuner lifecycle and legacy route working throughout.

**Files:** the server/core anchors in §2 and
[two-node Live TV cases](../../crates/plurxd/tests/live_tv_two_node.rs).
Extract small planner/probe helpers only where they make the current large
module easier to reason about; do not reorganize unrelated streaming code.

**Complete when:** the pinned compiler loop passes; targeted fixtures show a
2160p HEVC copy command with no video encoder permit, audio-only conversion,
necessary video conversion at source height, usable fMP4 publication, and
unchanged bounded cleanup. See §10 for the focused cases, not a new CI suite.

### 9.2 Clients and settings: make the full path the default

Update all three clients' start requests, capability adapters, delivery DTOs,
Info/Activity labels, one-replacement logic, and Developer settings. Keep
Apple AVPlayer, Android Media3, and the existing browser player. Preserve
dock/PiP/fullscreen, pause behavior, channel coalescing, and Stop semantics.

**Files:** [Apple LiveTv](../../clients/apple/Sources/LiveTv.swift),
[Apple settings](../../clients/apple/Sources/LiveTvDeveloperView.swift),
[Android API](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvApi.kt),
[Android player](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvPlayer.kt),
[Android settings](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvDeveloperScreen.kt),
[web lease](../../crates/plurxd/src/web/live-tv.js), and the §8 display files.

**Complete when:** client builds compile, Original is the default after
upgrade, delivered 4K/source audio facts are truthful, old-server playback
works, and one failed copy route cannot create a retry loop or second tuner
allocation. Advance applicable Apple and Android build counters in the
implementation commits, including generated version surfaces.

### 9.3 Integrate, measure a small representative set, and finish

Update [API](../API.md), [Playback](../PLAYBACK.md),
[Features](../FEATURES.md), [Operations](../OPERATIONS.md), and the existing
tuner plan/status where the implementation changes their claims. Use this
document for the short completion ledger rather than adding separate status,
handoff, review-plan, and acceptance-plan documents.

Resolve collisions with concurrent layouts/settings changes in the effort.
Freeze task merges, merge current main, compile the exact integrated source,
and follow §11. Record measurements and device limitations in the delivery
report. Finish the effort once its agreed behavior is built; do not expand
the feature to chase unrelated channel/provider failures.

## 10. Focused verification proves the change, not an endless campaign

Add cases to existing live tests and scripts, not a new harness. Run the
small relevant author checks while developing; register broader playback
cases with the existing manually dispatched sweep. These cases are evidence
to collect, not new pre-merge CI dependencies or runtime feature gates.

| Case | Evidence required |
|---|---|
| 2160p HEVC Main 10 with supported audio | Output remains 2160p/10-bit with expected color metadata; video packets are copied, no video encoder reservation, fMP4 decodes on a compatible client. |
| Same video, audio deliberately unsupported by client caps | Video elementary content is preserved; only audio encodes, channel reduction is explained. |
| MPEG-2 unsupported / explicitly supported player paths | First encodes video; second copies only if the concrete container/player combination supports it. |
| Interlaced sports sample | Client deinterlacing copy where proven, otherwise correct server deinterlacing and declared output cadence. |
| 720p source with Original or 2160p ceiling | No upscale; coded picture dimensions unchanged. |
| 2160p source with explicit 1080p ceiling | Output at or below 1080, explicit setting reason; no unexplained audio downgrade. |
| HDR source with HDR / SDR clients | Preserve actual HDR on supported path; verified conversion or explicit unsupported result on the other. |
| Encoder pool exhausted or encoder unavailable | Copy still starts; a route needing encoding reports capacity/requirements without silent downscale. |
| Long GOP, missing facts, malformed transport, mid-stream format change | Bounded startup/state; no invalid independent-segment claim, guessed original badge, or stale init reuse. |
| Stop/cancel during prefix, probe, admission, publication, replacement | One GET at a time; no leaked child, sample file, permit, scratch, or ambiguous replacement allocation. |
| Old/new client, ingress, owner combinations | Exact signed shapes, deterministic route, accurate legacy description, no v2 fleet barrier. |
| Settings upgraded from missing/720/1080 legacy values | New Original default; explicit new ceilings persist and never upscale. |

For copy fidelity, compare demuxed elementary content with representation
normalization where needed; container bytes and packet boundaries need not be
identical after remux. Check metadata as well as pictures. A generated fixture
can prove 2160p delivery without claiming that the local antenna receives a
4K channel. A physical tuner sample proves reception/tuner behavior separately.

**Bound the manual measurement:** use an existing received MPEG-2 channel,
the known HEVC channel if still available, and one controlled 2160p Main 10
fixture. Include an interlaced and an HDR case in these samples where practical.
For the 2160p fixture, exercise representative Apple, Android, and browser
paths; unknown physical-device results stay marked unverified. Do three
starts per measured before/after route, and one five-minute steady run per
representative successful route. Re-run failed cases after fixes; do not
repeat the whole matrix after every edit.

Record source/output facts, selected actions and reasons, request-to-first-
picture time, server CPU/GPU use, encoder reservations, delivered bitrate,
scratch peak, dropped frames/stalls, and cleanup. Keep host, client, and sample
constant for comparisons. First segment alone is not first picture.

**How to read it:** a copy session with zero video encoder reservations and
preserved compressed video proves the principal optimization. CPU/GPU numbers
quantify its benefit; there is no invented percentage threshold. Startup may
still be dominated by tuning/probing/keyframes. Network bitrate may increase
when preserving MPEG-2 or higher-resolution source material. Report that
trade-off rather than calling a smaller transcoded stream higher quality.

Unavailable hardware does not disable shipping code or create a new merge
gate. Record the exact unverified case and leave physical qualification to
the existing explicit deployment/sweep process. Fix demonstrated defects
within this scope; do not declare unsupported playback successful.

## 11. Use the current CI/CD contract exactly

[AGENTS.md](../../AGENTS.md) and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) govern execution. The
2026-09-09 change was deliberate. Historical plans and broad validation
profiles are not permission to restore the old PR test graph.

1. **Compile before editing Rust.** Establish Rust 1.97.1, verify the actual
   `rustc --version`, and use the [source-only compile loop](../ci/AGENT-COMPILE-LOOP.md)
   if the checkout host cannot run the pin. Archive committed source with
   `git archive`; transfer no `.git` or credential. Keep the target warm.
   The investigation host reported Homebrew Rust 1.98.0, which is not the
   repository-pinned evidence. This document-only task makes no build claim.
2. **Commit normally; task PRs target the effort.** No pre-commit hook or
   hook installer. No required adversarial review or fast lane on task PRs.
3. **Complete paperwork with the change that owes it.** Add corrective
   regression evidence unless a changed test is direct evidence; update
   functionality-point ownership and the docs index; advance affected mobile
   counters. Align both counters and marketing versions only when making a
   workspace release. No paperwork-only repair commits as the planned process.
4. **Integrate current main, then recompile that exact tree.** Previous-base
   results do not prove the final branch. Check formatting, Clippy, Rust,
   web static contracts, and affected iOS/tvOS/Android compilation.
5. **Open the main-bound PR as draft.** Request exactly one adversarial agent
   review of the integrated result. Address every finding and verify the
   fixes as author. No second review, re-review, panel, or follow-up approval.
6. **Mark ready, then apply `fast-lane`.** Merge only after the current head
   has a green **Main promotion gate**. Remove `fast-lane` before returning
   to draft. Draft PRs execute no Forgejo checks or tests.
7. **Keep runtime suites in the separate sweep.** The fast lane contains
   policy/static contracts and affected compilation, not unit, integration,
   browser, simulator, emulator, recovery, playback, package, or smoke suites.
   Do not add the §10 matrix as a merge requirement. A sweep repair that
   changes product behavior needs a separate issue and the main-bound process.
8. **Merge and deployment are separate.** Normal main pushes neither run the
   full suite nor publish the fleet image. Use explicit deployment and the
   [release process](../RELEASING.md), including
   [client deployment](../clients/CLIENT-DEPLOY-PROMPT.md), when authorized.
   Release tags retain their separate qualification/publishing path. No
   original-quality enablement ceremony follows deployment.

Useful author commands; use the correct runtime and run only affected checks:

```bash
rustc --version                                      # verify 1.97.1
cargo fmt --all -- --check                           # formatting
cargo check -p plurxd --locked --all-targets          # daemon compile loop
cargo clippy -p plurxd --locked --all-targets -- -D warnings
make apple-build                                    # iOS + tvOS compilation
make validation-lint                                # catalog/path ownership
make history-check                                  # corrective evidence
make operations-check                               # repository static contracts
git diff --check                                    # whitespace
```

Use `./gradlew --no-daemon :app:assembleDebug` from the Android project in the
pinned Android build runtime. The fast lane checks the workspace where its
scope requires it; daemon-only author compilation does not replace that.
Do not run `make rust-check` or `make android-test` merely to obtain compilation:
those commands include runtime suites. Do not change CI triggers for this work.

## 12. Completion is a usable default, with evidence labeled accurately

The integrated implementation must preserve source video through 2160p when
the actual client path supports it; convert tracks independently when needed;
honor only explicit quality ceilings and real playback constraints; and retain
bounded ownership/cleanup. All first-party clients must request and describe
that behavior, with honest mixed-version fallback. Existing accidental
720/1080 defaults must not survive as caps on upgraded negotiated sessions.

Close with one report: merged commit, deployed versions if deployed, focused
checks actually run, representative measurements, and exact unverified device
cases. Mark this document **built** after integration lands; distinguish that
from deployed and physically verified. A deferred universal device matrix,
shared-ingest optimization, or seamless switching project is not remaining
work for this delivery.

