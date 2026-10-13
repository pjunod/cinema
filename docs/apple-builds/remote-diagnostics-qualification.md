# Offline incident UI qualification

**Status:** open — qualification and startup recovery active.

Build: 235
Issue: #1066

Registers the diagnostics reporting UI regression in the iOS test target.
Preserves local history during incident recovery and resets the form scroll
context when the selected incident changes.
The test drives the shipped anonymous report form with an unavailable
Cinema endpoint, checks the local export and explicit upload boundary,
then relaunches and verifies recovery of the same saved incident. The tvOS
variant uses the real remote API and the exact focused SwiftUI action cell.

The [canonical ledger](../features/REMOTE-DIAGNOSTICS-STATUS.md) records actual
results and fixture failures. No physical-device, public delivery or signed
release qualification follows from simulator execution.
