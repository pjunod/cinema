# Preparation buffer pressure — why a completed copy froze Apple TV

**Status:** built and reviewed; merge gate pending · **Written:**
2026-10-08 · **Incident binary:** `6e1089d3fb27bde5cdd92d297249024ae2a6e9dd` ·
**Source inspected:** `079960dae` · **Owner:** GPT-6.1 Sol implementation, following the approved review.

Companion to [Playback](../PLAYBACK.md),
[Operations](../OPERATIONS.md), and
[the retained-output design](HONEST-MASTER-PLAYLIST.md). This document is the
canonical incident, implementation, review-disposition and landing ledger.
It executes the user-requested root-cause repair and accurate warning text.
It does not authorize a new media format, background feature switch, cache
budget increase, deletion of source media, or unqualified production rollout.

## 1. Incident and evidence

The viewer was watching *The Way He Looks* (file 5721) on Apple TV through
`media1`. Playback stopped at 778,243 ms (12:58.243), and the app reported
“The server is short of space.” Both the host root and the container's data
filesystem still had about 36 GiB available. This was a scheduler budget
hold, not evidence of ENOSPC.

All times below are UTC on 2026-10-08 (subtract four hours for the viewer's
America/New_York time). Values came from preserved Docker logs, read-only
queries of the live replicated state machine, and the settings UI. The raw
log was retained privately in `/tmp/plurx-apple-tv-freeze-20261008.log` on the
investigation host; it must not be committed because it contains private
infrastructure and media details. Its absence on another machine does not
make this narrative a portable raw evidence bundle.

| Time | Observation | Meaning |
|---|---|---|
| 22:49:00.895 | Active rendition `e4bb0740…` attached for file 5721, entry 11 | The existing viewer used an ordinary playback rendition. |
| 22:52:24.126 | Job `bc105397-9b09-4dcd-a6a4-4e4e3fac505f`, `copy_output_prepare`, started on media1 for file 5721 | Plurx prepared a complete copy of the same movie in the background. |
| 22:59:10.773 | That job succeeded, publishing artifact `cbe7ddcc-0f58-4a77-8c2d-eea9c6fc0929`, `wire_bytes=25734178464` | The completed copy alone was 23.966 GiB, roughly three times the playback budget. |
| 22:59:15.116 | Active rendition reported `WorkingSetFull { free_bytes: 25838661091 }` | Capacity pressure became visible 4.343 seconds after background completion. `free_bytes` means bytes to free, not filesystem availability. |
| 23:00:58 | Client had 0.1 s loaded and an HTTP segment wait | Previously buffered picture had run out. |
| 23:01:10 | `NoRoom { wanted: 22661575865 }`; recovery returned `server_hold` | Movie-local eviction could not clear the shared pressure. |
| 23:01:18–56 | Reopen at 778,243 ms, another held rendition attachment, then CoreMedia `-12889` | Restarting the client attachment reused the blocked server state. |
| 23:06:31 | User backed out and tried again; entry 11 attached, still `NoRoom` | A fresh UI attempt was insufficient; the saved resume position also did not reflect 12:58 and remains a separate diagnostic boundary. |
| 23:06:39 | Dormant rendition `2ef1173d…` purged; wanted fell by exactly 704,689,967 bytes | An older completed preparation was still charged until dormant cleanup. |
| 23:09:41 | User explicitly approved a targeted service restart on media1 | Operational recovery only, no fix deployed. |
| After startup | `/readyz` 200, term 18230 and applied index advancing | Process recovery confirmed; physical Apple TV playback recovery not yet confirmed. |

The older preparation job `92391510-ad5b-426c-a39a-22b8f6d5893a` for file
6676 completed at 22:36:36.349, with exactly 704,689,967 wire bytes. Its start
was 22:36:29.256; the purge at 23:06:39 matches the 30-minute dormant timer.
That exact byte-sized pressure reduction independently links completed
preparation to the live working-set counter.

