# Private incident capture, delivery and local TV approval

Build: 233
Issue: #1037

Adds a closed bounded recorder, off-main protected account storage, portable
ZIP export and iOS/iPadOS system sharing. Pending uploads keep exact archive
bytes and keep authority in scoped Keychain items. A dedicated cookie-free,
redirect-refusing transport supports explicit consent, chunk resume, creator
refresh, direct submission and one-report invitations.

Help, connection/login, library playback and Live TV surfaces can freeze a
report. Apple TV presents QR/manual handoff, comparison and a locally
controlled approval button in a remotely blocked sheet. Reproduction stops
within five minutes or on background; bounded connection checks retain the
incident's original evidence and do not transfer media. Platform attachment
availability and release/device evidence remain open work in B08–B10.

Both target compilation and declared regression anchors are recorded in the
[canonical execution ledger](../features/REMOTE-DIAGNOSTICS-STATUS.md).
No per-packet XCTest, physical-device, production delivery or release
qualification is claimed.

The privacy manifest declares support content and diagnostic/interaction
measurements for app functionality, with account linkage and no tracking.
Local retention uses container-file metadata; timing exports only elapsed
intervals. Declaration names and API reasons follow Apple's [collected data
types](https://developer.apple.com/documentation/bundleresources/app-privacy-configuration/nsprivacycollecteddatatypes/nsprivacycollecteddatatype)
and [required API reasons](https://developer.apple.com/documentation/bundleresources/app-privacy-configuration/nsprivacyaccessedapitypes/nsprivacyaccessedapitypereasons).
