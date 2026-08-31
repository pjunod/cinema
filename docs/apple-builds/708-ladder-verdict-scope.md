# A terminal verdict rules out one reconnect, not the ladder

Build: 101
Issue: #708

Supersedes the behaviour described in
[708-item-failure-asks.md](708-item-failure-asks.md) (build 100), which said
*"only `terminal` short-circuits"* and ended the whole ladder on one. That was
too wide. That note is left as written — a build note records what its build
did — and this one records what replaced it.

`terminal` is emitted only for `unsupported` and `invalid_configuration`, and
the server documents `is_permanent` as *"whether retrying this source,
**unchanged**, can ever succeed"*. `unsupported`'s own sentence is "this source
cannot be carried by **this delivery pipeline**" — a copy-producer exit. The
server treats it as recoverable by changing the pipeline:
`execute_prepublication_copy_retry` is admitted for exactly this reason, and
refuses every other reason.

So the verdict is about the recipe, not the source, and it now governs exactly
one thing: whether `retryEstablishedHDRDelivery` spends its reconnect. That
reconnect re-attaches the *identical* recipe and hopes, which is the retry
`is_permanent` rules out.

Two things it deliberately does not govern.

`retryWithNextCompatibilityFallback` asks the server for a different pipeline,
which is what the verdict leaves open. That rung only unlocks when AVFoundation
itself rejected the media (`isCompatibilityPlaybackFailure` — a local decoder
or container rejection), so the case is narrower than a producer refusal, but
it is the case where a picture was still available.

And whether the established-HDR rung *runs* at all. That rung also carries a
stop, and the stop is its real job: a stream that has already rendered real HDR
never descends to SDR, because tone-mapping a picture that was HDR a second ago
hides a transport fault behind a worse image. Skipping the rung outright — the
first shape of this fix — would have handed the compatibility ladder a stream
it has always been vetoed from. The verdict skips the reconnect and falls
straight to the stop.

The predicate reads the *retained* verdict rather than the ask's own answer, so
that it agrees with the failure sentence shown below it. A `terminal` response
stops the reporter the instant it arrives, so a second item failure in the same
control session gets nothing from a fresh ask while the retained slot still
holds the verdict — and a node failover replaces the item without starting a
new control session, so that second failure is reachable.

The ask itself is unchanged and still goes ahead of every rung: the exchange
has to be out before a reopen replaces the item it describes.
