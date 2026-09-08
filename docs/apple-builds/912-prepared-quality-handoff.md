# The prepared quality handoff, on iPhone, iPad and Apple TV

Build: 119
Issue: #912

Apple declares `prepare_replacement`, so the server now tells this client
about the successors it has been staging and reaping unseen. A quality change
on a live session publishes the viewer's intent and waits up to 1.5 seconds
for the server to offer a staged successor; VOD keeps the behaviour it has
today, because the server's throughput floor cannot be satisfied on a VOD
session at all.

An offered successor becomes a second `AVPlayer` that is muted, never on a
layer and never asked to play. It is primed to the viewer's film position
through the successor's own `media_origin_ms`, reports metadata and buffer
readiness, and is switched in by handing its item to the authoritative player
— which keeps the layer, Picture in Picture, the time observer and every KVO
already installed. The commit is sent only after the successor's own first
qualifying frame, taken from the item's video output against the film position
the switch happened at, never from a timer.

Every exit frees the second pipeline: a seek, a second quality change, an
audio change, backgrounding, or the player ending. Each of those settles the
staging as `aborted`, and a successor that cannot be made ready inside its
bounds settles as `failed` and falls back to the in-place change this platform
has always done. Leaving a staging to the server's 330-second deadline would
cost that session its only preparation for the rest of its life.

Twenty-eight focused tests cover the wire contract, the acknowledgement
ledger and the coordinator on both iOS and tvOS simulators. A directed
replacement on real hardware and the fallback interruption measurement remain
separate acceptance steps; compilation and simulators do not prove a second
decode pipeline on a device.