The configured `playback.vod_working_set_bytes` was 8,589,934,592 (8 GiB),
and `vod.output_preparation` was `copy_and_encoded`. The scheduler's release
threshold is half the budget: 4 GiB. Thus the first wanted value implies
30,133,628,387 bytes of effective working-set pressure, not a 25.8 GB disk
free-space reading. The incident job was copy preparation; encoded output
uses the same settlement lifecycle and must be covered by the fix, without
claiming that an encoded job triggered this incident.

### 1.1 Recurrence after the recovery restart

The same file stalled again at 1,548,022 ms. A manual v2 copy job completed
at `2026-10-08T23:31:18.781Z`, again with exactly 25,734,178,464 wire bytes.
`WorkingSetFull` followed at `23:32:42.094Z`, then `NoRoom`. This reproduces
the lifecycle failure after process recovery and proves that manual preparation
uses the affected ownership path as well as candidate preparation. The private
raw log is `/tmp/plurx-apple-tv-recurrence-20261008.log`; it is not committed.

The user authorized temporary operational recovery in the investigation chat.
The investigator saved `vod_output_preparation=off` around 23:40 UTC, and queued
copy cancellation was logged at `23:40:35.660Z`. That chat owns production
recovery. This implementation chat makes no production changes, and turning
preparation off is not evidence that the lifecycle repair is deployed.

## 2. Root cause — preparation loses its accounting category before cleanup

The following functions are unchanged between the incident binary and the
inspected checkout. Revalidate that statement against the implementation base.

1. `vod/serve/create.rs::prepare_complete_output` (the shared preparation path) reserves
   the complete output under the retained-output budget and resolves a
   preparation-specific rendition using an allowance nonce. It is allowed to
   build a whole movie rather than the viewer's short forward window.
2. `vod/generation.rs::materialize` charges each segment to `Shared::working_set`.
   `PendingFootprint::commit(true)` also adds it to `preparation_media`.
   `vod/driver.rs` schedules ordinary viewers against
   `working_set - preparation_media`. This intentionally isolates ongoing
   background full-title work from the smaller playback budget.
3. Publication deliberately skips `try_admit` while a preparation exists.
   `retained.rs::link_complete` hard-links completed segments into the retained
   artifact namespace; the original rendition entries and their charges remain.
4. `PreparedCopyOutput::drop`, after successful settlement/exposure, removes
   `copy_preparation` and calls `PreparationAllowance::release`.
   Release removes `preparation_media` and releases the preparation reservation,
   but does not remove or transfer `working_set` bytes. In one step the entire
   background movie becomes ordinary playback pressure. This is a category
   lifetime bug; the existence of actual files does not make that category
   transition correct.
5. No completion transition then calls `try_admit` for those originals, nor
   promptly retires them. The rendition has no reader, so its driver chooses
   `Idle`. Ordinary dormant purge waits 1,800 seconds.
6. Every viewer evaluates the node-wide pressure, but `MakeRoom` operates on
   only that viewer's rendition. Once its own evictable entries are gone,
   `NoRoom` holds the movie even though the bulk of the bytes belongs to a
   completed background rendition with no viewer.
7. Apple and web translate both hold reasons into a disk-shortage claim.
   This secondary wording bug conceals the ownership/lifecycle defect.

The separate larger movie copy and the live rendition happen to share a
source title, not a buffer identity. The failure does not require multiple
viewers or another application. A single viewer plus one successful Plurx
background copy is sufficient.

Existing `copy_preparation_full_footprint_cap_and_drop_preserve_ordinary_working_set`
tests the low-level subtraction exactly as written, but does not verify what
happens to another live viewer after complete-output settlement. Keep physical
byte accounting honest; do not “fix” that test by blindly subtracting every
preparation byte while files and asynchronous writers remain owned.

## 3. Required ownership contract

The repair must keep a complete background body out of the ordinary playback
budget until it is either removed or deliberately admitted into an appropriate
bounded owner. Successful retained publication must not cause a transient or
permanent spike in ordinary working-set pressure. The same rule applies to
failed publication, cancellation, lease loss, deadline, source replacement,
preemption and disabling preparation.

