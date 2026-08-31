# Ask the server before deciding a stall

Build: 98
Issue: #708

Every stall owner on this platform funnels into one function, and it decided
by itself: one same-delivery reopen, then a bounded ladder, then a stop. That
funnel now submits its evidence and waits, briefly, for the verdict the
evidence earns.

The ask goes before the funnel's first statement, which is the detail worth
getting right. `next(for:)` sets `attempted` and returns `.stop` on every
later call — it is the spend — so an ask placed after it would let a server
`hold` permanently retire the one reopen this client had, which is the exact
failure a hold exists to avoid.

A `terminal` verdict shows the server's sentence instead of the client's
generic one. A `hold` defers the reopen without spending the attempt and tells
the viewer why, because a hold is never lifted by anything this client does
and the recovery monitor re-enters on its own cadence — a client that only
returned would leave a frozen picture with nothing said. `retry_resource` does
the same and lets the monitor come round.

Anything else, including no answer inside the bound, is today's path exactly.
That is the branch every node in the fleet takes.
