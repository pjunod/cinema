# HEVC-in-MP4 admission — reference film K's decoder evidence and the reviewed fix

**Status:** open · review findings incorporated; runtime fix not implemented
· **Written:** 2026-09-15 · **Revised:** 2026-09-16 UTC · **Executes:** the
accepted direction from Fable's 2026-09-16 “Approve with changes” review;
[HEVC-SAMPLE-ENTRY-IMPLEMENTATION.md](HEVC-SAMPLE-ENTRY-IMPLEMENTATION.md)
is the build contract for Sol.

Companion to [PLAYBACK.md](../PLAYBACK.md) (the delivery decision),
[PLAYBACK-CAPS-V2-PLAN.md](PLAYBACK-CAPS-V2-PLAN.md) (capability semantics), and
[DV-DELIVERY-FINDINGS.md](DV-DELIVERY-FINDINGS.md) (Dolby Vision delivery).
This document records the diagnosis and its review, not the executable task
sequence. Reference film K is the reproducer; the defect also affects admission of SDR
and HDR10 HEVC-in-MP4 on web and Apple clients. Read this evidence before
the linked implementation contract. If implementation requires rewriting
library files, bypassing
dynamic-range negotiation, or changing playback-control recovery, stop:
those actions are outside this fix.

Source diagnosis was checked against main `3129ce993` in the earlier
investigation; the documentation checkout is `10f2afe60` with unrelated
working changes. Source links below name symbols rather than unstable line
numbers. Re-verify them against the intended implementation base. Neither
commit is asserted to be the deployed server's exact build.
The stale documentation checkout is not an implementation base: Sol must
start from fresh main and re-resolve the named symbols. The retained filename
keeps the original investigation's scope; the title and scope
have been corrected after review.

## 1. Finding — the source packaging is admitted on the wrong evidence

The affected source is **Reference film K**, library file ID **9** in the
investigated installation. Its video is HEVC Main 10, 3840 × 2076, with an
MP4 `hev1` sample-entry label. Fable's stored-row inspection reports
`extradata_size: 132`, DV Profile 8, `bl_compat_id: 1`, `el_present: 0`,
`rpu_present: 1`, approximately 31.6 Mb/s, and eight E-AC-3 audio tracks.
The video is the first stream; an attached MJPEG cover is stream 23.
The 132-byte configuration is not the minimal 23-byte hvcC record that
triggers parameter-set promotion. This makes reference film K the complete-hvcC case,
not proof of the minimal-hvcC delivery path.

**One-line root cause:** the code applies its HEVC packaging compatibility
rules when remuxing, but not when admitting the source to direct playback.

The web capability probe asks about `hvc1` codec strings, then reports the
broader codec family `hevc`. The server's direct-play decision checks that
family and the container, but does not carry the source's HEVC sample-entry
label into the decision. A positive answer for the probed packaging is
therefore treated as permission to send differently packaged source bytes.

The native decoder experiment isolates this mismatch:

- A 12-second copy of the source video labelled `hev1` cannot start decoding
  through AVFoundation on the investigation Mac.
- The identical compressed video labelled `hvc1` decodes 48 frames.
- Changing only the sample-entry label is sufficient for that result;
  changing only the other remux metadata while retaining `hev1` is not.

**Conclusion:** the label is a demonstrated cause of the native decoder
failure for this sample and machine. Combined with the direct-play trace
and the capability mismatch, it is the leading, evidence-backed explanation
for the Safari incident. A full Safari browser A/B and acceptance of the
actual server remux remain outstanding. Do not report that the complete
Safari fix is already proved, or that every Safari version rejects every
`hev1` file.

**Proposed correction:** make progressive HEVC packaging compatibility an
explicit input to delivery selection. When the source packaging is not
admitted but the video itself is supported, use the existing compatible
copy-remux path. Do not re-encode video merely to change its envelope.

### 1.1 The review found 171 potentially affected library rows

Fable reports a read-only census of the live store on nuc4, selecting the
first non-attached video stream from stored probe JSON:

| Source | Tag | Rows | hvcC bytes |
|---|---|---|---|
| MP4 HEVC SDR | `hev1` | 111 | 111–1060 |
| MP4 HEVC compatible DV P8 | `hev1` | 28 | 115–2663 |
| MP4 HEVC HDR10 | `hev1` | 10 | 23 |
| MP4 HEVC DV P5 | `hev1` | 1 | 127 |
| MP4 HEVC DV P5 | `dvhe` | 21 | 23 |
| MP4 HEVC | `hvc1` | 0 | — |
| MKV HEVC | zero placeholder | 2229 | outside direct MP4 admission |