Use an immutable rendition accounting domain: ordinary playback versus
nonce-private preparation scratch. Choose it at construction, preserve it
through settlement and cleanup, and route every charge/free/adoption through
it. The mutable `copy_preparation` option is job liveness, not byte ownership.
Do not derive accounting ownership from whether that option is present.

Prefer immediate retirement of the preparation-specific source rendition,
with cancellation-independent settlement, while the retained artifact keeps
its hard links and budget. Reuse the existing exact-key gate, generation fence,
producer termination and purge machinery where compatible. Keep the accounting
exclusion and a bounded cleanup reservation alive through physical settlement.
A retained hard link is a surviving cache owner; unlinking a preparation's
original pathname must not invalidate it.

The implementation must explicitly establish these invariants:

- One exact rendition/epoch owns retirement. A stale completion cannot close,
  unlink or subtract a newer incarnation using the same key.
- Cancellation of the request or worker cannot abandon retirement halfway
  through. A detached settlement owner or the existing maintenance owner
  retains the complete cleanup obligation and retries failures.
- Stop/fence producers and settle in-flight publications before releasing
  their accounting. Do not free a reservation while a child can still write.
- Reader attachment and retained-artifact acquisition race safely with cleanup.
  Preserve existing readers, response leases, continuous-quality dependencies,
  original-source fences, and the retained artifact's exact identity.
- Each byte belongs to an explicit budget category until deletion or a proved
  ownership transfer. Failed unlink/termination is observable and bounded;
  never hide orphan bytes by saturating a counter to zero.
- In-flight `PendingFootprint` commits cannot arrive after their category owner
  vanished and silently become ordinary playback bytes. Cover commit versus
  release at a deterministic barrier.
- Successful cleanup wakes all affected rendition drivers promptly. It does
  not wait for the 30-minute dormant TTL or require a client reconnect.
- Ordinary viewer and completed-cache accounting remain correct; prepared
  output does not get double charged after hard-link assembly and handoff.
- No new feature switch: this is an internal correctness fix.

Audit all mutation sites: stale replacement/adoption in `shared.rs`, ordinary
admission, dormant purge, generation identity-drift cleanup and publication,
and driver eviction. A private source rendition must never be admitted into
ordinary cache by a later attach or last-fragment race. Its nonce prevents an
ordinary viewer from normally selecting it, but cleanup still proves exact
identity and preserves any already-issued response/retained leases.

A different design is acceptable only if it proves these invariants and the
incident regression. Record the exact representation and lock order before
coding; do not leave “cleanup later” as an unowned promise.

### 3.1 Capacity handoff and hard links

Introduce one preparation-storage owner shared by construction, publication,
settlement and cleanup. Its accounting domain never changes to ordinary
playback. Treat job liveness and storage liveness as separate states.

The current registry sets the preparation reservation to zero during retained
assembly, and exposure removes it. Therefore retaining the existing allowance
object alone is insufficient. Replace those operations with an explicit atomic
handoff under the registry accounting lock: media already charged to the
retained artifact is covered by that retained charge; source-only metadata and
unlinked/in-flight materialization retain a residual scratch reservation.
The cleanup owner pins the retained artifact and its registry charge until
original scratch unlink and producer settlement complete. Registry collection
must not uncharge that artifact while the pin exists, even if exposure fails
or its ordinary TTL expires. Trace failed assembly through its retired/orphan
path with the same ownership rule. Before a retained charge exists, the full
preparation reservation remains held. Capacity is not available for a new job
merely because the old job's SQL lease or worker admission ended.

Account hard-linked data once against retained capacity while both names exist;
account scratch-only bytes and metadata separately. Do not assume file lengths
are disjoint physical allocations, or use deletion of one hard link as proof
that the inode is free. The representation must make the accounting transfer
atomic with publication of its owner, and expose a testable conservation
snapshot. Keep admission conservative if exact unique accounting cannot be
proved; no overflow, underflow or capacity-reuse gap is acceptable.

### 3.2 Construction and restart ownership

