# Return Apple TV playback to its title after sleep

Build: 284
Issue: #1347

On Apple TV, putting Cinema's on-demand player in the background, including
automatic sleep while paused, now closes the stream and hands the last position
back to the title screen. Wake returns there, and Resume opens a fresh stream
instead of exposing the retained item's expired-session error. Temporary
inactive system controls keep playback attached; iOS keeps background playback
and Picture in Picture.

Focused simulator checks cover teardown before dismissal, position handoff,
inactive transitions, pending autoplay and the existing session-release path.
Physical Apple TV automatic sleep/wake acceptance remains pending.
