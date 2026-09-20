# Android Dolby Vision — why a capable Lenovo plays the HDR base

**Status:** Fable review addressed; repair implemented on `codex/android-dv-delivery`, validation pending ·
**Written:** 2026-09-20 UTC (incident: 2026-09-19 America/New_York) ·
**Revised:** 2026-09-20 UTC · **Scope:** Android Profile 7 → 8.1 transport
and preserved-DV progressive signaling, including native Profile 5/8 sources.

Read §1–§3 for the finding and evidence, §4–§6 for the proposed repair and
acceptance, and §7 for the disposition of Fable's review. The
[implementation contract](ANDROID-DV-CONVERSION-IMPLEMENTATION.md) turns
this diagnosis into one bounded PR under the current fast-lane workflow,
with no runtime enablement gates. This document proposes changes to delivery
selection and Android transport execution; it does not authorize a decoder override,
library rewrite, deployment, or unrelated playback-control redesign.

Companion to the [playback reference](../PLAYBACK.md) and
[shared-index handoff](STREAMING-SHARED-INDEX-HANDOFF.md). The older
[DV delivery findings](DV-DELIVERY-FINDINGS.md) describe an August snapshot
before the present conversion machinery; its statement that Plurx never
converts Profile 7 is not the September system's behavior.

## 1. Verdict — two executor defects turn a DV plan into ordinary HEVC

The Lenovo supports Dolby Vision. Its display reports Dolby Vision, HDR10
and HLG, and its non-secure hardware DV decoder enumerates Profiles 5 and 8.
The incident's decoder record nevertheless names `c2.qti.hevc.decoder` and
`video/hevc`, with parsed PQ transfer and HDR static metadata.

The server selected a copy remux with Profile 7 → Profile 8.1 conversion.
Android's ordinary remux branch opens the progressive `stream.mp4` endpoint.
That endpoint re-derives the same converting decision from Android's legacy
capability query, then carries only `preserve_dolby_vision` into its builder.
It has no conversion field or Profile 7 RPU conversion step. Capability
negotiation succeeds twice; the executor discards the conversion requirement.

The progressive builder also omits `-strict unofficial` on its ordinary
preserved-DV branch. FFmpeg 7.1 consequently omits the DV configuration box.
Media3 parses the remaining HEVC configuration as `video/hevc`, explaining
the selected ordinary decoder. This second defect also affects native
Profile 5/8 sources on that branch; routing only P7 conversions to HLS would
leave their progressive DV delivery broken.

There are four repair sites:

1. **Server admission:** the conversion decision does not require the HLS
   path that implements conversion. Existing `requires_hls` calculation
   checks progressive HEVC sample-entry constraints, not conversion needs.
2. **Android execution:** its `Delivery` DTO does not read `requires_hls`.
   Ordinary remux playback, reopen and seek select progressive transport.
3. **Progressive endpoint:** it must serve a truthful compatible-base fallback
   when the requested conversion cannot run on this transport.
4. **Progressive packaging:** preserved DV needs its configuration record and
   a source-aware sample entry, with admission matching the emitted tag.

The transport and packaging fixes belong together: writing a valid P7 record
without first preventing unconverted P7 delivery can turn today's HDR-base
playback into a decoder failure. Changing the badge or claiming more device
profiles would conceal the defect.

```text
source: P7 + HDR10 base + enhancement layer + RPU
  → decision: preserve DV, convert to P8.1, remux
  → Android: ordinary remux → stream.mp4
  → progressive builder: no conversion; preserves P7 RPU and enhancement layer
  → MOV muxer: dvh1 + hvcC, missing dvcC because strictness was not relaxed
  → Media3: video/hevc
  → tablet record: ordinary HEVC decoder, PQ + static HDR metadata
```

**Confidence boundary:** both defects and their causal mechanism are verified
from the code and upstream sources (§3.1). The resulting byte layout is a
source-derived prediction consistent with the retained decoder record; it is
not a captured incident stream. The record supports HDR-base decoding, not a
measurement of panel output. Scope the native P5/P8 finding to this affected
branch; there is no evidence that DV has never worked on any Android path.

## 2. Evidence — device, source, decision and decoder agree on the boundary

### 2.1 Revisions and collection limits

