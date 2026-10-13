# Preserve launch IDs and release refused cadence transfers

Build: 232
Issue: #994

The DEBUG physical-acceptance launch path parses explicit decimal integer
arguments directly, preserving full-width item IDs and refusing malformed values
without a saved-default fallback.

The Apple cadence inspector cancels its actual transfer task on every exit and
refuses HTTP error bodies as sample evidence. Refused responses remain terminal;
no client retry masks server failures, and real decoded-cadence proof is required.

The initially attempted HTTP 503 retry was removed at Paul's direction. The
server's prepared-session failure must be diagnosed and corrected at its source.
Physical first-switch qualification remains failed on terminal responses; no
successful target landing is claimed.

The canonical ledger is
[Continuous Quality Build §10.311](../playback-control/CONTINUOUS-QUALITY-BUILD.md).
