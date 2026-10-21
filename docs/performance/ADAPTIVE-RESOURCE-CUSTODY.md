# Adaptive resource custody — storage, startup and recovery limits

Status: open; journal, startup and private owned-operation bridge compiled, not production
physical admission or qualified recovery. Integration owner Sol 6.1; existing
PR1248. The [implementation ledger](ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md) is the
current source/evidence and remaining M0–M7 authority. Nothing here authorizes
deployment, a device operation or deletion of unresolved accounting.

## What exists at 66291c562

The node-local authority is separate from replicated Store, recovery catalogue,
telemetry and caches:

    <storage.data_dir>/resource-custody/obligations.sqlite3
    <storage.data_dir>/resource-custody/installation.json

A bounded blocking owner retains the actual data-directory daemon lock, SQLite
connection and queued commands independently of an async waiter's lifetime.
The journal's typed private API supplies atomic family prepare, arm-before-
acquisition, identity enrichment, seal, hold and durable settlement. This API is
connected through a private retained-operation book, not ordinary production.
In particular, the
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

The runtime owns complete provisional families before the first wait, including
actual graph generations and pending journal receivers. Dropping a waiter cannot
refund or replay a named root. Prepare commits before current epoch/drain/deadline
recheck; every root slot arms before the actual retained launcher and enriches
its identity afterward. Ambiguous failure seals/holds rather than retrying a slot.
Task/typed-resource durable adapters remain absent and cannot use legacy graph
entry points to bypass write-ahead custody.

Private cleanup consumes the original nonclone proof, retains it on queue refusal
or uncertain commit, and refunds only after exact durable settlement. A public
cleanup refusal returns that original proof. Reservation/graph Drop cannot refund
a durable tag; restart reconstruction installs tags too. Retained successor pins
fence owner retirement until users close or transfer. Corrected data-only facts
require definite rollback; unknown operations retain their exact payload/UUID.

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
regression bodies, 10 startup/runtime/lock/namespace bodies and 12 new owned-
operation/worker contracts are compile-checked, NOT executed. Pinned Rust1.97.1
core+daemon all-target Hiqlite-contract cargo check passes at 66291c562 (95.955s).
Private APIs still produce warnings because production composition is missing;
there is no current Clippy -Dwarnings, independent full-candidate review,
behavioral qualification or platform physical-close pass.

Synthetic private test receipts prove only protocol shape, not kernel or physical
closure. Remaining work includes existing-supervisor/production composition of
the retained bridge; positive normal/restart/fencing and typed-resource backends;
shared-role transfer, real production demands and all adapters; safe learning/pacing, M4
remote runtime and M5 peer/deadline/domain/drain policy; diagnostics; independent
complete-candidate Fable review/fixes and once-only affected tests/fast lane before
an authorized merge. R2/R3 targeted design decisions are not that final review.