The serving node was identified from the decision and client events as `m6`.
Its running container label reported revision
`a2d9c2fb7b26e142a39b70da8cc0e57bbbdf878a` and image ID
`sha256:10690297399a20bedee58f165b672d20605dbb449dd4bc8d66f00723b571d125`.
The source inspection used checkout
`a603b4e26647ab0aa71564e7f4a5d624f9a87ff0`.

Fable's supplied review was **APPROVE WITH CHANGES**, verified against its
clone at `535f95d2`, deployed `m6`, read-only store/log observations, Media3
1.10.1 and FFmpeg 7.1. This revision rechecks the implicated local builders,
the live segmenter and the upstream sources. Fable's additional index and
240-hour log observations are attributed in §3.2; they were not independently
recollected for this revision. Review line numbers differ from this checkout;
function names are the stable anchors.

The relevant Android player/data files and server
[`stream.rs`](../../crates/plurxd/src/http/stream.rs) are unchanged between
those two revisions. The inspected HLS plan-review functions are also
unchanged; other HLS code differs. Reverify these anchors on the implementation
base. The APK reports version `0.3.0`, build `107`; its source revision was
not extracted, so build 107 is not independently proven byte-identical to
the inspected Android sources.

The initial node inspected was `nynuc`. Its recent Profile 8 decisions belonged
to another title and are **not** incident evidence. The matching decision and
client events below came from `m6`.

Evidence collection was read-only: server logs and SQLite opened with
`mode=ro`, plus paired wireless ADB dumps. No playback restart, app install,
display setting change, source rewrite or production mutation was performed.
The video decoder record is retained evidence from the reported playback,
not a claim that a decoder was still active during collection.

### 2.2 The Lenovo has the required display and decoder

| Observation | Captured value | Meaning |
|---|---|---|
| Model | `TB322FC` | Lenovo tablet from the deployment roster |
| APK | `versionName=0.3.0`, `versionCode=107` | Installed application build |
| Display HDR types | `[1, 2, 3]` | DV, HDR10 and HLG according to the app's Android constants |
| Display overrides | `isForceSdr=false`, `userDisabledHdrTypes=[]` | No observed forced-SDR or disabled-HDR override |
| Non-secure DV decoder | `c2.dolby.decoder.hevc`, hardware accelerated | A decoder exists for ordinary unencrypted DV playback |
| DV profiles | `32/2048 (DvheStn/8k60)`, `256/2048 (DvheSt/8k60)` | Profile 5 and Profile 8; no Profile 7 enumeration in this decoder |

The profile constants are mapped in
[`CapsPolicy.kt`](../../clients/android/app/src/main/java/tv/plurx/app/data/CapsPolicy.kt),
`DolbyVisionCodecProfile` and `dolbyVisionProfiles`. Runtime detection in
[`Caps.kt`](../../clients/android/app/src/main/java/tv/plurx/app/data/Caps.kt)
uses Media3's non-secure, non-tunneled decoder selector and the display HDR
types. An empty filtered capability logcat result did not supply the actual
request document; it must not be interpreted as an empty capability claim.

### 2.3 File 5418 is Profile 7; Profile 8 is the planned delivery

The reported title is identified here as **reference film T**, file `5418`.
The source row and stored probe on the serving node report:

```json
{
  "video_codec": "hevc",
  "width": 3840,
  "height": 2160,
  "bit_depth": 10,
  "hdr_format": "Dolby Vision · Profile 7 (HDR10-compatible)",
  "dv_profile": 7,
  "dv_level": 6,
  "dv_bl_compat_id": 6,
  "dv_el_present": 1,
  "dv_rpu_present": 1,
  "probe_color_transfer": "smpte2084",
  "probe_extradata_size": 795
}
```

At `2026-09-20T00:42:35.843005Z`, the server logged
`POST /api/v1/files/5418/decision`:

- `method=Remux`, `delivered_dynamic_range="dolby_vision"`,
  `preserve_dolby_vision=true`.
- Audio reason: `audio codec truehd unsupported`.
- Video reason: Profile 7 converted to Profile 8.1, with the HDR10 base copied
  and the enhancement layer dropped.

That reason proves the **selected plan**, not completed conversion. The
decision log prints legacy query fields as `unknown`, zero or empty for this
POST. Those values are not the v2 request's capability document and must not
be used to diagnose a missing decoder or an SDR display.

