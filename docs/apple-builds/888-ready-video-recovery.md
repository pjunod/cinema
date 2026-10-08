# Recover when audio advances without a ready video frame

**Status:** built; physical candidate acceptance open

Build: 216
Issue: #888

Video dimensions no longer count as a displayed picture. First-video reporting
and black-picture detection now read the current visible video layer, reset
for each item, and recheck current eligibility before compatibility recovery.
Audio-only playback retains its progress metric. Hidden, background, PiP, and
external playback do not trigger a local decoder failure.

The [implementation record](../clients/APPLE-BLACK-VIDEO-IMPLEMENTATION.md)
records the physical cause: background CQ Lab process interference. Stopping
the two test processes restored the unchanged original 4K Avatar and Tom
HDR10 samples. Temporary test apps were removed and normal Noirr Cinema
build 213 restored. Candidate build 215 has not been installed during viewing;
its physical acceptance remains separate from the successful device cleanup.