Establish the immutable domain and capacity/cleanup owner before calling
`resolve_rendition`, not after awaiting it. Today the rendition is installed
before `CopyPreparation` and `PreparationRun` exist, so cancellation inside
resolution or fence snapshot bypasses their destructors. Pass an owning handle
through the detached construction owner into the rendition. Every early exit
must either transfer ownership to that exact installed incarnation or schedule
cleanup; no request-owned future holds the only cleanup obligation.

Persist a small versioned private-preparation ownership marker before the first
private owned-file write, including init, identity and playlist metadata as well
as media. Marker construction itself is bounded and owned by the constructor
cleanup record. It records enough namespace/nonce identity to prove
which private source directory can be retired after restart; it contains no
credentials and changes no public wire/media format. Startup reconciliation
runs before new preparation admission, reconstructs bounded cleanup ownership
for marked leftovers and separately discovers retained artifact ownership.
Unknown, malformed or mismatched markers fail closed and report blocked cleanup;
never infer private ownership from a hash-shaped directory name. Cover crashes
before marker publication, after first media, during linking, after exposure,
and after partial cleanup. Existing generic cache adoption cannot import marked
private bytes into the ordinary working set.

Legacy directories from the incident build have no such marker. Before new
preparation admission, inventory the existing managed rendition namespace using
bounded, resumable metadata traversal. Do not treat an unmarked hash-shaped
name as proof of private ownership, and do not delete it on that inference.
Charge conservatively observed legacy bytes to a distinct cold/unknown cache
reservation under retained capacity, without putting them in ordinary playback
pressure. Deduplicate only where inode/hard-link identity proves shared storage;
otherwise conservative over-accounting is safer than making capacity available.
Unknown or unreadable inventory keeps new preparation admission closed and is
reported; it does not impose a synthetic buffer hold on in-budget playback.
A verified ordinary recipe adoption may atomically transfer its exact bytes out
of the legacy reservation into the ordinary owner. A retained reconstruction
may similarly transfer only proved identity-matching bytes. Everything else
stays capacity-backed until existing safe cache cleanup or an explicit operator
cleanup resolves it. Do not create a migration that bypasses ownership to force
background admission green. Test both an old unmarked preparation leftover and
an ordinary unmarked cache entry, plus incomplete/unreadable inventories. State
remaining conservatively reserved legacy bytes in rollout evidence.

### 3.3 Cleanup retry is an owned state

Do not copy the current dormant-purge failure semantics: it ignores the
termination result and uncharges remaining manifest bytes even when unlink
fails. A preparation cleanup record must retain the exact key/incarnation,
producer epoch, capacity owner, retained pin when applicable, remaining byte
ledger, retry deadline and last bounded failure class. Register the record
before detaching the final request/worker owner. The existing maintenance loop
services it; request cancellation can only stop waiting for cleanup, not remove
that record. Serialize with exact-key creation; avoid holding the node-wide
registry mutex across filesystem, producer or store awaits.

Fence first, obtain verified producer termination (including in-flight writers),
then remove originals and reduce their owned charge only for proved removals.
On failure retain the record, accounting and capacity, retry with bounded
backoff (first retry within one maintenance interval, ceiling 60 seconds), and
refuse additional preparation if reserved capacity or the existing artifact
count ceiling would be exceeded. Include cleanup records in that ceiling;
otherwise repeated cancelled jobs bypass the nominal artifact bound.
Retire the record only after all obligations settle exactly once. Wake held
viewer drivers on ordinary capacity changes; private cleanup itself must not
have caused those viewers to enter a capacity hold.

Expose low-cardinality diagnostics for private scratch bytes, pending cleanup
count/bytes, oldest cleanup age and failure class through the existing metrics
or admin diagnostics surface. Successful job completion and pending cleanup
are distinct facts. Tests must prove that persistent cleanup failure bounds new
background work without blocking an otherwise in-budget viewer.

## 4. Rejected shortcuts and bounded scope

Raising the 8 GiB setting only postpones the next full-film failure. Subtracting
unconditionally on `Drop` loses physical ownership. Keeping the exemption
forever hides unbounded disk consumption. Calling `try_admit` alone can fail
its completed-cache budget and still strand the body; it also does not cover
cancelled or incomplete preparations. Deleting `.retained`, source movies or
runtime caches manually does not fix in-memory accounting and destroys useful
evidence. Replacing the warning alone does not fix playback.

