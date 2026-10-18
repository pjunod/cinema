# Auto leaves an original it cannot carry, and a stall shows Buffering

Build: 262
Issue: #1153

Auto costed a quality rung only by a measurement of the whole title encoded
at that rung and threw away the cost the server declared for it, so on a
title that had never been encoded whole no lower rung could be chosen under
link pressure and a 28.8 Mb/s remux was held on a 9 Mb/s link until it
stalled. Rungs are now costed by their declared rate until a measurement
replaces it, an original is costed by its declared source rate, link
pressure on it no longer waits for the buffer to drain, and a completed
object stays link evidence for as long as it took to arrive.

While the film clock was stopped the player kept reading as presenting, so
the Buffering surface was raised and retired twice a second and never drawn.
The presenter now samples the stopped clock itself and keeps the wait up.
