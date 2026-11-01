# Subtitle cue index — why captions came and went, and the reader that fixes it

**Status:** built · **PR:** `claude/subtitle-cue-index` · **Written:** 2026-10-31

Companion to [SUBTITLE-CLUSTER-EXTRACTION-PLAN.md](SUBTITLE-CLUSTER-EXTRACTION-PLAN.md)
(how extraction is queued and shared) — this is *why every extraction was
slow, and the reader that removes the cost*.

## The symptom

Selected text subtitles showed for a while, vanished for a while, then came
back, on every client. The owner's report on 2026-10-31: "Sometimes they
still work, then don't for a while, then work again."

## What the fleet logs showed (2026-10-30, "Being 17", 51.6 GB 2160p remux)

| Path | What happened |
|---|---|
| Playback window (200 s) | 25–186 s per window on nynuc (21:40–22:50 UTC). At the slow end the window took almost as long as it covered, so cues existed for part of each window and every segment requested before it landed was answered empty. Players do not refetch an answered segment. |
| Whole track, playback node | "cluster job is still queued after the foreground claim wait" at 20 s, then a 120 s failure memo. |
| Whole track, cluster job | Claimed by nynuc 22:53, m6 23:25, nuc4 23:57, m6 00:39 and never published: each pass read 51.6 GB and hit the 30-minute source-pass deadline, then restarted from zero on another node. |
| Peer window (`/internal/media/subtitle-range`) | 504 after the 25 s work budget on m6 and nuc4. |

## Root cause — subtitles were found by demuxing the picture

Every reader ran FFmpeg over the source with `-map 0:s:N`. A subtitle-only
extraction still reads every video and audio byte of the span it covers,
because the subtitle packets are interleaved with them. Cost therefore
followed the picture's bitrate: ~8 MB per second of a 4K remux, about 2.5 GB
for one window, the whole file for the whole track. On a NAS shared with
playback, attestation and backfill, that is minutes per window and longer
than the source-pass deadline per track. Each symptom above is this one cost
surfacing in a different bound.

## The fix — read the blocks the container already indexes

Matroska indexes subtitle blocks. mkvmerge writes a cue point for every
subtitle frame and a `NUMBER_OF_FRAMES` statistic per track. In a 60-file
library sample every MKV (mkvmerge, DVDFab, WKSmerge) had both; "Being 17"
has 1005 cues for 1005 frames; the whole track is 2010 small block reads
(4.2 MB) and 31.6 s on a loaded NAS instead of a 51.6 GB scan that never
finished.

```
 source.mkv ──▶ EBML header · seek heads · Info · Tracks · Tags · Cues
                    │
   cues for track == NUMBER_OF_FRAMES, every ref is that track's
   unlaced block, track list matches the library scan?
                    │ no ────────────────▶ full demux, unchanged
                    │ yes
                    ▼
   two reads per cue (cluster timestamp, block), 8 in flight
                    ▼
   subtitle-only .mkv: original EBML header + Info + every subtitle
   TrackEntry + the blocks verbatim, one cluster per source cluster
                    ▼
   the unchanged FFmpeg command, input = the subset
```

[`crates/plurxd/src/mkv_sparse.rs`](../../crates/plurxd/src/mkv_sparse.rs)
does the reading and writing;
[`subtitles.rs`](../../crates/plurxd/src/subtitles.rs) `sparse_subtitle_input`
wires it in. Used by: the whole-track flight (before the cluster wait), the
playback window, and the cluster `subtitle_source` pass.

## Key decisions

1. **Prove completeness, don't assume it.** Distinct cue refs must equal the
   track's frame statistic, and each ref must resolve to a block of that
   track. Equal counts of distinct blocks of the track mean none is
   missing. Laced blocks void the proof (a block holds several frames).
   Missing statistics, missing `CueRelativePosition`, or a track list that
   disagrees with the scan all keep the full demux.
2. **Copy blocks, never interpret them.** The subset keeps codec private
   data, content encodings and block flags byte for byte, and FFmpeg does
   the conversion it always did. Every subtitle `TrackEntry` is kept in
   order so `0:s:N` names the same track.
3. **Reproduce the timeline FFmpeg would have produced.** A subtitle-only
   file has no audio or video start, so the original's start time is
   measured with the same FFprobe and carried as `-copyts -itsoffset
   <−start>` where the replaced command rebased (whole track, windows). The
   source pass already runs `-copyts` and gets the raw timeline.
4. **Random-access advice on the read.** Default readahead on an NFS mount
   turns each small read into megabytes; the reader opens its own fence and
   sets `POSIX_FADV_RANDOM` (`F_RDAHEAD` 0 on macOS).
5. **No switch.** The reader has no failure mode the demux lacks: anything
   unproven, unreadable or unconvertible falls back to the demux and logs
   why.

## Verification

- Unit: `mkv_sparse::tests` (byte-exact fixtures: proof present, statistic
  mismatch, missing statistic, missing relative position, misdirected cue,
  laced block, span filter, ordinals kept).
- Call sites: `subtitles::tests::a_cue_indexed_track_is_extracted_without_demuxing_the_source`
  and `a_cue_indexed_window_reads_only_its_span` pin the route and compare
  against the demux on a file whose start time is not zero (a 5 ms codec
  delay).
- Library parity, Debian FFmpeg 5.1.9 (the production build): see the PR
  description for the per-file table.

## Non-goals

- **Other containers.** MP4 and MPEG-TS keep the demux; the library is
  overwhelmingly MKV.
- **The cluster job's whole-file attestation.** The `subtitle_source` pass
  still hashes the source before publishing. Playback no longer waits on
  it.
- **Shared playback cache admission.** "budget or filesystem reserve
  exhausted" is logged on every node every minute while caches sit at
  15–23 GB of 50 GB; that is a separate defect with its own RCA to write.
