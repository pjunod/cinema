# Adaptive resource custody — storage, startup and recovery limits

**Status:** open · implementation active · updated 2026-10-29. Selected Linux
owners use the retained journal and operation book; complete admission and
recovery remain unfinished. The [implementation ledger](ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md)
records source, execution evidence and remaining M0–M7 work separately. No
unresolved accounting may be deleted to regain capacity.

## Registered planning closure — non-entry needs its own proof

The original probe and engine tasks are acquired owners once registered. Each
waits first on its own one-shot start receiver. Dropping that sender without
sending can establish non-entry only after the same registered handle joins
with its private non-entry result. Panic, lost outcomes and generic task errors
cannot produce that witness.

The [planning collector](../../crates/plurx-core/src/adaptive_resources/runtime/operations/planning_tasks.rs)
retains both original handles and checks their binding, member and registration
identity. The [settlement adapter](../../crates/plurx-core/src/adaptive_resources/runtime/operations/planning_settlement.rs)
also requires the same sealed graph's native absence proof. An acquired source
parent remains separate: its actual aliases must drain and its original close
operation must finish before P can settle. Cancellation or a surviving alias
retains these same witnesses and owners for retry.

| Planning task state | Accepted close evidence |
|---|---|
| Never registered | Existing task-never-acquired disposition, with separate original source closure. |
| Registered and positively joined without entry | Distinct `FixedProbeDiscoverTaskNeverEnteredV1` or `FixedPlanningEngineTaskNeverEnteredV1` fact, bound to the original task and join. |
| Entered, failed, panicked or lost outcome | Existing supported native closure, otherwise full retained charge. A never-entered fact cannot stand in for it. |

The new [typed facts](../../crates/plurx-core/src/resource_custody/codec_v2/settlement.rs)
contain no invented executable or media digest. They require absent task
observations and matching cross-member root/copy facts; old decoders reject
unknown tags. Full borrowed DTO and size validation precedes consuming any
one-shot proof. The original P native and application owners are destroyed before the matching
durable settlement releases P. M remains separately counted until its actual
application owner is gone. Nonzero original overage is not cleared by task non-entry.

Eight runtime/codec regressions are authored for these paths; exact-source
compilation and behavioral execution are pending. This implements selected
planning closure, not complete R1 selective recovery, native peak bounds or
legacy admission coverage. Startup still refuses if
`has_unreconciled_custody()` remains true after reconciliation, before competing
legacy native work; its remaining replacement is tracked in the ledger.

## Historical storage and bridge record through b63cb217c

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
The concrete discard-root-pipes task now arms before creation, retains its actual
JoinHandle and enriches exact graph/member identity. It takes no arbitrary future
or callback, exports no pipe, and uses two fixed read buffers. Sealed abort plus
actual join closes ONLY those pipes, not descendants or external work. Generic
legacy writers remain non-certifying; other typed-resource adapters are still
absent and cannot bypass write-ahead custody through legacy entry points.

Concrete source/pipe adapters now share combined pre-acquisition bounds. Every
root reserves three possible pipe slots before launch; source handles cannot
borrow those slots again. Pending reads retain their full requested buffer even
after their task completes until its result is consumed; audited drain buffers
use the same charge. Unknown generic writer costs are not finite spare memory.
This closes a local accounting gap, not process/containment kernel-overhead
estimation, durable source-file recovery, a physical receipt or production demand.

LIVE Windows process containment now requests termination and queries population
through each original retained unnamed Job, including after its leader exits.
Only the suspended-before-assignment launcher establishes descendant coverage;
ordinary attach-after-execution Jobs cannot certify it. A sealed complete root
inventory plus actual waits and native active-zero queries supplies private
graph-bound PROCESS-ONLY evidence. Ambiguous launches, failed/active queries or
unsupported platforms cannot become zero. The private restricted live-close path
now combines that process proof with actual audited task joins, source-operation/
handle close joins and disposal of unclaimed root pipes. It issues at most one
nonclone graph-bound receipt AFTER dropping real native Child/Job owners, without
refunding by itself. Generic writer factory
handoff (including panic before task registration) and returned copied source
buffers permanently disqualify the graph: their downstream ownership is unaudited.
Hardware/tuner/scratch costs also refuse because no external adapter closes them.
Public close remains UnknownHeld. No restart proof or Windows durable admission.

