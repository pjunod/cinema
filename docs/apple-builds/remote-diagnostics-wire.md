# Remote diagnostic wire — closed incident manifest validation

Build: 234
Issue: #1037

Adds the v1 diagnostic manifest decoder and shared malformed/privacy fixtures
for iOS and tvOS. Duplicate keys, arbitrary fields, invalid coverage, unsafe
paths and counters outside the contract are rejected. This is the report
format foundation; the report UI and delivery are still being built.

Simulator fixture evidence was obtained before the workflow ruling; no
physical-device, deployment or release completion is claimed. The canonical
[execution ledger](../features/REMOTE-DIAGNOSTICS-STATUS.md) owns subsequent
implementation and final fast-lane evidence.
