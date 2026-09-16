# VideoToolbox file captions — preserve playback and decide caption policy

**Status:** open follow-up · **Issue:** [#345](http://192.168.4.7:3000/noirr/plurx/issues/345) · **Written:** 2026-09-16 · **Origin:** F1 in
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

## Decision and implementation contract

1. Reproduce against the production VOD plan with a retained caption-bearing
   file, proving the actual decode/filter/encode route and output grade.
2. Decide whether to preserve captions through a corrected FFmpeg build,
   remove their forwarding with an explicit product policy, or choose a
   qualified alternative encoder. Do not blindly copy the live-only switch.
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
