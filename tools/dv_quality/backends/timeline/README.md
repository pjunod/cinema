# Synthetic timeline controls

This source-only slice extends the reviewed decoded-layer probe with authored VFR intervals, a real demuxer seek with preroll, and two independently decodable segments whose timestamps restart. It establishes 21 known picture/RPU associations and refuses five deliberate negatives. It does not qualify production playback, film fidelity, Dolby profile/container conformance, target mapping, trims, queues, fallback or hardware performance.

The prior approved decoded-layer bundle establishes initial-read timing and explicitly excludes seek. Its bytes and replay5 remain unchanged. This slice adds separate evidence rather than extending that earlier receipt implicitly.

## Replay and dependencies

```sh
sh replay.sh /absolute/new-scratch /absolute/approved-decoded-replay /absolute/approved-decoded-bundle
```

The supplied prerequisite is the approved encoded dual-layer replay5. The dependency lock verifies its complete FFmpeg static prefix, reviewed receipt/inventory/ledger identities, original pinned source archive, and generated RPU fixture files before copying. It verifies the copied prefix and fixtures again. The local Linux ARM64 image digest comes from validated `image-id.txt`; no machine-specific image ID is hardcoded. Different dependency builds require a new reviewed lock.

New C helpers compile with `-Wall -Wextra -Werror -std=c11` against the exact approved FFmpeg 9.0.1 static prefix. FFmpeg revision is `bf1b838f2ab88b4f8fd83443325c782ea0e0f7fa`, source archive SHA256 `fb1931fd4eb29297ee1c1017a24f800c4d8fbea35b4f2aaeb28308a48a9149b4`. The prior source-built prefix is reused after identity validation; this slice does not claim another fresh upstream FFmpeg rebuild. No upstream source patches are applied. Existing isolated FFmpeg 5.1.9/x265 3.5 encode the fixtures as separate processes. The selected decoder build has the previously recorded LGPL configuration; x265 licensing remains separate.

All writes, including test temporary directories, go into a new scratch directory. Python bytecode writes are disabled. Containers have no network and use 2 CPUs / 2 GiB caps. No host/fleet packages, credentials, repository files or production runtimes are changed. The prerequisite image's original live apt/pip dependency resolution limitation remains.

## Authored VFR intervals

The same six independent native 64 × 64 YUV420P10 picture identities and synthetic P7 FEL RPUs are reused. Native BL/EL hashes and decoder-attached raw RPU hashes must match the known source. Fresh complete RPUs have distinct bounded L1 tags; these are identity labels, not content analysis. Actual encoded profile is checked as Main10, rather than trusting requested encoder options.

The source generator declares irregular presentation timestamps before encoding. FFmpeg's actual encoded PTS are checked against that declaration, and compound-wrapper PTS retain those actual values. Frame ordinals select known picture/RPU references; they never replace decoder timestamps.

| Display PTS (ms) | Authored interval (ms) | Provenance |
| --- | --- | --- |
| 0 | 40 | Next actual display PTS minus current PTS |
| 40 | 70 | Next actual display PTS minus current PTS |
| 110 | 30 | Next actual display PTS minus current PTS |
| 140 | 90 | Next actual display PTS minus current PTS |
| 230 | 70 | Next actual display PTS minus current PTS |
| 300 | 41 | Explicit synthetic terminal policy |

Encoded BL/EL inputs retain nominal 24 fps DefaultDuration values and 41 ms API durations. The compound fixture deliberately authors the presentation intervals above; it does not claim preservation of those original nominal durations. Its exact 40,000,000 ns DefaultDuration supplies the first interval, and explicit BlockDuration values store every differing interval, including terminal 41 ms. A bounded independent EBML reader verifies the stored fields and timestamps. Stored intervals, raw demux durations, split durations and decoded frame durations are checked separately. The nominal 25 fps associated with the wrapper default is authoring metadata, not an inferred VFR rate.

No-reorder packet PTS arrive in display order. Reordered PTS arrive 0, 110, 40, 140, 300 and 230 ms. Both layers decode in display order and pair by exact rational PTS within an explicit epoch. Unknown DTS remain null; seek can change demuxer-derived DTS state. No claim of original DTS preservation across seek is made.

## Actual seek and preroll

The helper sends two real reordered packets, calls `avformat_seek_file` with target 200 ms, then flushes both BSFs and both HEVC decoders. The trace records the API result, actual flush calls, reader boundaries, keyframe flag and context identity. The actual demuxer lands at keyframe 140 ms and reads packets at 140, 300 and 230 ms. Freshly flushed decoders emit 140, 230 and 300 ms with stored intervals 90, 70 and 41 ms.

The fixture selector validates 140 ms as preroll and selects PTS at or after 200 ms for display. This selector is not a product seek contract: the 140 ms picture's 90 ms interval contains the requested target, and presenting that containing picture is not tested here. A filtered frame list alone cannot pass the seek evidence checks.

