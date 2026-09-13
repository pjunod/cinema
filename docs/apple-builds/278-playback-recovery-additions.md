# A create the server is still building is retried, not called a failure

Build: 149
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

Scope, and what it is not. The ladder runs in the `start` context only, and
the gate is `surfaceContext == .start` — which is "this playback has never
presented", not "this is a cold start". A create refused over a predecessor the
viewer is still watching is a refused *change* (row 7), keeps its banner and is
not retried. A stall reopen on a stream that HAS presented is not retried for
the same reason; a reopen on one that never did still runs the ladder, which is
the honest answer for a stream that has yet to produce a picture. A 503 the
server did not explain is not a "not yet" answer —
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
with the other 57. The sequence itself is tested too, in
`PlayerOperationOwnershipTests`, through two new constructor seams —
`waitCreateRetry` (M5's one clock, so a sixty-second bound costs the suite
nothing) and `releaseHlsSession` (so "a late session is RELEASED" is a thing a
test can see rather than a claim): the ladder's three delays under one request
identity, the deadline raising `exhausted` on the clock while the create is
still in flight, the late session being released and never attached, and a
quality change arming no deadline at all.

**None of it has been compiled.** There is no Xcode on the machine this build
was written on, so every assertion above is unrun.
