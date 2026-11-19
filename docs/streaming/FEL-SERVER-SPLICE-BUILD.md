# Server-side Dolby Vision FEL splice

**Status:** open. The lab replay passed in native Safari. The server
packager is in local qualification. Live publication, promotion and
deployed Safari acceptance are pending. · **Updated:** 2026-11-19 ·
**Owner:** Codex.

Keep the ordinary Dolby Vision Profile 8.1 conversion playing while
the server prepares a video interval reconstructed from the Profile 7
full enhancement layer. Publish the reconstructed video on the same
timeline and keep the existing AAC producer running through the cut.
The existing FEL measurement and qualification jobs own decisions
about whether the title and renderer can produce the enhanced interval.

## Evidence

- The first real-media splice skipped 22 source pictures. The source
  packet actually selected by the copy producer occurred 918 ms earlier
  than the separate origin probe reported. Match the actual source
  packet to the muxed video clock.
- Once the pictures matched, resetting both HLS renditions still
  produced a measured 44–46 ms gap in Safari's recorded AAC output.
  One continuous AAC rendition with no discontinuity at the cut played
  cleanly in native Safari. The decoded AAC samples and their timeline
  stay with the original rendition.
- The video producers emit different HEVC parameter sets. A single
  generic fMP4 HLS `hev1` initialization plus each producer's original
  VPS, SPS and PPS decoded all 252 reference pictures in AVFoundation.
  All 13 advertised sync samples in the revised package also decoded
  independently in FFmpeg. The package uses generic HLS fMP4 branding.
- The audio projection matched all 2,120 AAC samples across seven saved
  rolling segments, including two longer startup sample durations.
  The video and audio duration fields come from their respective clocks.

## Publication contract

1. Freeze an eligible native session's split rendition layout before
   exposing its master. Prove the video and audio configuration and
   their first complete playable window. Continue ordinary playback
   when split admission declines.
2. Bind the ordinary output clock to the same demuxed packet actually
   fed into the copy mux. The renderer hashes every framed base-layer
   VCL NAL of an accepted source picture. The publisher compares the
   complete, ordered witness with the next ordinary video object and
   copies its presentation timestamps picture by picture.
3. Prepare a future FEL interval through the existing renderer and
   its exact source-picture grid. Select only a boundary beyond the
   last publicly advertised video segment. Check source adjacency,
   random access and the decoded presentation interval, including
   reordered samples. Put the first FEL decode time immediately after
   the last ordinary decode interval and compensate composition
   offsets so every picture presents at its original clock time.
4. Stage verified video bytes under immutable role-specific object
   names. Keep using the incumbent AAC segments. Commit the splice
   only while the producer and publication lease, selected tracks,
   target media sequence and source identity still match. The
   publication snapshot exposes whole, contiguous rendition windows.
   A missed boundary keeps playing ordinary video.
5. Publish FEL delivery evidence with the committed video boundary.
   Playback Info uses that evidence when the viewer reaches the
   boundary. Keep the preparation and frame-exposure guards on the
   existing player-replacement path.

## Implementation and acceptance

| Task | Implementation boundary | Acceptance |
|---|---|---|
| Packaging | Core fMP4 track projection, exact clocks and fixed HEVC initialization. | Packet-equivalence regressions, independent sync decoding and native output comparison. |
| Publisher | Copy packet witness and the rolling writer, its immutable segments, retention and splice snapshot. | Producer supersession, seek, late window and partial publish keep valid media available. |
| Delivery | Native capability admission, HLS resources, trial hookup and delivered FEL evidence. | Same player and audio rendition across the cut, correct metadata, fallback and ordinary cold start. |
| Promotion | Astra review of the integrated tree, local gates, effort gate and the required main promotion. | Owner rebuilds the server. Twilight in Safari upgrades to Profile 8.1 FEL and plays through the cut without picture or sound interruption. |

The initial route covers fresh, local Auto playback from the film head
in Safari 27.2 with the qualified in-band HEVC and AAC shape. Resumed
starts use the existing playback route. The independent IDR checked
on the next ordinary segment is the safe fallback if rendering
cannot supply the next extent.

The integration branch is `effort/fel-server-splice`. Use the project's
[development pipeline](../DEVELOPMENT_PIPELINE.md) and
[main landing procedure](../ci/MERGE-TRAIN.md#train-off--what-you-do-today). The owner directed use of the direct landing path for this effort.
