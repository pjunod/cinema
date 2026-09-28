# Measure dispatched seeks and attribute live viewing time

**Status:** open — implementation complete; merge and physical rollout pending.

Build: 194
Issue: #582

Dispatched viewer seeks report one terminal outcome: resumed after the existing destination frame proof, or abandoned when superseded or stopped. Their monotonic duration is separate from startup TTFF. Live progress names the active delivery method; offline replay omits it.
