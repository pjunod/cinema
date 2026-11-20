# Adaptive playback history — implementation through October 22

**Status:** open. Companion to [ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md](ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md).

## Implementation history through 2026-10-22

Earlier published checkpoint `4d094726a55f14f3f236085d348b2a079bb0321d`
passed native Linux core+daemon compilation in 218.172 seconds (log SHA-256
`02366bf282056ce847dcce09584fbb56954b8b1c9a1415f7d5a790084dea7c48`); its
415 authored anchors were not run. The parser/metadata/strict-import prototype
then passed host compilation in 87.870 seconds (log SHA-256
`ea8c7d9c7ce1e18f0e55679db4661e4f42f0c3357c3f529b92bc8b009635a745`,
`../evidence/direct-check-20261023-141059.json`); 423 local anchors at that point
were not run. These receipts predate the published source below.

Published checkpoint `dd0a5b4cb068abb051678186648d5968488c67fe` passed exact
native Linux core+daemon all-targets compilation with pinned Rust 1.97.1 (EXIT
0, 230.430 seconds; `../evidence/direct-linux-dd0a5b4cb068-RESULT.json`, log
SHA-256 `37b14a4bceb08c322eef719b155e5ebad8e3f6e5eafb4dc745f2154da6cf65ca`).
Its 415 authored regression anchors were not run. The published source has the
ordinary branch, same-held-source before/after-publication drift latch and
root-only cancellation. Current uncommitted work extends cancellation to the
original registered writer and the no-root/all-sources-registered/unstarted
bootstrap case, including once-only close of the same file on identity
rejection. Typed capacity routing, finite progress belief and advisory output
measurement are integrated. Earlier partial startup traversal, possible child,
restart/adoption/seeks and the full allocation profile remain open. Host
compilation is running and has not passed. All V1–V5 and R1/M0–M7 remain OPEN.
No behavioral tests, final Fable review or qualification have run.

The selected executable now uses a fixed eight-entry verification cache keyed by
all seven S1 identity fields and digest. Only a prior successful held-file native
ELF/hash scan can populate it; a cache hit rechecks the same held descriptor before
and after lookup. Miss/identity change/digest change never grants cached authority.
This avoids repeated image reads without treating pathname metadata as a native
launch proof. Private tests use isolated fixed caches, avoiding global eviction races.

The retained runtime now owns response reader cells and exact OpenReader/ReadRange/
CloseReader operations. The same held child opens and verifies the published file;
original latched jobs retain the File and one fixed buffer. Only known durable
read observation exposes an opaque chunk. Chunk drop returns that SAME buffer,
and waits for it release the bundle lock. Drop requests closure rather than
claiming it: normal completion, later serving and eviction use the same close
operation; only positive original close permits removal of the lease. HTTP
`ReadyFile` counted-range selection and native-error causality now use this
interface and pass host compilation. Active native reader owners and their exact
cursor/lease inventory now move with the same bootstrap owner through provisional
S and known R→S; the old output selection resolves to S after rebind. The runtime
also explicitly preserves its actual ledger durable tag: the initial prepare-
bundle ID is distinct from the journal obligation ID.

Retained cleanup vocabulary fences new readers, removes exact owned files, syncs
the child, unlinks the child relative to the held parent, syncs that parent, then
closes the exact folded descriptor-copy set. Failed/uncertain removals and closes
retain charge and cannot retry a numeric descriptor. The S cleanup state machine,
typed settlement and dormant eviction now use the original native jobs and
nonclone closure capability. Actual closed inventories are disposed before
refund; unresolved cleanup preserves the rendition registry and charge. Host
compilation passes; native compilation and behavior qualification remain open.
Typed S settlement reconstructs the latest closed fold in the journal transaction,
checks exact retained lineage and empty files/readers/handles/storage, and retains
the original physical receipt through known COMMIT. The selected Linux ordinary
software-transcode branch now connects counted HTTP serving, S cleanup and
native R→S transfer; other recipes remain on the legacy path. Full quotas/cold
memory policy, partial-start ownership, restart/cache adoption and broader
selection remain open. Source/reference JSON defaults preserve historical
encoding.

The active harness has connected the selected Linux ordinary software-transcode
path. Core exposes
[`finite_vod`](../../crates/plurx-core/src/adaptive_resources/runtime/finite_vod.rs):
`SoftwareVodOwner` reserves the whole role before source/output acquisition, and
nonserde `FiniteOutput` uses exact retained publication tickets. The shared native
publisher is in core and reused by the daemon. Its retained bootstrap owner binds
payload capacity, selected arm/observation IDs, original native job, same child
descriptor and durable observation before success; cancellation keeps the
operation in its runtime cell. Native publication is single use, including after
partial failure.

A private same-ledger overage API adds only newly observed debt and fences fresh
admission. A repeated or smaller observation cannot add duplicate charge or
refund it. This is live accounting, not durable overage/restart qualification;
the bounded audit projection still holds an over-limit result rather than
acknowledging a fabricated bounded success.

