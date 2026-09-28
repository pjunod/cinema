# Recover Apple seeks that never complete

**Status:** built

Build: 192
Issue: #473

A native seek now has an eight-second completion deadline. If AVPlayer never
calls back, the current seek enters the existing bounded same-delivery recovery
at the requested position. Cancellation releases the caller even when AVPlayer
ignores it. Late callbacks cannot replace a newer playback item or seek.

The fresh item’s initial positioning seek uses the same deadline, so recovery
cannot wait indefinitely on a second AVPlayer callback.

The focused simulator regressions cover timeout, cancellation, late completion,
replacement ownership and a suspended recovery seek: six iOS and four tvOS
cases passed on September 27, 2026. The iOS Release simulator and signed tvOS
Release device builds compiled.

Bedroom’s physical remote test played file 6629, *In from the Side*, from
1:25:00, waited for the clock to advance, then issued two backward seeks.
The test passed; captured pictures changed while the clock advanced from
1:24:44 to 1:24:47. This verifies picture and clock recovery on that run, not
an audible-output check or reproduction of the original intermittent hang.
The injected regressions force the missing-callback failure. PR #473 records
the merge gate and final Release installation evidence.
