# Every class in the surface contract now has a way to be drawn

Build: 151
Issue: #278

Build 147 landed the Apple presenter; an audit found that parts of it could
never run. Four of the fixture's source rows had no raise site on this client
and one view branch could not be reached at all, so the classes behind them
were code the viewer could never see.

**`buffering` could not be drawn at all.** `waitingToPlayAtSpecifiedRate`
still drove a spinner of its own through `isPlaybackWaiting`, drawn by a
second `if` in `PlayerView` below the one that drew the stream-change
spinner — two owners for one pixel, which is the defect the contract exists
to kill. The wait is now a `media_waiting` fault, the presenter draws it as
`buffering` once it has lasted the contract's 350 ms, and both legacy flags
are gone. The debounce is the reducer's: nothing here keeps a second timer.

**The staged loading overlay is a fault.** `isChangingStream` raised nothing;
it now raises `client_preparing` on the transition that sets it and settles
that fault on the transition that clears it, so the overlay's life is exactly
the flag's life whether the open attaches, is refused, or is superseded.

**A readiness deadline names its own row.** `retryAfterReadinessTimeout`
borrowed the ladder's `owner_recovery_step`; it raises
`readiness_deadline_rungs_left` now, which is contract row 14 and names Apple
outright. Same class, same pixel — the ledger stops calling a readiness
timeout an ordinary fallback.

**`log_only` was raised by nobody, on any client.** Row 18's three Apple
equivalents — a prepared successor given up, a control exchange that came
back with nothing (including `404 session_gone` on a successor's first
exchange), and a session status poll that answered nothing — now emit
`surface_log_only` and draw nothing at all. One row per fact per attached
generation, so a reporter failing every cadence is one log line.

**The unreachable Keep Waiting branch is gone.** `failureActions` strips
`keep_waiting` whenever `retry` is present, because on this client they are
the same primitive, and every blocking class's defaults offer `retry`. The
label is deleted and the reason is written where the next reader will ask.

One thing this build adds that is not a raise site: the presenter now has a
clock. Every timing the contract states is measured by the reducer when its
caller applies an event, and AVPlayer's periodic observer stops firing the
moment the film clock stops — which is exactly when a wait needs drawing. A
500 ms task feeds `.tick`, the twin of the web's `playbackProgressTick` on
the same interval. It samples nothing and detects nothing.

**And one thing the audit did not name, found by closing the first.** A
`buffering` fault was retired only by presentation evidence, and a paused
picture never produces another sample — so a buffer that filled while the
viewer had paused left a spinner over a still frame until the generation
changed. That is the overlay outliving the thing it described: the defect this
contract exists to kill, reintroduced by the migration itself. The Android
session found it independently and the web had it too. Ruled: a `buffering`
fault is about a player that wants media, so a viewer who pauses makes it about
nothing. The Apple wiring is `wantsPlayback` — the viewer's own intent, and the
flag the owner's `stopForBlockingSurface()` deliberately leaves alone, so the
owner's stop can never retire a fault.

No threshold, budget, detector or ladder moved.