The prearmed source adapter retains the actual file and original native job,
checks scanner and `s1` identity using shared core code, and exports a nonclone
counted lease only after the exact acquisition observation. Existing
[`SourceFence`](../../crates/plurxd/src/fragment_index_cluster.rs) can hold that
lease without reopening the legacy pathname. The finite-output slice adds an
explicit `PublishMetadata` journal operation for opted-in `identity.json` and
`encoded-process` outputs; the software-transcode profile requires its marker,
while copy omits it. The same-child native sync cell retains the original job
through known Arm/Observe and bounds each batch to 16 files. The daemon
[`PendingWrites` finite adapter](../../crates/plurxd/src/renditiondir/finite_publication.rs)
connects metadata durability, and startup passes the already-open runtime Arc to
`VodServe::Shared`.

The VOD generation engine, identity check and conversion are core-owned; the daemon re-exports them while retaining its existing test bodies. The `finite_vod` command, pump and initial-unit reader are joined to the physical graph's concrete `SoftwarePump` task and guarded actual pipes. The pump reads the exact init boundary, then queues the encoded-process marker, identity, init and segments through selected publication tickets and syncs them with bounded progress and diagnostics. Known durable Observe ends pending-I/O payload accounting; actual payload is cleared and the same directory returned under the original bundle lock before another queue operation can enter. The whole role reservation remains held until retirement.

The source-bound AVC route now preflights and accounts the bounded Segmenter prefix, merge, finish split and published batch through `GenerationMemoryLimits`/`Generation`. Init parsing has an explicit four-copy bound; parser reads, fragment inventory allocations and plan-boundary vectors are fallible. A canonical grid command and producer clock alignment are compiled. These are bounded components, not an end-to-end quota: parser metadata, native process, launcher and runtime-journal memory remain unresolved. The retained task-start latch is released only after known typed task Observe and same-ledger entry recheck. The pump waits for publication/sync bundle ownership through the same locked-queue helper; public sync queue behavior remains fail-fast. Finite control retains stop intent across root registration and uses the exact original control without waiting on Child or holding the journal lock, but whole closure is not proven.

The core finite-request path requires an immutable `SoftwareAvcCommand` and `SoftwareAvcPumpPlan` in the same reserved bundle before I/O, verifies actual source identity/path/scanner inputs, prearms source cells without exporting a lease, and carries the same `File` through spawn. It enriches the source-copy observation only after actual root launch, then registers and observes the retained pump task once. A cancelled graph-lock wait preserves the root-entry latch. The SourceCell parent-close job records Linux `close(2)` success only on return 0; an error or unknown outcome is never retried on a potentially recycled descriptor, and cancellation retains the original job/latch. This is parent-file closure only: whole SourceMember/root closure and refund proof are not wired. Shared-fd helper extraction preserves daemon behavior.

The producer-join slice freezes graph entry while allowing the original writer to finish its admitted publication/sync work. The bundle lock is released before the same writer/root joins; terminal diagnostics stay in the original graph record and callers receive copy-only summaries. Stop intent retains an abort handle for the original task and the original root registration, including stop-before-registration. Existing source/root/publication/sync jobs are joined in place without launching or observing replacement work. After writer/root joins, the same ledger fences fresh entry and positively observed source parents close through their original close jobs. This is producer quiescence, not a physical-close receipt: inherited copies, Linux fixed-image/known-helper conclusion, unobserved partial starts and retained output/reader ownership remain unresolved; R is still charged.

Durable output Fence/Freeze, the V3 frozen inventory, proof-bearing journal R→S mutation/recovery and post-COMMIT same-ledger swap are connected. Adopted output is read-only and reader cost growth is checked against the same stored S resources. Native publication records one positive writable-file close before releasing its held directory or payload; close failure/unknown stays charged without retry. The selected Linux ordinary software-transcode branch connects these components to original fixed-process/source closure, counted HTTP serving, cleanup and a provisional same-native S owner. Full native/index/runtime memory accounting is still required; partial-start ownership, restart/cache adoption and broader recipe selection remain open.

The selected Linux ordinary software-transcode branch is connected; other
recipes remain on legacy admission. Same-process seeks reuse one rendition
owner and its full-plan producer; cold-cache reuse remains an R1 gap. Full
memory profile, partial-start handling and broader recipe selection remain
open.

All V1–V5 remain **OPEN**. Still missing: ordinary entry and complete producer
ownership/terminal composition; the first metadata-marker caller; complete
memory/index and durable-debt accounting; actual reader leases; retained-output
transfer, conserving R→S and exact cleanup/settlement/capacity reuse.

The latest published base is `dd0a5b4cb068abb051678186648d5968488c67fe`. Its
native Linux core+daemon all-target Rust 1.97.1 compile passed in 230.430 seconds.
The 415 authored regression anchors were not run. The published source has the
ordinary branch, same-held-source before/after-publication drift latch and
root-only cancellation. Current uncommitted work extends cancellation to the
original registered writer and the no-root/all-sources-registered/unstarted
bootstrap case, including once-only close of the same file on identity
rejection. Typed capacity routing, finite progress belief and advisory output
measurement are integrated. Earlier partial startup traversal, possible child,
restart/adoption/seeks and the full allocation profile remain open. Host
compilation is running and has not passed. All V1–V5 and R1/M0–M7 remain open;
no behavioral tests, strict Clippy, final independent review, qualification/fast
lane, merge or deployment are claimed.
