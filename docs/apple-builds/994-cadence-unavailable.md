# Preserve cadence verification across a transient unavailable response

Build: 232
Issue: #994

The Apple prepared-change cadence inspector retries HTTP 503 on the same URL and
Range within its original absolute deadline. Refused bodies cannot supply sample
evidence; each attempt cancels its actual transfer task. Authorization and gone
responses remain terminal, and the decoded-cadence proof remains required.

The DEBUG physical-acceptance launch path parses explicit decimal integer
arguments directly, preserving full-width item IDs and refusing malformed values
without a saved-default fallback.

Both iOS and tvOS source and test targets compile without running unit suites.
Targeted physical runs remain failed: the phone saw fragment 503 then 410, while
the TV saw playlist 410. No successful target landing or native qualification is
claimed. The canonical ledger is
[Continuous Quality Build §10.311](../playback-control/CONTINUOUS-QUALITY-BUILD.md).
