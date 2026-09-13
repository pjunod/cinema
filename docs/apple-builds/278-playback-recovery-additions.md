# A create the server is still building is retried, not called a failure

Build: 148
Issue: #278

M5 of the playback surface contract
([PLAYBACK-SURFACE-CONTRACT.md](../clients/PLAYBACK-SURFACE-CONTRACT.md) §3.3
row 6, [implementation](../clients/PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md)
§4.6). M2 is build 147; this is the only milestone of the effort that changes
what the player does, and on this client it changes exactly one thing.

A session create refused with `startup_timeout`, `media_owner_transition`,
`vod_index_pending` or `vod_engine_unattested` is the server saying "not yet",
and it is now answered by re-posting the *same* create under the *same*
`request_id` after 1 s, then 2 s, then 4 s. The identity matters: the server
persists a create's answer under `request_id`, so a replay recovers the session
it already made rather than spawning a second encoder. While the ladder runs
the viewer sees the `preparing` spinner with the server's own sentence instead
of "Couldn't start playback."

The ladder is bounded by an **absolute** sixty seconds measured from the first
attempt, on a watchdog rather than between attempts, so a server that holds
every create for three minutes cannot stretch it. When the deadline passes —
or the third retry fails — the recovery owner stops the player and raises
`exhausted` with Try again and Close. A create that finally succeeds *after*
the deadline is released through `endHlsSession`, never attached: the viewer
has already been told this attempt is over, and an encoder nobody is watching
is a hardware slot held for nobody.

Scope, and what it is not. The ladder runs in the `start` context only. A
create refused over a predecessor the viewer is still watching is a refused
*change* (row 7), keeps its banner and is not retried; a stall reopen is not
retried either. A 503 the server did not explain is not a "not yet" answer —
the code is what says so — and every other refusal keeps exactly the handling
it had. No threshold, budget, detector or ladder was retuned, the presenter
gained no side effect, and `refusalSurfaceOutcome` still declines to classify
`create_503_not_yet`, because that function feeds the raise that happens *after*
the owner's stop and a progress class there would be a spinner over a stopped
player.

The two other M5 additions are the web's (one bounded `hls.startLoad`) and
Android's (`BEHIND_LIVE_WINDOW` on a finite timeline). Neither touches this
client.

Tested: `PlaybackCreateRetry`'s ladder, its absolute deadline, and the fact
that the retried codes are the fixture's row rather than a second list.
`AppleClientTests` also runs the fixture's three new ordered-event cases along
with the other 57. **Not tested here:** the async sequence itself — its
watchdog, its late-session release and its cancellation — needs a Mac, and this
build was written without one.
