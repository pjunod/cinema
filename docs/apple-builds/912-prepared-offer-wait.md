# Wait for the prepared offer with the picture still up

Build: 166
Issue: #912

A quality change on a live session used to publish the viewer's intent and
then wait 1.5 seconds for the answer to *that same exchange*. The server
spawns the candidate after it has built that response, so the answer could
never carry a `prepare`: every directed quality change on this client fell
straight through to an in-place reopen, and the prepared handoff only ever ran
from the server's own push.

The wait is now its own thing, beside the stall ask rather than a wider bound
on it. It runs for up to twelve seconds, across exchanges, while the incumbent
keeps playing — which is what makes twelve seconds affordable where 1.5 is the
most a frozen picture can be asked to wait. It reads the server's new
`delivery.preparation` value: `staging` keeps the incumbent playing and
refreshes what the next exchange carries at 1 Hz, and `none` on an exchange
*after* the one that carried the ask is a decline. A server or relay that does
not send the field at all is waited out by the bound; absence is never read as
a refusal.

A quality change no longer pins a film position. It changes what is delivered,
not where the film is, so the fallback reopen samples the position when it
runs rather than at the tap — otherwise a twelve-second wait rewound the film
by twelve seconds. A viewer seek during the wait ends it at once and owns the
position outright: it already carries the new selection.

A successor is aligned to the incumbent's live position and that seek is
awaited before the item is put in front of the viewer, instead of being
exposed at the position it was staged at and left to catch up. The developer
tab gains two advisory rows — the last outcome and the last `preparation`
value — which are informational and gate nothing.
