# Keep playback recovery and prepared handoff under one lifecycle

Build: 143
Issue: #257

Playback recovery now preserves the viewer's latest intent while a loaded
player waits, and a failed prepared replacement no longer disables later
attempts. Prepared quality and track changes use the same capability-aware
plan as a new session; throughput and fleet observations remain advisory.
