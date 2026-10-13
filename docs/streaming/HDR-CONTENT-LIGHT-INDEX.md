# HDR copy indexing — changing light levels do not change the decoder

**Status:** implemented; focused regressions passed, physical qualification pending · **Written:** 2026-10-12

Companion to [the playback reference](../PLAYBACK.md): this records the
indexed-copy rejection behind a long Android startup.

## A metadata comparison selected the slow presentation

The incident remux had ready indexes, but both recorded
`parameter_sets_constant=false`. Seven bounded source samples kept identical
VPS/SPS/PPS and mastering-display metadata while changing content-light SEI
values. Observed MaxCLL/MaxFALL pairs included 230/197, 283/200 and 281/139.
These samples establish the changing light-level metadata; they are not a
whole-film proof of every codec parameter.

The indexer compared complete `PromotionInputs`, including raw HDR10 SEI,
and treated any difference as a decoder-configuration change. That excluded
the indexed copy path. Rolling recovery then held its first playlist for
48 seconds of media; the app measured 23.7 seconds to its first frame.

## Scan the metadata and retain the original samples

The index now scans every video sample for HDR10 metadata, including pictures
inside a GOP and fragments unsuitable as independent starts. Content-light
values accumulate as component-wise whole-title maxima. Zero remains
unspecified: if a component is explicitly unknown anywhere, the title-wide
component remains unknown. Mastering-display payloads must agree, and malformed
metadata or incompatible decoder parameter sets still reject indexed copy.

The stored promotion contains canonical metadata. Both hvcC content-light
messages and existing sample-entry clli boxes are normalized from that stored
value before the muxer identity check and before ordinary promotion fills missing fields. Seeking to a different
scene therefore cannot produce a different initialization segment solely
because its light-level metadata differs. Other initialization messages and
boxes are preserved. Video samples, their per-sequence SEIs and Dolby Vision
RPUs remain untouched by this change.

Local and shared HEVC index identities include the metadata-scan revision.
Old rejection records cannot silently become new positive evidence. A source
needs a new complete index; until it has one, the existing recovery behavior
remains available. There is no startup-buffer reduction or new setting.

## Verification and remaining acceptance

Focused regressions cover whole-title maxima, unspecified values, conflicting
mastering metadata, malformed content-light payloads, identical promoted
initialization data from different landing scenes, and a changed maximum on
a non-keyframe passing through the real fragment indexer.

Physical acceptance requires a rebuilt complete index, indexed playback on
the Lenovo, a seek across metadata changes and retained first-frame evidence.
This document does not claim a production deployment or full-title device
qualification.
