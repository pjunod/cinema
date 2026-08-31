# Let a server verdict reach the player it is about

Build: 96
Issue: #708

The control reporter was built without its `onExchange` handler, so the
parameter that carries a verdict back defaulted to a no-op and the server
could send an answer this client would never see. The session now holds the
last terminal verdict in a lock-guarded slot — the mirror of the snapshot slot
the player already publishes into, and for the same reason: the reporter is an
actor and the player is not, so neither ever calls the other.

A terminal verdict arms the words the next failure will carry rather than
causing one. The buffer already fetched is still worth playing, and the two
places this player already stops now show the server's sentence instead of
AVPlayer's generic error, whose message can carry a media URL.

The stall funnel also publishes what it is about to act on, so an exchange
carries which of the three stall kinds happened — a starved decoder, a silent
freeze, or the server's own delivery clock — rather than the vaguer picture
the periodic observer would have caught.
