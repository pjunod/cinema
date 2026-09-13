# Live TV now says what the player is doing

Build: 153
Issue: #300

The tvOS Live TV player now separates an ordinary start from a playback stall.
It shows **Catching up to live** only when AVPlayer reports
`toMinimizeStalls` continuously for 350 ms; the normal
`evaluatingBufferingRate` phase remains quiet. The controller also publishes
the distance behind the playlist edge, media buffered after the playhead, the
pause instant, and a fullscreen-only message. Lineup-loading copy never leaks
onto the picture.

Fullscreen Live TV now uses the reviewed playback surface: a compact telemetry
strip, a programme and progress band, five focus-contained actions, a waiting
tile, and an unmistakable paused state. The invisible reveal layer becomes a
focus target only after the controls and guide are gone, and revealing the
controls returns focus to Pause or Play live. The iPhone surface is unchanged.

Info on tvOS is now a two-column snapshot ledger. It reads every value the
programme, channel, delivery, signal, and player models actually carry,
including summed AVPlayer access-log stalls and dropped frames. Unknown
counters say `unknown`; absent values omit their row; the stream row reports
the delivered packaging without inventing a segment duration.

The simulator compilation and nine focused Live TV regressions are recorded in
PR #300. The Apple TV focus, pause-expiry, first-frame, and first-minute stall
checks remain part of the combined HDHomeRun physical pass before this effort
can be promoted to `main`.