A general cross-rendition pressure sweeper may be useful later, but is not
required to paper over this lifecycle bug. Implement one only if review proves
it necessary for this incident, with separate ownership and regression evidence.
Do not expand this repair into watch-progress persistence or all CoreMedia
recovery behavior without separately proving those causes.

## 5. Implementation work and ownership

This is one bounded corrective change, one draft PR to `main`, with sequential
steps under a single implementation owner. It is not an effort split across
independent task branches. If scope requires multiple task PRs, switch to an
`effort/preparation-buffer-pressure` integration branch and follow the effort
and promotion gates; do not silently merge partially integrated tasks to main.

| Step | Owned files/surfaces | Deliverable |
|---|---|---|
| B1 | `vod/copy_preparation.rs`, `vod/session.rs`, `vod/shared.rs`, preparation creation, generation, driver and retained lifecycle as required | Explicit category/cleanup owner, fenced settlement, prompt retirement, bounded retries and truthful counters. |
| B2 | `vod/tests/copy_preparation.rs`, `encoded_preparation.rs`, relevant lifecycle seams and regressions | Real preparation-to-publication regression with another live rendition, success and cancellation/fault coverage. |
| B3 | Apple `PlayerController.swift`, web `prepared-switch-measurement.js`, existing related test suites; Apple build metadata/notes | Replace the false disk-space claim with “The server’s playback buffer limit has been reached.” Keep recovery semantics unchanged. |
| B4 | This ledger, docs index, backlog, operations reference if behavior changes | Exact tests, review dispositions, PR/gate/landing evidence and remaining deployment/device acceptance. |

Current branch `codex/playback-buffer-warning` has only the two warning edits
plus these planning documents; it also has unrelated untracked `Claude outputs/`
which must not be staged, moved or deleted. No implementation is merged.

## 6. Regression and validation contract

Create a small deterministic reproduction rather than a 25 GB fixture: set the
ordinary working-set budget below a valid full preparation's measured bytes,
keep a second rendition live with an owed segment, complete/settle/expose the
preparation through production paths, and prove that the live request completes
without `working_set`/`no_room`. The test must fail on the incident lifecycle
and pass on the repair. Use a title larger than the playback budget even if the
ordinary default fixture is tiny; a small custom budget is sufficient.

| Case | Required assertion |
|---|---|
| Copy success | Retained artifact remains acquirable/readable; live request progresses; source temporary charge drains once. |
| Encoded success | The shared settlement path satisfies the same contract with an encoded fixture. |
| Failure/cancel/lease loss/deadline/preempt/off | Partial media has a bounded cleanup owner, never spills into viewer pressure, eventually drains. |
| Publication versus release race | Commit/release fencing conserves charges and prevents post-retirement writes. |
| Attach versus cleanup race | Existing viewer/response ownership survives; stale cleanup cannot touch a replacement. |
| Termination or unlink failure | Accounting stays charged to a bounded non-viewer owner; retry is observable and succeeds once fault clears. |
| Duplicate drop/cleanup and restart | No double subtraction, unowned media or stale directory adoption; startup safely reconciles retained links. |
| Ordinary playback pressure | Real ordinary over-budget behavior still holds/evicts correctly; the repair is not a bypass. |
| Warning | Apple and web name the playback buffer limit and never infer filesystem exhaustion from these two codes. |

Before editing Rust, run the pinned compiler and establish the repository loop.
This host has `rustup run 1.97.1 rustc` (verified); bare `/opt/homebrew/bin/rustc`
is 1.98.0 and must not be used as equivalent evidence. Set the toolchain path
or invoke rustup explicitly for check, Clippy, formatting and focused tests.
Use a source-only archive loop if native prerequisites prevent a valid build.

Run focused new regressions first, then the complete affected daemon unit suite
and relevant retained/preparation/lifecycle suites. User explicitly requested
fixing failed unit tests: investigate failures, fix attributable failures, and
resolve unrelated blockers rather than hiding/skipping tests. Record exact
commands and distinguish infrastructure failures from test failures. Replicated
core tests require `--features hiqlite-store` or `make unit-core`.

