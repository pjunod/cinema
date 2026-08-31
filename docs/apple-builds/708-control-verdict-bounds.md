# Bound the verdict and the evidence to the thing they describe

Build: 97
Issue: #708

Two limits the first cut of the return path did not have.

The evidence override expired only on forward progress, so a viewer who
stalled an hour into a title and scrubbed back to five minutes left it in
place — and the snapshot mapper ranks an override above everything the player
reports, so every exchange for the rest of the title would have said stalled.
Any movement ends the stall the evidence describes, including movement
backwards.

The terminal verdict had no expiry at all. It survived every reopen for the
life of the player, so a sentence delivered at ten o'clock could caption an
unrelated failure at eleven with total confidence. It is now bounded by the
lease the server itself gave the session that produced it, which is a number
already on the wire rather than one invented here.
