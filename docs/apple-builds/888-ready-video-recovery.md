# Recover when audio advances without a ready video frame

**Status:** built; physical candidate acceptance open

Build: 215
Issue: #888

Video dimensions no longer count as a displayed picture. First-video reporting
and black-picture detection now read the current visible video layer, reset
for each item, and recheck current eligibility before compatibility recovery.
Audio-only playback retains its progress metric. Hidden, background, PiP, and
external playback do not trigger a local decoder failure.

The [implementation record](../clients/APPLE-BLACK-VIDEO-IMPLEMENTATION.md)
separates this recovery repair from the unresolved 4K HLS rendering cause.
The physical workaround confirmed on build 213 was 1080p followed by Apply
with restart; candidate playback verification remains open.