`feb5c2fdf` adds exact live runtime-member composition: every manifest slot must
be an actual armed/invoked root or named discard task. The sealed original graph's
generation/counts must match, with no omitted source handle. Facts are captured
before native disposal; the real single-use receipt enters the existing retained
journal mutation synchronously. A rejected queue/unknown commit retains original
proof, while only a matching durable settlement permits the existing once-refund.
Seven Unix fixtures host-compile (10.075s); none ran. This does not supply a
positive Linux/restart backend or production finite path. The final C1 amendment
accepts a trusted fixed-software/known-helper normal graph, not arbitrary hostile
descendants. No custom broker/service/configuration or deployment is selected.

`b63cb217c` adds mandatory exact same-file v1→v2 migration at real storage startup.
The ready witness/location guard and database inode stay unchanged. Original v1
DDL/documents/history remain strict; recognized installation completes before an
upgrade transaction rebuilds only meta/members and adds retained asset ownership.
Historical rows/JSON/operations/u64 values and full totals are validated before
and after; unknown commit never permits empty authority or owner registration.
Typed source/copy/task/scratch and Closed-or-Transferred records feed the existing
journal reconstruction. Every active obligation is family-owned OR asset-owned,
disjoint, including full trial costs. Assets are cleanup-only and cannot spawn;
retained handles, buffers, readers and files cannot become free CPU-sized zeros.
Fourteen new bodies host-COMPILED NOT RUN;225.756s combined then56.750s only
source-invalidated core. Actual v2 acquisition/atomic R→S writer, concrete VOD
owners, positive restart transport and behavioral acceptance remain missing.

The controller's genuine SDK-bound core+daemon all-target Windows compile passes
on fixture repair2726df816 (96.023s); prior failures remain evidence. New live-close
source host-compiles (combined125.139s, changed core47.42s); full Windows check
passes135.763s on35f0d8715 with changed core73.591s on6dc165309. Native execution/
restart acceptance remains owed; every behavioral fixture remains NOT RUN.

Private cleanup consumes the original nonclone proof, retains it on queue refusal
or uncertain commit, and refunds only after exact durable settlement. A public
cleanup refusal returns that original proof. Reservation/graph Drop cannot refund
a durable tag; restart reconstruction installs tags too. Retained successor pins
fence owner retirement until users close or transfer. Corrected data-only facts
require definite rollback; unknown operations retain their exact payload/UUID.

## Current refusal and platform behavior

Released M4 Store authority is also integrated: exact request/Prepared/winner/
adoption transactions and migration/old-writer fences are coordination, NOT
physical proof. Its normal API cannot mark Closed or refund a local reservation;
fixture history erasure exists only under the contract-test compilation feature.
Worker RPC/admission/publication composition remains open, not advertised ready.

The production startup guard still refuses unreconciled custody before cleanup
or competing legacy native work. Selected Linux owners and supported recovery
paths do not establish complete selective reconciliation or unaffected serving.
A successful root wait or file close alone does not establish descendant
extinction. Keep the original unresolved owners and their full charge.

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

The released journal retains original authorship and earlier scoped compiler
receipts; its synthetic fixtures are protocol checks, not native physical proof.
Twelve new live-close bodies are Windows-compiled, nine also host-compiled.
All behavior remains NOT RUN.
The canonical ledger binds current source/compiler evidence and preserved failures.
Private APIs still produce warnings because production composition is missing;
there is no strict Clippy, independent full-candidate review, behavioral
qualification or native platform physical-close pass.

Synthetic private test receipts prove only protocol shape, not kernel or physical
closure. Remaining work includes existing-supervisor/production composition of
the retained bridge; positive normal/restart/fencing and typed-resource backends;
shared-role transfer, real production demands and all adapters; safe learning/pacing, M4
remote runtime and M5 peer/deadline/domain/drain policy; diagnostics; independent
complete-candidate Fable review/fixes and once-only affected tests/fast lane before
an authorized merge. R2/R3 targeted design decisions are not that final review.