Android Media3 then logged file `5418`, method `remux`, HEVC, height `2160`,
and first frame after `3690 ms` at `00:42:37.957052Z`.

### 2.4 The matching video decoder record is ordinary HEVC

`dumpsys media.metrics` retained a video codec record at
`2026-09-19 20:43:27.023` device local time. Its app UID `10290` matches the
Plurx audio-focus record; its log-session ID matches the session created at
`20:42:36.694`, just before the incident's first frame.

```text
codec                         c2.qti.hevc.decoder
mime                          video/hevc
width × height                3840 × 2160
parsed-color-transfer         6
hdr-static-info               1
playback-duration-sec         48
```

Parsed transfer `6` is the PQ/ST2084 value used by the Android playback
policy. The record also contains a different `config-color-transfer` value;
do not collapse configured and parsed fields into one output measurement.
No matching use of `c2.dolby.decoder.hevc` was observed.

The Android badge uses `player.videoFormat` sample MIME and color transfer in
[`renderedRange`](../../clients/android/app/src/main/java/tv/plurx/app/player/PlaybackPolicy.kt)
and [the player screen](../../clients/android/app/src/main/java/tv/plurx/app/player/PlayerScreen.kt).
These are format signals, despite the parameter name `decoderMime`; they
are not a compositor measurement. The retained codec record adds independent
evidence beyond the user's HDR badge report.

## 3. Code trace — where the conversion disappears

| Step | Source anchor at the inspected checkout | Finding |
|---|---|---|
| Select P8.1 | [`playback/mod.rs`](../../crates/plurx-core/src/playback/mod.rs), `dolby_vision_converts_to_p81` around line 1168 | Requires source P7 conversion facts, enabled node conversion, client P8 and no P7 claim |
| Choose transport | [`stream.rs`](../../crates/plurxd/src/http/stream.rs), decision handler around line 2059 | `requires_hls` comes from `hevc_copy_requires_hls`; conversion is absent from this test |
| Encode delivery | Same file, `delivery_plan` around line 548 | Remux receives `stream.mp4`, an HLS sessions URL and `requires_hls` |
| Parse Android delivery | [`Models.kt`](../../clients/android/app/src/main/java/tv/plurx/app/data/Models.kt), `Delivery` around line 476 | No `requires_hls` property |
| Open and seek | [`Controller.kt`](../../clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt), `restartAt` remux branch around line 1613; `executeSeek` around line 1207 | Ordinary remux goes through `remuxUri`; transport predicates also equate remux with progressive |
| Execute progressive copy | [`stream.rs`](../../crates/plurxd/src/http/stream.rs), `stream_mp4` around line 2466, `RemuxSpec`, `progressive_hevc_copy_args` | Recomputes a decision, then passes preservation without conversion to the ffmpeg remux |
| Execute HLS conversion | [`hls.rs`](../../crates/plurxd/src/http/hls.rs), `apply_plan_review` around line 882 | Sets `SessionKind::Copy.convert_dolby_vision` from the server's review |
| Build converted media | [`vodserve.rs`](../../crates/plurxd/src/vodserve.rs), `with_dolby_vision_conversion`; [`transcode.rs`](../../crates/plurxd/src/transcode.rs), conversion-aware copy recipes | Existing conversion machinery is downstream of session delivery |

`hevc_copy_requires_hls` returns false when a v2 client omits
`progressive_hevc_sample_entries`. Android does omit it. Adding a conversion
condition only inside that sample-entry helper's optional branch would
therefore still miss Android.

Android's [`progressiveRemuxUri`](../../clients/android/app/src/main/java/tv/plurx/app/player/TrackSelection.kt)
appends its capability query, including `dvprofile`, to the progressive URL.
The conversion is not lost by v2-to-legacy translation: the endpoint's
re-derived verdict still converts, but `RemuxSpec` takes preservation alone.

### 3.1 Missing strictness explains why Media3 selects HEVC

The source's 795-byte extradata does not trigger the 23-byte minimal-hvcC
promotion branch. With DV preservation selected, the ordinary progressive
builder produces this relevant subset:

```text
-c:v copy -tag:v dvh1 -bsf:v filter_units=remove_types=32-34
# No -strict unofficial; NAL types 62 and 63 remain.
```

