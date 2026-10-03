# Jellyfin play bindings — retain identity, delegate native ownership

**Status:** open · J1 storage implemented, focused backend regressions pass ·
**Written:** 2026-10-02.

Companion to [the build contract](JELLYFIN-COMPATIBILITY-BUILD.md) and
[shared service seams](JELLYFIN-SHARED-SERVICES.md). These records describe
negotiations and exact native references. No public facade route is mounted;
activation, resource release and watch revisions still require the adapters.
Physical Apple TV qualification remains deferred to the frozen J6 candidate.

## 1. Durable identity — no second owner inventory

Each play UUID retains its authenticated user/token/device/client-family
scope, stable player ID, native item/file IDs and permanent opaque item/file
incarnations. Its bounded JSON payload records source/profile fingerprints,
selection, source origin and the expected native request fingerprint. It contains no plaintext access token or device
ID. A binding references either a native media incarnation or a direct-file
grant; it does not copy native owner, epoch, renewal or terminal-reason fields.
Any ingress resolves a media reference through `MediaSessionRoute` to learn
the current owner. Ownership transfer does not rewrite compatibility metadata.

Admission checks the current compatibility login and file-to-item membership,
including both live opaque incarnations, in the same transaction as insertion.
Activation checks that those incarnations remain live and the native route
belongs to this user/player, matches the expected request/origin and is
published, or the direct grant belongs to
this user/file/source token and is unrevoked and unexpired. Native session
activation claims remain responsible for producer ownership and replacement.

## 2. Bounds and cleanup — active playback survives negotiation pressure

Pending records expire after ten minutes. Conditional insertion admits at most
64 pending records per login/device/family and 4,096 across the server; cleanup
and insertion share one backend transaction so concurrent ingresses cannot
bypass either ceiling. Only expired pending and terminal rows are removed.
Active references are never evicted to admit another negotiation. Their
resource lifetime remains the native coordinator's responsibility.

A terminal decision changes only that exact play UUID and retains its native
reference for 24 hours. Unique reference indexes prevent another play UUID
from aliasing that native incarnation or direct grant. That retention is a compatibility lookup bound, not a
native capability lease. After cleanup a late UUID lookup is absent and must
fail closed; adapters cannot guess a successor by user/item. UUIDs are generated
by the server for new negotiations and are never reused. An ended UUID cannot
be activated again. Ending metadata does not itself release native resources.

SQLite migration 94 and replicated migration 72 install the same table and
indexes. Import preserves every payload, scope, state, manual revision and
exact reference. The native incarnation column intentionally has no cascading
foreign key to `media_sessions`: native terminal cleanup must not erase the
reference identifying a compatibility tombstone.

## 3. Verification and remaining work

Focused backend contracts cover pending pressure without active eviction,
expiry fences, direct-grant scope, per-UUID terminal records, native owner
transfer and a successor surviving an old play's late stop. A separate
contract deletes a file, reuses its native integer and requires the retired
opaque incarnation to refuse activation while the current incarnation succeeds.
They run on both SQLite modes and on a remote client backed by three voters.
The four lifecycle/import contracts pass. The server-limit contract fills
4,095 pending records across distinct login scopes, races two ingresses for
the last slot and admits exactly one on each backend; active playback survives
the full 4,096-row budget. Fifteen SQL parameter checks, four authority-read
census checks, transaction/import inventories, frozen-v42 migration parity,
seven document/source guards and mandatory workspace Clippy/format/JavaScript
checks pass on pinned Rust 1.97.1. The final task must be rechecked after its
base advances and must pass the Effort development gate before integration.

The manual revision field starts at zero. It is reserved storage, not a
manual-watch fence: J3 must capture and compare the transactional item/user
revision through ingress, queued coalesced writes and the eventual Store write.
Replacement login/logout must enumerate and release their exact bound plays
through the native coordinator. Public negotiation/activation, producer
replacement, terminal durability and cross-node event ordering remain open.
