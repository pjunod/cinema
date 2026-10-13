# Offline incident UI qualification

**Status:** open — archive export and recovery qualification active.

Build: 236
Issue: #1066

Registers the diagnostics reporting UI regression in the iOS test target.
Preserves local history during incident recovery and resets the form scroll
context when the selected incident changes.
The test drives the shipped anonymous report form with an unavailable
Cinema endpoint, checks local export with no ordinary upload controls,
then relaunches and verifies recovery of the same saved incident. The tvOS
variant uses the real remote API and the exact focused SwiftUI action cell.

The [canonical ledger](../features/REMOTE-DIAGNOSTICS-STATUS.md) records actual
results and fixture failures. The archive-first follow-up also verifies the actual exported ZIP and system
share-sheet action. Native TV transfer is unfinished; hosted delivery is
deferred. No physical-device or signed
release qualification follows from simulator execution.
