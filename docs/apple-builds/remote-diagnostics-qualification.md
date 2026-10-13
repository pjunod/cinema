# Offline incident UI qualification

**Status:** built — TV ZIP download, phone export and recovery have software qualification.

Build: 237
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
share-sheet action. Native TV local archive transfer is built and being qualified; hosted delivery is
deferred. No physical-device or signed
release qualification follows from simulator execution.

Build237 adds a physically selected, temporary local download for the TV's
own immutable ZIP. The QR opens a phone page; Download consumes the link once.
Closing/background/authorization retirement revokes it. The UI warns about
unencrypted trusted Wi-Fi. Actual tvOS remote UI passes selected archive and
close revocation; iOS/tvOS capture/wire suites each pass eleven cases. Final
review, fast gate and the exact follow-up source are in the canonical ledger.