The deliberate no-reset control still decodes successfully, but old buffered 0 and 110 ms pictures leak into the new epoch. Their native pixels and raw RPUs identify the original pictures independently of the epoch label. The acceptance checker refuses the missing explicit reset.

## Timestamp restart and cross-epoch pairing

Two separately encoded three-picture containers have PTS 0, 40 and 110 ms, intervals 40, 70 and terminal 41 ms, and different native-picture/RPU identities. This is a driver-declared input boundary, not automatic HLS/discontinuity detection or a single-container timestamp-wrap test.

The positive path drains and destroys the first pair of contexts, opens the second container, and creates fresh contexts for epoch 1. Pairing uses `(epoch, rational PTS)`; equal timestamps from different epochs are distinct pictures. The no-reset path instead leaks delayed old 40 and 110 ms pictures into epoch 1. A separate validly decodable negative substitutes epoch 0 EL pictures into epoch 1; matching PTS alone cannot hide the wrong native identity. Swapped RPU payloads are another successful-decode association refusal.

## Duration omission control

The retained implicit-duration wrapper reproduces the inherited harness omission: copying codec parameters/timebase without stream rates yields neither DefaultDuration nor BlockDuration. Its original buffered API packets report 41 ms, while post-seek packets and pictures report 33 ms. Recorded public stream rates are 119/4 and 293/12, with codec parameter rate 24/1. The absence of stored duration fields and FFmpeg's rate-dependent fallback explain why API duration is not authoritative source-duration evidence; the exact private fallback branch was not instrumented.

This is a wrapper-authoring defect, not a demonstrated FFmpeg bug. The checker refuses the unspecified presentation intervals even though decoding succeeds. The corrected fixture stores exact intervals and verifies that initial and post-seek API values agree with those fields.

## RPU reuse remains unqualified

All supplied fixtures have `use_prev_vdr_rpu_flag=false`. Fresh-RPU-only checks are deliberately limited to this cohort and are not general DV validity rules. Pinned FFmpeg source shows an ID-indexed mapping cache, profile/compression checks for reuse, and unknown-previous-ID failure. HEVC flush calls `ff_dovi_ctx_flush`, which clears cached mappings, DM data and extension blocks. These are source observations, not an executed legitimate-reuse fixture or Dolby conformance proof. Reuse across seek/reset requires a separately generated, legal profile/compression cohort and explicit cache lifetime evidence.

## Evidence and remaining work

A bounded ordered grammar requires the actual first read, seek/reset boundary before any new-epoch packets or frames, required drains before fresh-context discontinuities, and no duplicated lifecycle events. Every trace field has its declared type; numeric overflow/nonfinite values and booleans in integer fields are refused. Focused tests mutate actual seek evidence, flush/reset declarations, epoch labels, keyframe reports, decoded/split durations, stored BlockDuration, native EL bytes, raw RPU bytes, failure status/diagnostics and finite JSON. The shell driver records every real decode exit status and stops on any failure. Four isolated controls complete the actual decoder artifact writes and then exit 37 or crash with SIGSEGV; recorded status 37/139, missing success marker and checker refusal are required. No synthesized status 0 is used. The negative suite requires its exact expected reason; crashes or unrelated corruption do not count as successful negatives. Every accepted pair binds native BL/EL and raw RPU hashes, with container/encoder/trace evidence in the result. Negative observations retain the actual leaked/substituted picture identities.

This slice does not rerun the prior GPU arithmetic oracle. The inherited arithmetic domain and limits remain the earlier synthetic identity/NLQ subset, 2e-5 PQ / 2 RGB48 codes, not a new VFR quality qualification. Remaining cases include legitimate reuse, seeking beyond this short GOP cohort, presentation-at-target semantics, live protocol discontinuities, timestamp wrap, format changes, dropped/late layers, cancellation/backpressure and production fallback.

Primary references: [Matroska element semantics](https://www.matroska.org/technical/elements.html), [pinned Matroska duration writing](https://github.com/FFmpeg/FFmpeg/blob/bf1b838f2ab88b4f8fd83443325c782ea0e0f7fa/libavformat/matroskaenc.c#L2896), [pinned duration fallback](https://github.com/FFmpeg/FFmpeg/blob/bf1b838f2ab88b4f8fd83443325c782ea0e0f7fa/libavformat/demux.c#L696), [pinned RPU cache resolution](https://github.com/FFmpeg/FFmpeg/blob/bf1b838f2ab88b4f8fd83443325c782ea0e0f7fa/libavcodec/dovi_rpudec.c#L520), [pinned HEVC reset](https://github.com/FFmpeg/FFmpeg/blob/bf1b838f2ab88b4f8fd83443325c782ea0e0f7fa/libavcodec/hevc/hevcdec.c#L4187).
