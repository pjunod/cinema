# Do not discard the verdict that stopped the reporter

Build: 99
Issue: #708

A terminal verdict is answered and then ends reporting in the same instant.
The ask gave up the moment it saw a stopped reporter, without reading its
answer slot again — so it discarded the one verdict it most needed to see, and
the client fell through to its own guess for exactly the case where the server
had been certain.

Found by the Android mirror's terminal test, which is the argument for having
written it: the hold path passed throughout, because a hold leaves the reporter
alive and the next poll reads the answer normally.
