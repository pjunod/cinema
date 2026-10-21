# Adaptive resource custody — storage, startup and recovery limits

Status: open; journal and initial startup reconstruction compiled, not production
physical admission or qualified recovery. Integration owner Sol 6.1; existing
PR1248. The [implementation ledger](ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md) is the
current source/evidence and remaining M0–M7 authority. Nothing here authorizes
deployment, a device operation or deletion of unresolved accounting.

## What exists at c39c30542

The node-local authority is separate from replicated Store, recovery catalogue,
telemetry and caches:

    <storage.data_dir>/resource-custody/obligations.sqlite3
    <storage.data_dir>/resource-custody/installation.json

A bounded blocking owner retains the actual data-directory daemon lock, SQLite
connection and queued commands independently of an async waiter's lifetime.
The journal's typed private API supplies atomic family prepare, arm-before-
acquisition, identity enrichment, seal, hold and durable settlement. This API is
not yet connected to production acquisition/settlement. In particular, the
existence of a PREPARED/ACQUISITION_POSSIBLE row is not a launch capability.

Startup obtains single-use inputs from the actual SelectedStore and checks the
held lock inode against its no-follow path. The journal and actual physical
graphs use the same daemon process-instance identity and native boot observation.
No configured node ID or random token substitutes for host continuity. Startup
opens this one runtime before scratch clearing, system probes and media work;
serving AppState clones retain that same owner rather than opening more journals.

All recovered non-settled obligations become cleanup-only UnknownHeld in the
private ledger. Full costs and old allocation epochs survive a reduced cap;
shared family edges are retained but charges count each distinct obligation once.
All trial members retain their latch. New runtime bindings are minted locally;
old receipts and disk UUIDs cannot refund them. Complete old-owner records and
protected paths remain available for future positive reconciliation. No current
public accessor exposes the ledger, journal, an acquisition owner or receipt.

## Current refusal and platform behavior

No positive old-owner/whole-graph recovery backend is implemented yet. The interim
startup path therefore refuses pending custody BEFORE cleanup or legacy competing
work. This preserves protected paths but is not the final availability/recovery
implementation: selective reconciliation and unaffected serving still remain.
No normal production path has been switched to finite physical demands, and
successful root waits or file closes still do not establish descendant extinction.

An existing namespace, including an empty directory, malformed file or dangling
symlink, is Existing authority, not a reason to retry FirstInstallation. Missing,
corrupt, future or mismatched authority refuses without rebuilding an empty DB.
An unreadable namespace is not absence. Do not remove either file, edit costs,
copy in an older witness/DB, or fall back to legacy admission to recover headroom.

The installation implementation currently supports private Unix storage with its
actual locking/sync/filesystem requirements. Windows durable ACL/publication is
explicitly unsupported. A fresh unsupported platform with no custody namespace
keeps ordinary legacy behavior and has NO physical ledger; that is not zero
usage or permission for adaptive expansion. Existing authority there refuses.
There is no fallback from a failed Unix journal open to empty accounting.

## Backup, copies and moves

This local authority is not part of logical cluster snapshots or peer state
transfer. Preserve the current local DB, witness and any crash journal separately
from catalogue backups. Do not replace them with older snapshot contents or
relabel obligations when changing node identity. The offline restore/reidentity
integration and its refusal cases remain part of the implementation backlog.
A logical backup of a fresh node never settles the source node's physical work.

The private v1 witness binds the retained directory's platform, canonical path,
device and inode. It rejects detectable moves/copies and same-path directory
replacement without rewriting identity. This is an installation-LOCATION guard,
not authenticated host/deployment continuity, old-owner fencing or a refund.
Missing/unknown identity does not migrate automatically. A remount may
conservatively refuse; no supported move, rebind or force-adopt API exists.

Matching fields do not detect coherent VM/filesystem clones, inode reuse,
coincident cross-host identities or old DB+witness bytes restored INTO the same
retained directory. Those operations remain unsupported for live custody. Local
hashes and UUIDs cannot detect lost valid history. A changed boot ID alone also
cannot prove that a copied source host's processes or external resources ended.

## Evidence and remaining work

The released journal was serially adopted with original authorship. Its 28
regression bodies plus 10 startup/runtime/lock/namespace bodies are compile-
checked, NOT executed. Pinned Rust1.97.1 core+daemon all-target Hiqlite-contract
cargo check passes at c39c30542. Private journal APIs still produce warnings;
there is no current Clippy -Dwarnings, independent full-candidate review,
behavioral qualification or platform physical-close pass.

Remaining work includes cancellation-independent write-ahead acquisition and
settle-before-refund integration; positive normal/restart/fencing and typed-resource
backends; real production demands and all adapters; safe learning/pacing, M4
remote runtime and M5 peer/deadline/domain/drain policy; diagnostics; independent
complete-candidate Fable review/fixes and once-only affected tests/fast lane before
an authorized merge. R2/R3 targeted design decisions are not that final review.
