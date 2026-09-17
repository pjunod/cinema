# Measure what the prepared switch cost the viewer

Build: 167
Issue: #912

The prepared quality handoff now reports what it did to the picture and the
sound. Three rows under a new PREPARED SWITCH group in the playback-info
ledger, and the same three on the developer tab: dropped frames over the two
seconds either side of the commit, the access log's stall count over the same
window, and the wall time from the tap to the successor's first frame.

Every reading is a read of an `AVPlayerItemAccessLog` AVFoundation already
writes, taken on the two-second status poll this controller already runs plus
one synchronous read on either side of `replaceCurrentItem`. Nothing was added
to the audible or visible path, and no reading is consulted by anything that
decides anything: an unmeasured window reports itself as unmeasured rather than
as a failure, and the acceptance bar these numbers are compared against lives
in `docs/playback-control/QUALITY-SWITCH-CONTINUITY-RESULTS.md`.

An `MTAudioProcessingTap` gap detector is deliberately not shipped. Installing
one means setting the item's `audioMix`, which puts a real-time callback of
ours inside the audible path of every session; a callback that overruns is a
silence the viewer hears. The developer tab states that condition and says it
is not met, and states nothing that can block the handoff.
