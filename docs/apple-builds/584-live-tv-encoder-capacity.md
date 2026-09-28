# Name an encoder refusal on Live TV as the encoder

**Status:** built · 2026-09-28

Build: 194
Issue: #584

A Live TV start the tuner owner refuses for video-encoder capacity now says
so — "The tuner owner's video encoder is busy" — instead of "All Live TV slots
are busy", which described tuners that were idle. The server code is
`encoder_capacity`; the shared start-cases fixture pins the copy on all
three clients. See the
[RCA](../streaming/LIVE-TV-SLOTS-BUSY-OVER-BACKGROUND-RCA.md).
