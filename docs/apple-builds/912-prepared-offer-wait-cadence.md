# Make the prepared-offer wait winnable, and bound the switch it leads to

Build: 167
Issue: #912

Three things the adversarial review of the wait found.

The 1 Hz cadence was not a cadence. It refreshed what the next exchange would
carry and then waited for the pump, which sleeps out `next_exchange_ms` — five
seconds, set once at bootstrap. A twelve-second wait therefore bought two or
three exchanges against a server whose priming budget is forty-five seconds,
so the offer would almost never arrive inside the bound and the fleet counters
would have gone on reading zero. The wait now solicits its own exchange, paced
by the reporter's own 250 ms floor, for as long as the server says `staging`.

The decline rule did not survive its caller. It asked whether an accepted
answer had been observed before, and the caller polls every 25 ms with the same
answer until the next exchange lands — so the dispatch exchange's own `none`
declined a quarter of a second in, rather than one exchange later. It asks now
whether a *later exchange* has looked. It matters because a dispatch `none` is
transient: the server withholds a purpose while the incumbent is waiting for
capacity, and the next exchange is the one that would have said `staging`.

The alignment seek before the switch is bounded at four seconds. An
AVFoundation seek that never came back left the commit suspended with no way
out: the readiness monitor had already been dropped, `.switching` made every
route into the coordinator bail, the server held its one preparation slot for
the full deadline, and the viewer's tap produced nothing at all because the
prepared path had already claimed it. The bound turns all of that into the
ordinary in-place change.
