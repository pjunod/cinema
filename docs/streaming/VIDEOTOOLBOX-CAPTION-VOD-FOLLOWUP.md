# VideoToolbox file captions — preserve playback and decide caption policy

**Status:** active root-cause repair since 2026-10-08 · **Issue:** [#345](http://forge.lan:3000/noirr/plurx/issues/345) · **Written:** 2026-09-16 · **Origin:** F1 in
[Fable's review](../reviews/LIVE-TV-VIDEOTOOLBOX-ATSC1-FABLE-REVIEW.md).

Companion to the [live-TV repair](LIVE-TV-VIDEOTOOLBOX-ATSC1-ROOT-CAUSE-AND-FIX.md).
This work is separate because changing the shared encoder affects VOD
caption preservation, cached output identity and every file input.

## Reproduced failure and affected input

Fable independently reproduced the same exit 183 on caption-bearing MPEG-2
using the VOD shape (`-hwaccel videotoolbox` into `h264_videotoolbox`). A
single caption-bearing frame sufficed. `hevc_videotoolbox` passed the
reviewer's control. These are supplied review measurements, not a guarantee
that every Mac or FFmpeg build behaves identically.

The [DVR recorder](../../crates/plurxd/src/live_tv/dvr.rs) preserves tuner
MPEG-TS bytes, including A/53 captions. Its recordings therefore retain the
trigger even after the live-encode workaround lands. Other caption-bearing
MPEG-2 files can fail on the same H.264 VideoToolbox route. The live fix does
not repair this path.

## Active repair decision, 2026-10-08

The user authorized implementation as part of the
[video-processing follow-up](MACOS-VIDEO-PROCESSING-STATUS.md). Preserve
A/53 captions by repairing the pinned Jellyfin VideoToolbox SEI insertion
path. Disabling caption forwarding or adding an encoder retry is not the
repair. The existing Live policy remains scoped to Live.

A new bounded control reproduced the failure using caption-bearing HEVC/AC-4
broadcast input, software picture decoding, CPU scaling and required hardware
H.264 encoding: `a53cc=1` failed before output; otherwise identical
`a53cc=0` produced 24 frames. The actual Apple output SEI and invocation are
retained privately for parser diagnosis. The original MPEG-2 failure remains
part of the acceptance matrix.

The captured Apple output identifies the concrete parser error: its first
SEI has one type-5 message with a 45-byte unescaped payload and four inserted
emulation-prevention bytes. The old parser skips 45 encoded bytes, lands
inside that payload and interprets the remaining bytes as another message.
Its per-message size accumulator is also not reset, yielding a false
190-byte payload with only two encoded bytes left. Another valid SEI with a
21-byte payload and one escape triggers the corresponding type error.

SEI payload sizes count unescaped RBSP bytes; the parser was counting
escaped EBSP bytes. The repair must advance by logical payload bytes while
retaining encoded offsets for the append operation, reset message size,
validate trailing bits and check output capacity including the start code.
The committed repair is `622c1ee42`; patch SHA-256 is
`8e9e7c5222a4ac497c88d802bfdaf38ee29e1c7c7c6b6b1f8f251d397bc7a8b4`.
Its rebuilt Jellyfin baseline passes all seven direct hardware controls.
Public reordered HEVC/AC-4 and original MPEG-2/A53 sources each produce
48 decoded frames and 48 caption records under default H.264 VideoToolbox
forwarding. The oracle compares caption triplets in presentation order,
bound to actual normalized frame timestamps; raw packet order is insufficient
for the reordered source. Caption-off and caption-free controls contain no
captions, and copy/remux preserves all 48. The decoded planes are identical
with forwarding enabled and disabled for both sources.

HEVC output still contains no A53 records, as in the baseline; successful
HEVC encoding is not evidence of caption preservation. Comparing Apple
vendor SEI bytes across independent encoder sessions was inconclusive because
one vendor byte varies; no masking or speculative change was introduced.
Malformed size, escape, trailer, overflow and append-capacity regression
sources are retained. Unit execution remains with the merge coordinator.

The first normal API run uses the coherent shared package carrying both
repairs. It delivers 120 decoded video/audio frames, but the caption oracle
fails: almost all caption records contain padding, with only two 608 tuples
and three valid 708 tuples. Frame count and the presence of caption side data
therefore do not establish preservation.

The finite VOD producer preserves source timestamps with `-copyts`, then
applies film-relative `trim` and `fps` before its final timestamp adjustment.
This broadcast source starts at 66,271.889756 seconds. Capturing the actual
descriptor-based producer command confirms that its original clock reaches
`fps` with a zero start time. A bounded reproduction consumes only two source
frames (PTS 66,271.889756 and 66,271.906444) while scheduling 17 observations
from zero through 0.267 seconds, all with identical picture checksum
`9D1D0B95`. This confirms first-picture duplication and explains the missing
subsequent captions.

The repair belongs in the shared finite-VOD source-clock owner. Its source
origin must be frozen from current held-source/producer evidence and included
in the existing rendition identity. Per-input correction must preserve source
audio/video offsets and leave already-normalized bitmap sidecars in film time.
A global timestamp reset and cached catalog start times are not substitutes
for that authority. The first bounded software-decode control restores picture
progression with a per-input offset of -66,271.889756 seconds: the unchanged
chain has one unique output picture, while the corrected chain has 17.
This is mechanism evidence only. Exact seek behavior, zero/negative origins,
A/V offsets and normal-API picture/caption timing still need final-candidate
controls. Bounded positive/negative-origin controls preserve the existing A/V
skew, including AAC priming. The pinned FFmpeg demuxer defines an absent
format start as zero; retain that source-bound default as a distinct versioned
fact rather than rejecting valid raw inputs or confusing it with reported zero.
Malformed, nonfinite and unrepresentable timing remains an error.

The initial bounded shutdown also required forced termination. That failure
is retained separately; no owned producer remains. Direct caption preservation
is established; complete file-VOD acceptance remains open until the ordinary
API path, source timeline and lifecycle are proven on the final candidate.

## Decision and implementation contract

1. Reproduce against the production VOD plan with a retained caption-bearing
   file, proving the actual decode/filter/encode route and output grade.
2. Preserve captions through a corrected pinned Jellyfin FFmpeg build.
   Diagnose the actual SEI bytes and retain existing messages while adding
   captions. Keep bounds checks for malformed and truncated input; do not
   copy the live-only switch.
3. Inspect recipe/cache identity if encoding policy changes, so previously
   failed or differently captioned output is not treated as identical.
4. Add a regression for MPEG-2/A53 file input and a DVR-shaped recording,
   plus controls for copy/remux, caption-free files and HEVC output.
5. Document the per-host caption difference and prove playback in the
   affected Mac/client combination before closing the issue.

Acceptance requires first-frame and sustained playback without SEI-copy
errors, accurate caption-loss/preservation claims, and focused tests against
an exact candidate. No server-wide feature gate or new caption UI is part of
this follow-up unless separately requested.
