# Retained exit — how S survives collector shutdown and process restart

**Status:** open · scoped shutdown qualification passed ·
2026-11-07. Companion to the [ordinary collector](ADAPTIVE-PLAYBACK-ORDINARY-COLLECTOR.md)
and [canonical ledger](ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md). This covers
shutdown-only local-handle parking. Full R/S ownership, inventory completeness,
complete peaks and real-trial acceptance remain open.

## Drain preserves the original cache owner

Shutdown seals fresh admission and enters the runtime's irreversible drain
before selecting retained work. Already-owned cleanup remains available through
the original gates. Actual ordinary sessions and rich application aliases drain
before the final global directory seal.

Terminal session tombstones still own real client allocation slots. Shutdown
waits for each original terminal cleanup to complete, removes only that same
session/cleanup identity, and destroys the session and empty registry backing
outside the locks before waiting for retained S allocations. Cleanup completion
is its existing notification contract; it does not establish destruction of a
Tokio task header. Original producer joins and the separate application and
allocation barriers remain required.

A dormant purge can move retained output into the ordinary closing map. Under
the same original gate, shutdown positively joins its original producer and
drains that envelope's rich rendition, output and producer before the core
application wait, then drops its map and local envelope references. Surviving
empty envelope/header aliases retain support until their actual final drop.
The path preserves the existing retained-cache and compatibility disposition,
leaves `known_closed` false and supplies no destructive native ticket,
settlement or S refund.

The snapshot visits both operations-book S and recovered S that has never
appeared in a serving map. It matches original obligation, operation, role,
restored reservation and native identity. Empty serving maps supply no absence
proof. In-flight original adoption remains owned while its existing receiver
resolves; shutdown observes that original result and authority, including the
acknowledged head and sequence. The one-shot proof already moved into the
original journal command is never required to reappear or submitted again.

The collector's paid fixed records retain each original selection before
serving-map removal. Fresh slot-bound S and recovered S use the same permanent
service futures. Cancellation leaves the same selection, original close job
and charge available for retry. A later capture failure preserves any already
captured or parked prefix.

## Native exit closes handles and preserves assets

The [retained-exit operation](../../crates/plurx-core/src/adaptive_resources/runtime/operations/finite_retained_exit.rs)
uses original descriptor ownership. Book-owned S closes through the same
bootstrap member and CleanupCell; unserved imported S uses its actual original
File pair. The final imported take/install and the irreversible drain share
the operations-book barrier.

Local CloseHandles acknowledgement requires the actual original positive
result, exact completed IDs and no remaining original handles or unknown
close facts. Root wait, failed joins, partial closure and identity mismatch
retain uncertainty. The exit performs no new adoption, durable settlement,
asset unlink, path reopen or S refund.

| Boundary | What successful completion establishes |
|---|---|
| Original driver join | The same original F/output has completed its required join |
| Application release | Actual output, body, metadata, client and reader owners are gone |
| Descriptor exit | Original selected local handles have positively closed |
| Parked record | The exact S exit identity remains retained through final drain |
| Collector Released | Original actor, pool, headers and gate aliases have closed and support refund committed |
| Actual process exit | A later runtime may use the existing recovery/import contract |

S remains the same durable binding, census owner and full reservation throughout.
Descriptor exit does not construct a native terminal receipt. The consuming
RetainedExitReady/Done facts are distinct from FiniteCloseCompleted.

## Reader object lifetime remains an application obligation

A FiniteReader now retains the same original application guard as its last
field. Registration precedes publication under the original role. Calling
`close()` completes its native boundary while a retained reader object still
holds application ownership; its selectors drop before that guard releases.
The additional pointer is included in the structural reader projection.
Native read chunks and response/client allocations retain separate obligations.

## Paid storage stays bounded across retries

The fixed pool has 256 records. New exit continuation state and its private
requested-buffer allowance are independently charged with the actual record
and service-frame layout. Source preflight counts original directory/ID copies
and overlapping transient set/map storage before allocating or cloning.
Borrowed fold-state inspection avoids a clone before that check. Imported S's
older native layout and persisted charge are not enlarged to fund new exit work.

Completed Parked records are unavailable for replacement through the final
shutdown. Capacity exhaustion, oversized metadata or uncertain adoption leaves
S and support held; a truncated capture cannot report completeness. Support
shutdown retries resume the same lifecycle lease after publication is removed,
including cancelled waiters and late gate aliases. Runtime admission and the
directory remain sealed after successful process-exit parking.

## Qualification separates passing cases from the restart failure

The earlier `2a63691563227276abb573582a14280c20be72d9` reader batch passed
ten cases and failed the new closed-reader fixture before its lifetime
assertion. Tree `c913f5dacdf39ba3cb0a53b7c59a975a992d440b` passed the
workspace/all-targets check and the original native-close negative case; its
reader fixture still failed, and its real process restart failed before final
publication removal. These failed runs supply no successful restart evidence.

The reader fixture now follows the existing successful order: create and close
the reader after the real R-to-S commit. The restart exposed terminal session
slots retained past end; the production fix above releases those actual owners.
Independent source review approved that production fix at
`d7eabbf903ef93263210385e2f4ed93d59819fff`. A genuine dormant-purge regression
now also exercises the retained closing-envelope branch.

At `481504801927b1e6ef0be6bc7f46be5ee521abb8`, the corrected reader and genuine
closing-envelope cases pass, and the workspace/all-targets check passes. The
first restart child exits successfully, but the second unserved-import child
fails, so the parent restart case still has no pass credit. Its exit checker
compared the original S mutation sequence with the later adoption sequence.
The source-reviewed correction validates the native tuple against its original
claim and the authority against the exact retained original acknowledgement.
It keeps the original owner, operation, binding and native-head checks and
submits no new adoption. At tree
`e771599e96027537131cba7465009bbbcd175c95`, the complete process-restart,
real closing-envelope and native acknowledgement regressions pass, with zero
failed or ignored cases. The workspace/all-targets check and strict Clippy
also pass under pinned Rust 1.97.1. Its two successful child phases remain part
of one restart case.

There are 14 distinct source-scoped passes: ten unchanged reader cases at
`2a636915`; the corrected reader-lifetime case at `48150480`; and the three
restart, closing and acknowledgement cases at `e771599e`. Reruns add no new
distinct cases. The earlier failed runs remain failures.

[Task PR #1419](http://forge.lan:3000/noirr/plurx/pulls/1419) merged into the
effort at `66461276151bc3b3eb5e614d6d006ff96a2d0da4` after all eight jobs of
gate 6591 passed on candidate `b57d255f`. Normal commit/push hooks and the
exact committed-source check passed. Candidate and landing trees are both
`559568495be86f9e3a4cf6a3d7daf6636c23b6c9`; all seven regression trailers
remain in the landing message. Full M0 and real-trial eligibility stay open.

The qualification includes reader lifetime and retirement cases; full-pool,
stale-binding and partial-prefix refusal/retry; rejection of partial or failed
original CloseHandles results; and the real process-restart scenario. The
restart child initializes the actual collector, holds and releases original
reader/output/gate aliases around cancelled shutdown, preserves the same S
charge and files/inodes, and exits. Another child exercises unserved imported
S before normal serving adoption. The parent must import without a new encoder.
Exact commands, counts and source scope belong in the task PR and receipts.