These are review-supplied population facts, not an independently repeated
census or 171 device-confirmed failures. The common case is SDR; a DV-only
mitigation misses it. Apple currently requests HLS normalization for
preserved DV, but that exception does not cover SDR or HDR10. The same
admission gap is present in its capability/decision path; physical Apple
device failure and repair still need acceptance evidence.

### 1.2 Review disposition — what changed in the build contract

| Finding | Disposition |
|---|---|
| F1: scope and SDR population | Accepted: generic packaging admission, SDR and DV regression siblings; no reference film K/file-ID special case |
| F2: Apple producer | Accepted: web and Apple capability changes ship in one PR; Android initially omits the field |
| F3: two copy builders | Accepted: name and test progressive as well as segmented output; exclude incompatible progressive delivery |
| F4: reference film K configuration | Accepted: retain the 132-byte hvcC and stored DV facts above |
| F5: persistence | Accepted: nullable column and bounded M2-style backfill; no SQL-JSON projection; no fragment-index identity change |
| F6: avoid unnecessary Chrome remux | Accepted with guard: probe each label for progressive playback, not MSE; predicted browser answers require measurement |
| F7: Original reason | Accepted: assert both final method and packaging reason |
| F8: no legacy query addition | Accepted: leave LegacyCaps/CAPS_Q unchanged; prevent rejected new claims from downgrading to GET |
| F9: no transport-system redesign | Accepted: narrow HTTP/execution guard and explicit 409 for incompatible progressive-only delivery |
| F10: causal conclusion | Accepted with existing native-vs-browser evidence limits retained |

Two additional code checks refine the review: existing HEVC probes OR
progressive and MSE results, so they cannot directly populate a
progressive-only claim; `askDecision` falls back on every POST 400, so new
capability validation must not become a bypass through legacy GET. Both
have explicit work and regression tests in the implementation contract.

## 2. Separate the three incidents before reviewing the fix