Before push: formatting, affected all-target compilation, denied-warning Clippy,
focused regressions, docs-index/infra checks, web syntax/control tests, Apple
iOS/tvOS compilation and relevant XCTest. Existing local wording-only evidence:
`node --test tests/playback/web-control.test.js` passed 39 tests;
tvOS `AppleClientTests/testEveryHoldReasonHasSomethingAViewerCanRead` passed.
Those results do not validate the future Rust repair or a moved branch base.

Claim the next Apple build using `make apple-build-bump`, with a real issue-keyed
release note and docs-index row. Commit normally through the tracked hook.
Behavior-changing commit/PR subjects start `fix(`. Put exact `Regression-Test:`
anchors, one per line, in the PR body and preserve them in the merge commit;
Forgejo API merging must pass `MergeMessageField`.

## 7. Review, delivery and acceptance ledger

| Stage | State/evidence |
|---|---|
| Incident diagnosis | Proven lifecycle defect, live job/hold timing and exact older-job byte reduction; retained raw logs private. |
| Independent evidence audit | Completed: confirms unchanged incident lifecycle, nonce-private ownership, admission shortcut failure, double-subtraction hazard, and release-versus-publication race. Incorporated into §3 and §6. |
| Adversarial plan review | [Review](../reviews/PREPARATION-BUFFER-PRESSURE-PLAN-REVIEW.md): F1–F3 resolved in §§3.1–3.3; final independent verdict approved to build. |
| GPT-6.1 Sol build chat | Claimed 2026-10-08 in the build chat; pinned Rust 1.97.1 all-target daemon baseline check passed against `079960dae`. User authorizes implementation, reviews, fixes, PR/gates and merge to main. |
| Implementation review | [Actual diff review](../reviews/PREPARATION-BUFFER-PRESSURE-IMPLEMENTATION-REVIEW.md): F1–F7 corrected; approved at `4b8ae6f02`, no remaining findings. |
| Unit/compile evidence | Pinned daemon all-target check passed during development. Original-tree settlement regression failed with exactly 1,595,580 bytes of spill and passed after repair. Web warning suite: 40 tests passed; iOS/tvOS builds and two tvOS warning XCTest passed. Docs/index/build/infra checks: 50 passed after correcting the existing Android README build claim from 150 to declared 152. Final focused lifecycle suite: 21 passed. Tracked hook: catalog, formatting, workspace all-target denied-warning Clippy and all 77 served JavaScript syntax checks passed. Native broad units reported font-environment failures and stalled executable fixtures; remaining broad units run after merge on the full workflow's pinned Linux/Rust 1.97.1/Jellyfin FFmpeg 8 surface. |
| Main gate and merge | [PR #947](http://forge.lan:3000/noirr/plurx/pulls/947) is ready. Run 4529 passed every component check but its final current-base fence failed when main advanced from `079960dae` to `1088d7529`. Integrating that base requires fresh exact-candidate proofs and a new gate. Never call red, missing or superseded checks green. |
| Deployment/device acceptance | No fix deployed. Recovery restart succeeded; actual movie playback and a background completion during playback remain to observe. |

If main moves, integrate it and rerun affected proofs on the exact intended
candidate. A single ordinary PR follows the current draft → adversarial review
→ ready → affected main gate convention. A multi-PR effort requires its manual
effort gates and final current-tree promotion receipt. Keep this ledger and the
backlog synchronized through build, qualification and merge; do not equate merge
with production deployment or physical acceptance.

### 7.1 Plan review dispositions

| Finding | Correction | Verification |
|---|---|---|
| F1 · Capacity disappears before originals are retired | §3.1 requires an atomic retained-charge/residual-scratch handoff and artifact pin through failed unlink/collection. | Reviewer confirmed resolved at design level; implementation must test publication and GC with held cleanup. |
| F2 · Cancellation before preparation installation and crash recovery | §3.2 moves storage ownership ahead of resolution and persists a versioned marker before private payload. | Reviewer confirmed resolved at design level; construction barriers and restart crash matrix required. |
| F3 · Existing purge drops unresolved termination/unlink obligations | §3.3 specifies a bounded retry record, verified process/writer settlement and exact successful-removal accounting. | Reviewer confirmed resolved at design level; fault-injected termination/unlink and cancellation tests required. |

### 7.2 Storage representation and lock order

The implementation uses an immutable private storage handle on `Rendition`,
separate from `copy_preparation` job liveness. Ordinary `working_set` contains
only ordinary media; private publication uses its storage footprint. A
construction guard owns the handle before recipe resolution; detached
construction binds its exact key and writes the versioned ownership marker
before any private payload. Releasing a job closes new writes and registers
maintenance cleanup rather than releasing storage capacity.

Registry handoff charges the retained body and reduces the private reservation
in the same accounting lock. An assembly reservation restores the original
capacity if no artifact was constructed; otherwise the private owner pins even
a failed partial artifact until source cleanup finishes. The residual capacity
covers scratch metadata and materialization outside the retained charge.

Cleanup takes the exact-key gate, then fences the exact incarnation, confirms
producer and writer settlement, and unlinks bounded batches. It retains its
record, capacity, pins and failure class on errors. No synchronous node-wide
mutex spans an await. The retained registry lock is acquired only for short
accounting transactions after private footprint operations have released their
lock. Startup inventory precedes background admission and conservatively
reserves unmarked managed cache bytes; verified ordinary adoption transfers
only its exact materialized media. Unknown ownership closes background
admission without imposing ordinary playback pressure.

### 7.3 Human validation-order update

The user explicitly changed the order during implementation: after syntax and
lint pass, merge and watch the remaining unit suite, then fix failures after
merge. The broad unit suite therefore does not delay the ordinary corrective
PR once its focused repair regressions, independent recheck and required
affected-surface merge gate pass. Any broad-suite failure remains work to fix;
it is not skipped or waived. This changes validation order, not production
rollout authorization. The investigator reports a second user-triggered service
restart at `2026-10-08T23:58:22Z`; read-only settings still showed preparation
off. Neither recovery restart deployed this repair.

### 7.4 Current-main integration

The qualified Batch 03 promotion advanced main to `1088d7529` during the
platform compile checks. Integration preserves its remote-control and Mac
video work, the new `planned_codec` fixture field, and both sets of client
regression anchors. Main already claims Apple build 218, so this repair
reclaims build 219 with its issue #944 note; existing build notes on main
keep their original claims. Android documentation now follows main's
declared build 154 and continues to distinguish merged repairs from pending
rollout and physical acceptance.

The same independent reviewer recalculated the ownership census against
the combined source. Every current-main count matched its source, and the
repair's deltas remained exactly the previously reviewed additions. Compiler,
focused regression, policy and Apple proofs must describe this combined tree
before it is pushed for the fresh gate.

The refreshed pinned Rust 1.97.1 workspace all-target check passed, and the
expanded focused run passed 25 tests: all 21 storage regressions, the tracing
target contract, the readiness snapshot contract, clean Plex census shutdown
and executable creation while sibling threads fork. The tracing calls use
the existing contract's canonical spacing. The readiness test asserts empty
owned storage and requires inventory status and evidence to agree in one
snapshot; asynchronous inventory completion is not claimed prematurely.

Python discovery covered 386 validation and 871 operation tests. The five
validation and eleven operation identities denied process/socket access in
the sandbox passed on permission retries; both failed receipts remain in the
session evidence. The checksum-provisioned K08 upstream oracle passed
separately. Web control/settings passed 41 tests, iOS/tvOS build 219 passed,
and both selected tvOS warning XCTest passed. The normal hook and fresh main
gate remain required; remaining broad Linux units are watched after merge.

The separately coordinated receipt/history repair then landed as
`1de457260`. Its four changed files contain validation metadata and controls;
the Rust, Cargo/toolchain and native client source diff from `1088d7529` is
empty. Integrating this metadata preserves the reviewed product source and
its applicable proofs. The exact merged candidate still receives the compiler
loop before push and the current-base gate before landing.