The format-blind `hevc_copy_tag` treats the source as lacking a compatible
base. The source-aware helper used for segmented copies would retain `hvc1`
for this HDR10-compatible source. Both helpers and the bitstream filters are
in [`transcode/mod.rs`](../../crates/plurx-core/src/transcode/mod.rs).

FFmpeg's MOV muxer writes the DV configuration only with the required
strictness setting; otherwise it omits the box and logs a warning. Plurx
runs this progressive child with `-loglevel error`, suppressing that warning.
The segmented copy builder already sets strictness for every preserved-DV
copy. [FFmpeg 7.1 MOV writer](https://github.com/FFmpeg/FFmpeg/blob/n7.1/libavformat/movenc.c).

Media3 parses a DV configuration child before setting the Dolby Vision MIME;
the sample-entry name alone does not establish it. With `hvcC` and no parsed
DV record, this track remains HEVC. This explains the incident decoder record
without assuming the tablet lacks DV support.
[Media3 1.10.1 box parser](https://github.com/androidx/media/blob/1.10.1/libraries/extractor/src/main/java/androidx/media3/extractor/mp4/BoxParser.java).

Merely adding strictness is unsafe for the P7 case: a source P7 record would
then be exposed to a device enumerating only P5/P8. Media3's alternative HEVC
decoder selection excludes Profile 7, so the current accidental fallback
cannot be relied on after fixing the record.
[Media3 1.10.1 decoder selection](https://github.com/androidx/media/blob/1.10.1/libraries/exoplayer/src/main/java/androidx/media3/exoplayer/mediacodec/MediaCodecUtil.java).

For preserved native P8.1, the same missing record loses DV identification
without any conversion being required. Native P5 is affected by missing DV
identification too, but it has no ordinary HDR10-compatible base; do not
describe its failure as a valid HDR10 fallback. The promotion branch already
sets strictness and is a separate regression case.

### 3.2 The first conversion acceptance must exercise live recovery

Fable reports no converting fragment index for file `5418`: the read-only
store had a legacy artifact and one non-legacy identity, identified as the
stripped variant; no local artifact was recorded on `m6`. This is a dated
review observation, not a permanent property. Recheck the exact converting
identity at acceptance, rather than infer availability from artifact count.

Without that identity, VOD returns `vod_index_pending`. With live recovery
enabled, a fresh HEVC copy goes through
[`copyseg.rs`](../../crates/plurxd/src/copyseg.rs). Since `d5ff3338`, which is
in the deployed revision, `start_copy_with_audio_offset` retains conversion
for a fresh GOP-aware segmenter. Takeover and the legacy muxer still narrow
through `served_copy_options`; those paths cannot be called converted DV.

The live converter rewrites fragment RPUs, excludes enhancement-layer NAL 63,
and writes profile 8, the original level, no enhancement layer and base
compatibility ID 1 through
[`converted_dolby_vision_record`](../../crates/plurxd/src/fragindex.rs).
That machinery exists; a request flag alone does not prove it ran successfully.

Fable found zero conversion-start log lines in the inspected 240 hours on
`m6`, `nynuc` and `nuc4`. This establishes **no production evidence found in
that window**, not that conversion has never run anywhere. Treat the device
acceptance as the first evidenced production exercise available to this
review. A converter refusal is a failed run, even if create returned P8.

For the expected rolling session, seeks and track changes can create fresh
sessions and converter warm-up. Validate that churn explicitly. Immutable
VOD is a separate secondary acceptance case after a converting index is
available; do not build an index merely to bypass the live-path test.

## 4. Proposed repair — require a transport that implements the plan

### 4.1 Make conversion an independent HLS requirement

For a remux whose decision has `convert_dolby_vision=true`, return
`delivery.requires_hls=true` and the existing sessions endpoint. Combine
this requirement with the existing sample-entry requirement; do not replace
the latter. Evaluate conversion independently of optional sample-entry claims.

For a v2 client without HLS, narrow conversion eligibility before constructing
the verdict. For a source/client/node combination that admits the compatible
HDR10 base, return the existing strip verdict, `delivered_dynamic_range=hdr10`,
no delivered DV profile, and the existing metadata-removal reason. A playable
HDR10 fallback must not become a new transport-refusal black screen.

Carry the transport fact from `DeviceCaps.transports` into the effective
profile or equivalent planning context. It must not be inferred from a
manufacturer name or a generic DV bit. Preserve explicit P7 enumeration and
legacy HLS-create behavior. In particular, do not default every legacy request
to no-HLS: a legacy session create is already an HLS request, while a legacy
progressive GET has the concrete limitation addressed in §4.3.

Only narrow when the fallback is actually admissible: source base grade,
display, HEVC decode and node stripping capability must still agree. If no
compatible copy exists, retain the existing supported-transcode/refusal
policy; never manufacture HDR10 from a P5 source or an SDR display claim.
Existing sample-entry admission errors remain independent of this change.

### 4.2 Carry transport through every Android media attachment

Add `requires_hls: Boolean = false` to `Delivery`, preserving older-server
compatibility. Carry it into the plan and effective playback recipe. Use one
transport selection rule for initial open, resume, seek, audio changes,
subtitle changes, prepared replacements and recovery.

Prefer a derived `recipe.progressive` property or equivalent shared predicate
over changing the meaning of `SubtitleDelivery.usesPlanTransport`. Transport
requirements belong to the recipe, not to whether subtitles happen to be on.
All three remux tests (`progressiveTransport`, `executeSeek`, `restartAt`)
must consult it. Subtitle Off after `NativeSession` must retain required HLS.

When required, open a **copy-video HLS session** with the existing caps
document, selected audio, audio offset, preservation flag and start position.
Let the server derive conversion; do not add a client-authoritative
`convert_dolby_vision` override. Reuse the copy-session body builder in
[`SubtitlePolicy.kt`](../../clients/android/app/src/main/java/tv/plurx/app/player/SubtitlePolicy.kt)
without pretending a subtitle is selected to force the route.

Update `progressiveTransport`, seek dispatch and media-origin handling along
with startup. A fix only in `restartAt` would let the next seek leave HLS.
Keep delivery mode `remux`; HLS packaging does not mean video transcoding.
Actual session delivery fields supersede the original decision for the badge.
Existing subtitle burn/HDR restrictions continue to apply.

### 4.3 Narrow progressive conversion requests to a clean compatible base

At `stream_mp4`, before registering delivery or starting ffmpeg, derive the
served recipe under the endpoint's actual capabilities. If the request asks
for conversion and a compatible HDR10 strip is supported, serve that copy
with both preservation and conversion false. Remove RPU/EL NALs and stale DV
configuration; emit the appropriate HEVC sample entry and HDR signaling.
Retain the actual selected audio, start and offset.

Log `asked_preserve`, `asked_convert`, `served_preserve`, `served_convert`,
served grade and the transport-limitation reason, following the existing
asked/served pattern in the copy-session path. Status and any delivery
metadata must describe the served recipe. Android build 107's format-based
badge can read this HDR result; that does not prove every other caller
corrects an old decision badge. Test exposed metadata separately from pixels.

Use this narrowing even when a caller claimed HLS but opened progressive.
There is no new 409 for ignoring `requires_hls` while an honest compatible
base is available. Stale APKs and saved URLs retain a playable picture, and
server/APK deployment need not be coupled. If stripping cannot be performed
correctly, follow existing capability-based failure handling rather than
send orphan DV metadata under a fabricated HDR10 label.

### 4.4 Preserve configuration on native-DV progressive copies

Emit `-strict unofficial` for **every served preserved-DV copy**, not only
the minimal-hvcC promotion branch. Evaluate it against served options after
§4.3 narrowing. Ship that narrowing and this packaging fix together: exposing
an unconverted P7 record to a P5/P8 device is not an acceptable intermediate
server state.

For ordinary non-promotion copies, pass structured source compatibility into
the builder and use `hevc_copy_tag_for_source`: compatible P8.1 stays `hvc1`,
while non-compatible P5 stays `dvh1`. Change
`progressive_hevc_output_tag` in the same patch so admission predicts the tag
actually written. Preserve the separate in-band parameter-set handling for
minimal-hvcC sources; do not replace its `dvhe`/`hev1` branch indiscriminately.

Test both emitted records and tags. Merely finding `-strict` in an argv does
not prove a valid DV record survived the full muxing chain. This closes
native-P5/P8 progressive delivery without a blanket HLS migration.

### 4.5 Reuse the segmenter's conversion and add bounded diagnostics

The conversion primitives already exist in
[`dvpipe.rs`](../../crates/plurxd/src/dvpipe.rs) and the configuration-record
writer. Progressive conversion would duplicate `copyseg`'s box-boundary
framing and require another pipeline identity kept in step with
`DV_CONVERSION_TRANSFORM_REVISION`. Existing HLS sessions also provide the
attribution, stop and status machinery. Reuse them instead of building a
second converting stdout relay.

Add normalized v2 client kind/build, presented transfers, DV profiles,
display-DV bit and transports alongside the legacy fields in the existing
decision log. At progressive start, log the output-affecting video options
and asked/served transformation once. A sanitized argv may be included, but
unredacted media paths, credentials and URL queries are not required to
diagnose this failure. Avoid per-fragment logging for this addition.

## 5. Acceptance — prove the wire format, then the tablet

| Case | Required result |
|---|---|
| P7 convertible source, Lenovo-like P5/P8 caps, no converting index, live recovery enabled | Remux + required HLS; rolling session; output record is P8.1, RPUs are converted, no NAL 63 |
| Same source with exact converting index available | Immutable VOD uses the converting identity and emits the same correct grade |
| Same source, conversion disabled or required source facts absent | Existing truthful fallback/refusal; no P8 claim without conversion |
| Native P8.1 over ordinary progressive | `hvc1` + HEVC configuration + DV profile-8 record; DV MIME and decoder on the Lenovo; no new conversion requirement |
| Native P5 over ordinary progressive | `dvh1` + valid profile-5 record; DV MIME and compatible decoder; no fake HDR10 fallback |
| Minimal-hvcC preserved DV | Existing in-band sample-entry contract and DV record survive; admission matches actual output |
| Client explicitly enumerates P7 | Preserve its existing exact-profile policy; do not force P8 solely from the source label |
| Convertible P7, v2 client has no HLS but admits HDR10 copy | Strip verdict and bytes, `hdr10`, no DV profile and no new refusal |
| Old caller or HLS-capable caller opens progressive for a conversion plan | Clean stripped HDR10 bytes where supported; narrowing logged and served metadata truthful |
| Required stripping capability unavailable | Existing admissible fallback/refusal; no orphan DV configuration or unfulfilled HDR10 promise |
| Seek, pause/resume, audio/subtitle change and prepared replacement | Required HLS remains in the effective recipe; no accidental progressive reopen or SDR rescue |
| Subtitle Off after native subtitle session | Still HLS when required, including the next seek |
| Takeover or legacy copy muxer | Truthful narrowing or existing failure handling; never claim a conversion that arm cannot perform |
| Older server omits `requires_hls` | Existing Android behavior remains compatible |
| Session cannot actually produce DV | Delivery/status report the achieved grade or refusal, never the abandoned decision's P8 claim |

Backend tests must cross the decision/endpoint boundary, not merely assert
that a new boolean equals the decision's boolean. Include a P7 fixture with
compatibility ID `6`, level `6`, RPU and enhancement layer, matching this
incident. **The primary run must have no converting index**, and must verify
that session status identifies rolling delivery. Capture its complete init
and a complete playable fragment. Parse profile 8, compatibility ID 1, RPU
and base present, enhancement layer absent; inspect converted RPUs and
absence of NAL 63. Verify no re-encode of the base video. A converter error
fails acceptance even if the create response declared DV. Test indexed VOD
separately, with an exact matching identity rather than a generic indexed bit.

Android tests should deserialize a real delivery JSON fixture and exercise
transport selection for the lifecycle cases above. Start with required-HLS
seek and subtitle-Off, where independent dispatch tests can undo the repair.
Extend the existing playback, subtitle and prepared-replacement suites;
exact new test names are implementation work.

Before Rust edits, establish the pinned Rust 1.97.1 loop in the
[development pipeline](../DEVELOPMENT_PIPELINE.md) and
[agent compile loop](../ci/AGENT-COMPILE-LOOP.md). The local default inspected
during diagnosis was Rust 1.98.0, which is not pinned-toolchain evidence.
Run focused Rust regressions, check, Clippy and formatting against the actual
candidate before pushing. Android's relevant build commands are:

```bash
cd clients/android
./gradlew testDebugUnitTest :app:assembleDebug :app:lintDebug
```

Physical acceptance on `TB322FC` must retain three separate facts: requested
plan, emitted stream configuration/RPUs, and selected decoder/format. Play
file `5418`, confirm the DV-capable decoder/format is selected, then exercise
seek and one audio/subtitle change, including subtitles Off. Retain the live
session's init/fragment and matching `dumpsys media.metrics` record; expected
Lenovo codec/MIME are `c2.dolby.decoder.hevc` and `video/dolby-vision`. Run a
native P8 progressive title as a separate case. Capture compositor/display
evidence if claiming panel output; a badge alone does not establish it.
Do not interrupt an unrelated active viewing session to collect acceptance.

For an optional before-fix wire capture, use the observed capability query,
audio index and serving node. Do not substitute the review's example
`audio=3` for the incident's selected audio `0`. Read bounded complete MP4
boxes through the entire `moov`; a missing ASCII string in the first 4096
bytes does not prove that a configuration box is absent. Parse a full media
fragment separately to verify RPUs and enhancement-layer NALs. Such a request
starts another delivery and may update playback activity; it is not merely
a read of retained diagnostics. No such request was made for this revision.

## 6. Scope — no device exceptions or library conversion

No Lenovo model allow-list, widened DV profile claims, forced decoder choice,
on-disk conversion, library rescan, blanket HLS migration, or badge-only fix
is proposed. The tablet already reports the needed capability and the source
already carries the structured conversion facts. These changes would either
hide the delivery defect or enlarge the repair unnecessarily.

The repair also includes native-DV progressive configuration and matching
sample-entry admission (§4.4); it is not limited to conversion transport.
It touches the server delivery contract and Android's shared recipe
execution. The implementation contract keeps those changes in one ordinary
main-bound PR, with focused local checks and the current fast lane. It does
not invoke the disjoint-file exception or require a multi-PR effort.

## 7. Fable review — accepted changes and retained evidence limits

| Finding / original question | Disposition in this revision |
|---|---|
| Q1: where conversion is lost | Accepted: both negotiations reach the converting verdict; the progressive executor ignores conversion (§1, §3) |
| Missing strictness and native P5/P8 defect | Accepted and brought into scope; fix configuration and source-aware tagging with the P7 endpoint narrowing (§3.1, §4.4) |
| Q2: HLS versus progressive conversion | Existing copy HLS retained; rationale now identifies duplicated framing/identity rather than absent conversion primitives (§4.5) |
| Q3: refuse no-HLS clients and progressive conversion GETs | Original proposal withdrawn; use truthful compatible-base narrowing, including callers that advertised HLS (§4.1, §4.3) |
| Q4: lifecycle routing | One recipe transport predicate; seek and subtitle-Off are first-class regression cases (§4.2, §5) |
| Q5: live versus VOD conversion | Fresh live conversion since `d5ff3338` confirmed in code; live-without-index is primary acceptance, indexed VOD separate; takeover/legacy narrowing remains explicit (§3.2, §5) |
| Q6: logs | Add normalized v2 facts and sanitized output recipe/argv; no credential or private-path logging needed (§4.5) |
| Historical production claim | Limited to Fable's inspected 240-hour/node window; absence of logs is not proof of no earlier production conversion (§3.2) |
| Exact emitted incident bytes | Mechanism supported by code/upstream source; byte layout remains source-derived until a capture exists (§1, §3.1) |
| Suggested short-prefix capture | Useful clue, insufficient absence proof; parse complete boxes and retain exact request choices (§5) |

The review's recommendations are incorporated. The implementation contract
specifies request-scoped transport capabilities for legacy/v2 planning and
served-grade metadata for progressive fallback. Building that contract and
collecting the acceptance evidence remain outstanding. These are
implementation obligations, not a reason to
restore the withdrawn 409 proposal. The checked-in policy must describe the
served grade even when a client UI continues displaying an earlier plan.

**Delivery status:** the server and Android repair is implemented and compiles
on the pinned Rust and Android toolchains. The merge-boundary adversarial
review, focused validation, fast lane, emitted-media capture, deployment and
post-fix Lenovo run remain pending; none is claimed as evidence yet.