| Incident | Observed path / defect | Relationship to this proposal |
|---|---|---|
| reference film K in Safari | Direct MP4 playback; no HLS session or playback-control reporter in the observed attempt; native `hev1` decode failure | This document |
| Repeated seeking / `invalid_control` | Valid old-buffer/new-target snapshots rejected; backward-seek gaps could also inflate producer runway | Separate [draft PR #336](http://192.168.4.7:3000/noirr/plurx/pulls/336), as of this investigation |
| Chrome startup with text subtitles | Separate startup/subtitle handling defect | Addressed by [PR #335](http://192.168.4.7:3000/noirr/plurx/pulls/335); not proof of Safari compatibility |

Routing reference film K through HLS will make the seek/control path relevant to its
future playback. That does not make the earlier direct-MP4 failure a control
protocol failure. Acceptance should use a candidate that includes the seek
repair, or explicitly report that dependency as unqualified.

## 3. Evidence — observations, controls, and their limits

### 3.1 Incident observations support a decode problem, not a control failure

The earlier read-only telemetry investigation found Safari direct-play
attempts around **2026-09-16 00:23–00:29 UTC**. They recorded a startup/TTFF
event after roughly 2–3 seconds, then an approximately 8-second stall with
zero or near-zero buffer, a restart, and eventual recovery exhaustion.
The earlier browser investigation also reported zero presented video frames
for 45 seconds when opening the original media URL outside the application.

These are retained investigation observations, not a fresh browser replay
performed while writing this document. The TTFF event is not proof that a
decoded picture reached the display; it conflicts with the frame evidence.
Do not use that event alone as the acceptance metric.

There was no HLS control session in the observed direct attempt. The original
bytes are served by `direct` → `serve_file_range` in
[http/stream.rs](../../crates/plurxd/src/http/stream.rs), not by the copy
pipeline that already normalizes HEVC delivery.

### 3.2 The four-way decoder comparison isolates the sample-entry label

The diagnostic artifacts are local, temporary, and not committed media:
`/private/tmp/plurx-avatar-safari.Yxy2yT/`. They contain `hev1.mp4`,
`hvc1.mp4`, `tag-only.mp4`, `flags-only.mp4`, `probe.swift`, a compiled
`probe`, `variants.cjs`, and `evidence.json`. Retain them until review and
browser verification finish; the directory is not a durable test fixture.
No library file was modified.

The first clip was made with video stream copy, without audio or subtitles.
The second was copy-remuxed with `-tag:v hvc1`. Comparing the two files
showed five differing bytes: two characters in the four-character label
and three HEVC configuration-array completeness flag bytes. Two additional
variants separated those changes.

| Variant | Sample-entry label | Other configuration | AVFoundation result |
|---|---|---|---|
| `hev1.mp4` | `hev1` | Original diagnostic clip | 0 frames; cannot start |
| `hvc1.mp4` | `hvc1` | Copy-remux output | 48 decoded frames; no error |
| `tag-only.mp4` | `hvc1` | Every other byte identical to `hev1.mp4` | 48 decoded frames; no error |
| `flags-only.mp4` | `hev1` | Remux configuration flags, label restored | 0 frames; cannot start |

The failing variants returned:

```text
AVFoundationErrorDomain -11833: Cannot Decode
NSOSStatusErrorDomain -12906
The decoder required for this media cannot be found.
```

The working variants returned `DECODED 48 STATUS 1 ERROR nil`. The probe
deliberately stops after 48 frames; status 1 means it was still reading, not
that the full clip or movie completed.

Compressed video packet SHA-256, equal for the two remux variants:

```text
7b919b98b553a73112fbfeef1346a5a42ed05390a873dde9bdb83150f579dfae
```

**How to read this:** the picture codec and compressed packets can decode on
this Mac. Changing the sample-entry label changes native decoder admission.
This is not evidence that the GPU needs a lower resolution or a different
video codec. The tag-only edit is a diagnostic control, not an approved
production muxing technique.

### 3.3 The execution environment can invalidate the control

The comparison was repeated while preparing this document. In the restricted
execution sandbox, the two `hvc1` controls also failed, with AVFoundation
`-11821` and underlying `-12911`; a system-query permission warning appeared.
Running the same read-only probe on the same four files outside that sandbox
restored the 0 / 48 / 48 / 0 result in the table.

That paired result identifies an environment-dependent false negative. It
does not identify which individual sandbox facility the decoder requires.
Run the known-working control in the same environment as the failing sample.
If both fail, stop interpreting the run as evidence about source packaging.
The probe's process exit code is not its verdict: it catches errors and
continues, so inspect the per-file frame count and error.

### 3.4 What has not been proved

- A same-machine, same-Safari-version A/B of original and remuxed media was
  blocked by the locked desktop during the earlier investigation.
- The native test uses AVAssetReader pixel-buffer output, not Safari's media
  element, AVPlayer presentation, audio output, or an HDR display check.
- The copied excerpt is not the full original file. It removes network and
  app state from the decoder experiment but does not prove full-film timing,
  seeking, multiple tracks, or fragment-boundary continuity.
- The working `hvc1` controls do not establish Dolby Vision preservation,
  profile conversion, HDR correctness, or standards compliance of a bare
  tag replacement.
- Browser/OS build identifiers were not retained in the evidence manifest.
  Capture them in the browser acceptance receipt; do not generalize this
  result to a version matrix that was never tested.

## 4. Code path — where compatibility information is lost

| Boundary | Current source anchor | Finding |
|---|---|---|
| File probe | `parse_probe_json` in [scan/probe.rs](../../crates/plurx-core/src/scan/probe.rs) | Chooses the first non-attached video stream; retains raw JSON, codec and profile, but no dedicated video sample-entry field |
| Domain facts | `MediaFile`, `ProbeResult` in [domain.rs](../../crates/plurx-core/src/domain.rs) | `video_codec` / `video_profile` exist; neither expresses `hev1` versus `hvc1` |
| Stored evidence | `get_file_probe_json` and file mappings in [store/mod.rs](../../crates/plurx-core/src/store/mod.rs), [hiqlite_media.rs](../../crates/plurx-core/src/store/hiqlite_media.rs), [sqlite/mod.rs](../../crates/plurx-core/src/store/sqlite/mod.rs) | Raw probe JSON can recover existing source tags without reading every media file again |
| Browser probe | `HEVC_TIERS`, `buildPlayCaps`, `capsDocument` in [web/index.html](../../crates/plurxd/src/web/index.html) | HEVC tier strings are `hvc1.*`; codec-family reporting loses that packaging distinction |
| Capability translation | `DeviceCaps`, `LegacyCaps`, `DeviceProfile::from_caps_v2` in [playback/caps.rs](../../crates/plurx-core/src/playback/caps.rs) | No progressive HEVC sample-entry constraint is carried into the profile |
| Delivery choice | `evaluate`, `decide`, `decide_forced` in [playback/mod.rs](../../crates/plurx-core/src/playback/mod.rs) | Container compatibility plus codec/HDR/audio checks can admit the raw MP4 |
| Existing DV exception | `dv_transport` → `remux_dolby_vision` in the same capability/decision modules | Already supports a DV-specific normalization request; web currently emits `progressive` when it claims DV |
| Byte delivery | `delivery_plan`, `direct`, `stream_mp4` in [http/stream.rs](../../crates/plurxd/src/http/stream.rs) | Direct playback sends source bytes unchanged; fixing only a remux builder cannot affect that attempt |
| Corrective copy path | `CopyVideoOptions::from_probe`, `copy_video_args`, `hevc_copy_tag_for_source` in [transcode/mod.rs](../../crates/plurx-core/src/transcode/mod.rs) | Already owns tag choice, parameter-set handling, and DV-preserve/strip/convert behavior |
| Progressive copy path | `progressive_hevc_copy_args`, `RemuxSpec` in [http/stream.rs](../../crates/plurxd/src/http/stream.rs) | A separate builder deliberately uses `hev1`/`dvhe` for promoted parameter sets; Remux alone is not a compatible-output guarantee |
| Native-HLS route | `initialRoute`, `indexPendingFallback` in [playback-policy.js](../../crates/plurxd/src/web/playback-policy.js) | Native-HLS remux chooses copy HLS and must not fall back to incompatible progressive output |
| Apple capability producer | `DeviceCaps`, `Caps.capsDocument` in [Caps.swift](../../clients/apple/Sources/Caps.swift) | Needs the new packaging constraint too; `dvTransport: hls` is only the DV exception |

The probe parser does inspect `codec_tag_string` when detecting Dolby Vision;
the claim is not that it never reads tags. The missing fact is a source
sample-entry value available to the progressive-playback compatibility test.

```text
source: HEVC + MP4 + hev1        browser probe: hvc1 supported
              │                               │
              └──── sample-entry distinction lost ────┐
                                                     ▼
                                     HEVC + MP4 judged compatible
                                                     │
                                                     ▼
                                         direct source bytes
                                                     │
                                                     ▼
                                    native decoder rejects this hev1
```

## 5. Reviewed direction — packaging can require remux, not video encoding

The names below are **planned interfaces**, not fields already implemented.
The linked implementation contract owns the exact semantics and acceptance
tests after review. Keep the decision pure; do not launch ffprobe from it.

### 5.1 Retain the source sample-entry fact

Proposed domain addition to both `ProbeResult` and `MediaFile`:

```rust
pub video_codec_tag: Option<String>,
```

Extract `streams[*].codec_tag_string` from the **same first non-attached video
stream** that supplies `video_codec`. A cover-art stream or second camera
track must not supply a different tag. Normalize valid four-character tags
to lowercase; missing, malformed, and zero-placeholder values are unknown,
not `hvc1`. Keep codec-family normalization separate from packaging.

**Selected persistence:** an additive nullable file column, populated on
scan and backfilled from valid stored `probe_json`, following
`backfill_dolby_vision_facts`: cluster job lease, per-node cursor, 256 rows
per tick, and a separate done stamp.
Cover both SQLite and Hiqlite reads/writes, publication/import paths, and
all `MediaFile` constructors. Existing rows without usable probe JSON remain
unknown. Backfill must not re-probe every movie or run an unbounded startup
transaction. Reuse the established migration mechanism, but still verify this
column's read/write/import coverage and stale-backfill fencing. Precedent
does not prove a new query is correct. Keep this admission fact out of
`CopyVideoOptions`, argv fingerprints, and fragment-index identity.

The SQL-JSON projection alternative is rejected: it duplicates first-video
selection across stores instead of delivering one source fact to every
decision. Per-request JSON cost was not the deciding argument; some paths
already fetch probe JSON for other purposes.

### 5.2 Report progressive packaging support explicitly

Proposed additive v2 capability field and matching profile field:

```rust
pub progressive_hevc_sample_entries: Option<Vec<String>>,
```

Example client claim:

```json
{"progressive_hevc_sample_entries": ["hvc1"]}
```

This constrains **original progressive HEVC-in-ISO-BMFF delivery**, not
whether the client can decode HEVC at all, and not which output tag every
DV remux must use. Codec profile, resolution, display transfer, DV profile,
audio, and transport checks remain independent.

Accepted semantics, detailed in the implementation contract:

| Input | Meaning / action |
|---|---|
| Field absent | Legacy behavior retained; do not pretend old clients reported new evidence |
| Present, `hvc1` only | Raw `hvc1` may pass this one gate; `hev1` or unknown packaging requires normalization |
| Present, `hvc1` and `hev1` | Both may pass this gate, only if the client actually established support |
| Present, empty | Client admits no raw progressive HEVC packaging; use a supported normalized transport or report no compatible route |
| Invalid entry / oversized list | Reject the new malformed claim, rather than silently widen support |

Bound the field to the four relevant labels `hvc1`, `hev1`, `dvh1`, `dvhe`,
at most four unique entries. Acceptance of a DV label never grants support
for all DV profiles. Unknown future labels need an explicit contract change.

The web client must probe both `hvc1.*` and `hev1.*` for progressive playback,
preserving profile/tier coverage. Its current tier helpers combine MSE and
progressive results: their positive answer alone is insufficient for this
new field. DV labels likewise need label-specific progressive evidence.
Do not infer either answer from a user-agent string. Retain working Chrome
direct playback when the required progressive probes actually pass; the
review's predicted Chrome/Safari answers are not measured browser results.

Apple emits `["hvc1"]` when claiming HEVC, in the same PR; its supported DV
profiles remain independent. Android initially omits the new field. Carry
the same document through decision, create, and replacement. Add **no**
legacy query field. When a new constraint is present, a POST error must not
retry GET with that constraint removed. Old clients retain legacy behavior;
rollout of the enforcing servers precedes updated clients.

### 5.3 Apply the packaging gate in the shared decision

For HEVC/H.265 sources in the ISO-BMFF family (`mp4`, `m4v`, `mov`), evaluate
the explicit progressive constraint alongside existing container admission:

```text
video / HDR / quality requires conversion? ── yes ─▶ existing transcode policy
              │ no
raw container, sample entry and tracks admitted? ─ yes ─▶ direct remains eligible
              │ no
supported normalized delivery available? ──── yes ─▶ copy-video remux
              │ no
              └───────────────────────────────────▶ existing unsupported/recovery policy
                                                    (never return the known-bad direct URL)
```

This describes Auto. Original retains its existing no-video-reencode
semantics but must still obey the packaging gate: **Original is not a
promise to send the original container bytes.** Forced Transcode retains
its current behavior. Unrelated quality and subtitle choices may legitimately
require encoding; do not override them to force a copy.

The new reason should explain the actual verdict, for example:
`HEVC sample entry hev1 not admitted for progressive playback; normalizing
through copy-video delivery`. Unknown metadata needs a distinct reason:
`HEVC sample entry unknown; normalizing for the reported packaging constraint`.
Do not say the codec is unsupported. Do not claim copy in a final verdict
that another gate changed to Transcode. `decide_forced(Original)` currently
builds its own reasons, so testing only `evaluate` is insufficient.

This gate must govern the answer returned by `/decision`, the session's
server-side re-derivation, and relevant replacement/resume paths. It is not
a URL rewrite in the browser or an exception keyed to file ID 9.

### 5.4 Reuse copy normalization and preserve dynamic-range ownership

The existing `copy_video_args` starts with `-c:v copy` and chooses the HEVC
output tag through `hevc_copy_tag_for_source`. Preserve that ownership.
Compatible DV bases normally use `hvc1`; preserved non-compatible DV may
need `dvh1`. Do not replace this logic with unconditional `-tag:v hvc1`.

The progressive builder is different: `progressive_hevc_copy_args` emits
`hev1` or `dvhe` when parameter-set promotion is required, because it cannot
rewrite its init after muxing. This is why Safari/Apple must take the
segmented normalization path for those sources, not merely any Remux URL.
Keep that builder correct for clients that can use its output; do not
globally replace its labels with `hvc1`.

Reuse `CopyVideoOptions::from_probe` and the existing parameter-set and
DV metadata paths. The index and serving pipes must use the same options
and identity. Merely editing four bytes in a served MP4 does not establish
valid parameter-set placement, configuration records, or fragment behavior.

For Safari acceptance, qualify the real native-HLS/fMP4 copy path, not just
an arbitrary progressive FFmpeg output. A Remux verdict alone does not prove
which transport the client will use: trace `delivery_plan` and the web
transport adapter. Audit the case with missing bitrate too; it must not
accidentally fall back to the rejected original URL. Do not invent HLS
support when the client reports none.

Do not redesign transport admission in `DeviceProfile`. The reviewed fleet
routes Safari and Apple remuxes to HLS. Add a narrow check at the
HTTP/execution-plan boundary using the original caps: a progressive-only
client whose available remux still violates its label constraint receives
a typed 409, not `/direct` or an incompatible remux URL. The exact rule and
negative tests are in the implementation contract.

The existing DV-preserve/strip/convert decision remains authoritative. Assert
the delivered dynamic-range and profile fields, the init segment's codec
signaling, and the master playlist together. The fix must not advertise DV
while stripping its metadata, or label an HDR10 fallback as DV preservation.

## 6. Alternatives — why not apply the shortest-looking patch?

1. **Set Safari's existing `dv_transport` to `hls`.** Rejected as the repair:
   it addresses the observed compatible DV case but misses the common SDR
   and HDR10 population. Do not build it as a detour to the packaging gate.
2. **Force all Safari HEVC through remux.** It avoids unknown source tags but
   discards working direct paths, adds producer/index load, and substitutes
   browser identity for capability. Not the preferred durable contract.
3. **Use stored probe JSON only in one HTTP handler.** Small patch, but risks
   decision/session divergence. A shared source-fact boundary is required.
4. **Disable HEVC or force H.264.** Re-encodes a video the native control can
   decode and may lose HDR. It treats the wrong failure.
5. **Retag the library file or patch bytes while serving ranges.** Mutates
   user media or creates new mux/range correctness obligations. The controlled
   byte edit was an experiment, not a library repair plan.
6. **Increase retry or startup timeouts.** More time does not change decoder
   admission. Recovery changes must be separately justified and reviewed.

## 7. Implementation packages — each ends with evidence

No package is implemented by this documentation change. Proposed test names
below are labels for new tests, not claims that tests already exist.

### 7.1 Source facts survive probe, persistence, and existing libraries

Add the approved source-fact representation and bounded recovery/backfill.
Test first-video selection, attached pictures, multiple video tracks, null
and malformed JSON, zero placeholders, and existing rows on both stores.
Check import/publication round trips and mixed-version migration behavior.

**Acceptance:** an already-scanned reference film K-shaped row yields `hev1` without
opening or rewriting the source; an unknown row stays explicitly unknown.
No decision path performs a synchronous media probe.

### 7.2 Capabilities reach the common decision without being widened

Add the approved bounded capability representation, serialization, browser
and Apple producers, with no new legacy query field. Use one compatibility helper
from Auto and Original. Test the final response and actual session recipe,
not just a private helper. Check cached capability invalidation on web update.

**Acceptance:** web and Apple hvc1-only claims plus supported SDR and
Reference film K-shaped hev1 sources select copy-video Remux in Auto and Original,
including the packaging reason. Progressive-only probes and the POST-error
guard have negative regressions. Forced Transcode
is unchanged; removing the new claim reproduces documented legacy behavior.

### 7.3 Prove the delivery bytes, not only the method name

Generate a small rights-safe HEVC fixture where feasible. Test actual copy
arguments and produced init/media bytes. Reuse existing DV fixtures/tests
for Profile 5, compatible Profile 8, and conversion cases; do not commit the
commercial movie clip. Include a parameter-set-in-band case exercising both
builders: the incompatible progressive output must not be selected for an
hvc1-only client. A test must not pass by merely replacing a string.

**Acceptance:** the planned copy path runs without a video encoder, serves
decodable normalized media, and keeps the index/producer identity aligned.
Audio selection, duration/timestamps, and DV/HDR signaling remain consistent.
Byte-for-byte packet equality is a diagnostic control here, not a universal
production requirement: legitimate bitstream normalization may change NAL
units without re-encoding pictures.

### 7.4 Qualify Safari and record the exact candidate

On an unlocked Mac, record macOS/Safari versions, source facts, candidate
commit, capability payload, decision reasons, chosen transport, and served
codec/DV signaling. Keep URLs and authentication tokens out of shared logs.

Run original and compatible-copy controls under the same conditions, then
play the full title through the candidate app. Verify presented frames,
moving picture, audio, and timeline advance—not only `loadedmetadata`,
`canplay`, or a server TTFF event. Confirm the expected HDR/DV mode on an
appropriate display separately from decode success.

**Proposed acceptance bounds, not measured results:** first visible frame
within 15 seconds on the known test network; two minutes of sustained
playback without terminal restart; ten sequential alternating forward and
backward seeks, each resumed within 10 seconds from the seek request,
including buffering time, as specified in the implementation contract.
Also issue a burst of seeks and verify the final requested position wins.
Record storage/index warmness; report a cold-index delay separately rather
than silently changing the bounds. The review found no hvc1 MP4 in the
library: use a rights-safe synthetic hvc1 control, not an invented existing
title. Include non-HEVC, Apple SDR/HDR10, and Chrome direct-play controls.

**Acceptance:** the original failure is reproduced or its disappearance
explained; the candidate's normalized path presents frames and passes the
finite playback/seek checks; no video encoder was used just for packaging.
If this browser check remains blocked, label the result native-decoder and
unit-test verified, not Safari-accepted.

## 8. Regression matrix — what must fail before the repair

All entries assume other codec, display, audio, quality and transport checks
are satisfied unless the row says otherwise.

| Case | Required result |
|---|---|
| HEVC `hev1` MP4 + explicit hvc1-only progressive claim | Remux; copy-video recipe; packaging reason |
| SDR `hev1` MP4 + web or Apple claim | Same result without any DV dependency |
| `hev1` MSE supported, progressive unsupported | Never advertise progressive `hev1` from the MSE result |
| New constrained POST returns 400 / 404 / 405 | No legacy GET that drops the constraint |
| Promoted source, hvc1-only and progressive-only | Typed 409; neither direct nor incompatible progressive remux |
| Same case under Original | Remux, no video encoding, reason retained |
| Same case under forced Transcode | Existing forced-transcode behavior |
| HEVC `hvc1` MP4 + hvc1 claim | Still eligible for DirectPlay |
| HEVC `hev1` MP4 + explicitly proved hev1 claim | Still eligible for DirectPlay |
| HEVC MP4 + unknown tag + restrictive claim | Conservative normalization; reason names unknown evidence |
| Field absent on old client | Explicitly tested legacy behavior; no invented claim |
| Restrictive claim, no supported normalized transport | Typed 409; no return to known-bad direct or incompatible remux bytes |
| H.264 MP4, audio-only media | No new HEVC packaging restriction |
| HEVC MKV | Existing container-remux policy; not a new ISO-BMFF exception |
| Attached picture before HEVC video | Tag comes from actual playback video |
| Compatible DV Profile 8 with preservation negotiated | Correct compatible tag and DV metadata/playlist agreement |
| Preserved non-compatible DV Profile 5 | Existing `dvh1` policy retained; no blanket hvc1 retag |
| Unsupported DV/HDR, limited resolution, explicit subtitle burn | Existing higher-priority policy retained; truthful final reason |
| Decision, session-create, and replacement for same facts | Same packaging verdict and compatible recipe |
| Existing database row, import, publication, both store backends | Same fact, including unknown; no rescan prerequisite |

At least one fixture must demonstrate the old defect: current code admits
the raw reference film K-shaped source, while repaired code requires normalization.
Run the actual HTTP/session seam as well as the pure decision test. If the
pure fixture already remuxes because of audio or DV policy, it does not
exercise this bug; construct compatible ancillary facts deliberately.

## 9. Verification and release — no CI-as-compiler or hidden deployment

Follow [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md), including the
pinned compiler loop before Rust edits and the current draft/review/fast-lane
rules. This document does not authorize merging or deploying an implementation.

Suggested local checks after implementation, in addition to the new focused
tests and any store/migration-specific commands selected by changed paths:

```bash
rustup run 1.97.1 rustc --version                # verify the actual compiler
cargo check --workspace --all-targets --locked  # compile the complete touched model
cargo fmt --all --check                        # formatting
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p plurx-core playback::             # decision and capability behavior
cargo test -p plurx-core scan::probe::          # source-fact extraction
cargo test -p plurx-core transcode::            # copy/DV normalization contracts
python3 scripts/js-check                       # shipped inline JavaScript syntax
node tests/playback/web-control.test.js        # existing web behavior controls
python3 -m unittest discover -s tests/validation -p test_playback_routing_inventory.py
python3 -m unittest discover -s tests/operations -p test_docs_index.py
```

Use the repository-pinned toolchain for all Cargo commands; the version check
does not by itself change an incorrectly configured shell. Record exact
focused test names and nonzero executed-test counts. Distinguish pre-existing
failures and missing FFmpeg features from regressions rather than claiming
an entire suite passed after exclusions. Re-run against the actual PR base
if it moves.

Register the new route in
[routing-decisions.toml](../../tests/playback/routing-decisions.toml), update
[PLAYBACK.md](../PLAYBACK.md), document the wire addition in
[API.md](../API.md), and add the corrective test/evidence mapping required by
the history audit. Keep production deployment separate from PR creation.

For an approved rollout, monitor direct/remux/transcode counts, cold-start
latency, producer/index load, first-presented-frame evidence, and new unknown
tag reasons. Additive metadata should survive rollback. A code rollback may
restore the original failure; do not describe it as a media repair. Never
rewrite originals or clear user media/caches as an implicit rollback step.

## 10. Review outcome — Sol builds from the implementation contract

Fable's review is “Approve with changes,” not approval of runtime code.
The changes to this plan are summarized in §1.2. The review executed a
decision fixture at `3129ce993`: reference film K-shaped DV P8 and SDR HEVC both
returned DirectPlay; `dv_transport=hls` changed only the DV case to Remux;
Original omitted a packaging reason; the preserved compatible-P8 copy tag
was hvc1. The review did not rerun the native decoder experiment or test
physical Apple devices.

[HEVC-SAMPLE-ENTRY-IMPLEMENTATION.md](HEVC-SAMPLE-ENTRY-IMPLEMENTATION.md)
turns those findings into ordered work packages, exact contracts, seam tests,
and release evidence. It supersedes the earlier proposal's open choices.
Runtime changes still need their own adversarial review and validation.

## 11. Reproduction appendix — use the known-working control

These commands read existing diagnostic copies, not the library file:

```bash
avatar_diag=/private/tmp/plurx-avatar-safari.Yxy2yT

# Inspect the actual first video stream's codec and packaging.
ffprobe -v error -select_streams v:0 \
  -show_entries stream=codec_name,codec_tag_string,profile,width,height,extradata_size \
  -of json "$avatar_diag/hev1.mp4"

# Compare compressed packet payloads, not whole MP4-file hashes.
ffmpeg -v error -i "$avatar_diag/hev1.mp4" -map 0:v:0 -c copy \
  -f hash -hash sha256 -
ffmpeg -v error -i "$avatar_diag/hvc1.mp4" -map 0:v:0 -c copy \
  -f hash -hash sha256 -

# Decode real pixel buffers; use an environment where the hvc1 control works.
"$avatar_diag/probe" "$avatar_diag/hev1.mp4" "$avatar_diag/hvc1.mp4" \
  "$avatar_diag/tag-only.mp4" "$avatar_diag/flags-only.mp4"
```

The retained Swift probe opens each file with `AVURLAsset`, loads video
tracks, creates `AVAssetReader`, and adds `AVAssetReaderTrackOutput` with
`kCVPixelBufferPixelFormatTypeKey` set to
`kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange`. It counts only samples with
a non-null `CMSampleBufferGetImageBuffer`, stops at 48, prints the reader's
status/error, and cancels reading. Thus its success criterion is decoded
images, not compressed samples being demuxed.

If the temporary copies are gone, reproduce with a newly authorized local
copy of the source and a fresh temporary directory. Extract a short clip
using `-c:v copy`, create a second copy with `-tag:v hvc1`, compare packet
hashes and MP4 box differences, then construct the two diagnostic controls
on copies only. Validate box boundaries and equal file layouts before a
tag-only byte edit; the retained `variants.cjs` was a one-fixture diagnostic,
not a general MP4 editor. Do not publish the movie clip, source URL, library
path, or authentication material with the review.
