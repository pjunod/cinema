# Shared libraries — build status and remaining acceptance

**Status:** implementation integrated; promotion qualification tracked below ·
**Updated:** 2026-10-06 · **Owner:** Root coordinating GPT-6.1 Sol builders.

**Batch:** [completion PR #827](http://forge.lan:3000/noirr/plurx/pulls/827).
**Live promotion status and final receipts:** [PR #828](http://forge.lan:3000/noirr/plurx/pulls/828).
**Current integration batch:** [PR #829](http://forge.lan:3000/noirr/plurx/pulls/829).

Companion to [the implementation contract](SHARED-LIBRARIES-IMPLEMENTATION.md)
(the authority, ownership and acceptance rules). This page records what is
integrated, what is being built and what is still unproved. A compiler pass
is not playback or topology evidence.

## Current work

Paul lifted the inherited promotion hold on 2026-10-06. PR #827 landed at
`de2ece8e3`; integration commit `4d0aa44b3` merges current main `cca4a09b9`
into that effort in `/private/tmp/plurx-shared-sol-promotion-integration`.
The 59 conflicted paths have been reconciled. The composed sequence is
SQLite 104/105 and replicated sharing 80, custody 81, activated Source 82.
Frozen Source installation layout 71 remains independent of migration order.
Main's rolling playback, quality controls and native link receipts survive.

The integrated Rust workspace and all test targets pass Rust 1.97.1 Clippy
with denied warnings; replicated contract-feature targets compile separately.
iOS/tvOS app and test targets, Android app/unit sources and Windows MSVC
workspace/all-target checks compile. No tests were executed for that checkpoint.

The one independent adversarial review of `4d0aa44b3` found an unbounded
initial Apple Shared successor seek. Its correction reuses the existing
bounded seek owner, capped by the original preparation budget, and invalidates
late completion before cancellation. Review verification and the subsequent
fast-lane results are recorded in PR #828. That PR is the live status page:
receipt updates there do not change the source tree being qualified. The
compiler evidence below remains attributed to its actual input revision.

The isolated checkout is `/private/tmp/plurx-shared-sol/repo`, on
`codex/shared-libraries-completion`. Source-only archives compile on lab4 in
`~/work/codex-shared-sol/compiler-source` using the verified Rust 1.97.1 compiler and the
existing warm target. Paul's checkout is not used for changes.

| Work | State | Evidence or next action |
|---|---|---|
| B relay, Source playback, ownership repair | Merged into effort | PR #794; earlier receipts in the contract |
| Crash recovery, assets and direct play | Merged into effort | PR #804 |
| Directed reopen and native controls | Merged into effort | PR #809 |
| Prepared successor, catalogue and protocol parity | Included in completion PR #827 | Supersedes PR #816; original `551f18c8a` receipts remain historical |
| Web prepared successor | Integrated into the new batch | Claude's `5fe0b1782` series, retained as proper commits |
| Purpose-key census, TLS permissions, deterministic receiver fixtures | Integrated into the new batch | Three completed commits from the unitfix branch |
| Partial fd/artwork fixture repair | Integrated in `d4d2ec8e4` | Exact file-identity assertions and deterministic artwork timing; compiled/linted, execution deferred |
| Burned subtitles and HDR | Implemented in `d4d2ec8e4`; runtime unqualified | Source-owned extraction/fonts; precise PQ/HLG claims; typed DV refusal; native parity integrated |
| Native/web handoff and burn/HDR | Integrated through `32a556f7b` | Native plan parity, web refusal reopen under original account, Android renderer failure cleanup |
| Native catalogue/history audit | Integrated through `32a556f7b` | Frame evidence before progress; Source-separated Continue Watching UI; next-episode membership race fixed |
| Android frame identity follow-up | Integrated `a062ce299` | Prepared successor carries its proved frame; late old attachment callbacks cannot mark the new attachment; Android sources compiled |
| Failed Source Start | Integrated through `8b885f707` | Fresh invocation precedes preparation; exact retained intent can fence only its original undispatched g0 after an ambiguous commit; absent, foreign or g1 claims remain unresolved; execution deferred |
| Busy predecessor transport | Integrated `9789741b2` | Graceful H2 drain preserves existing successor writes; actual closure still required; no runtime receipt yet |
| Approved endpoint cleanup | Integrated through `f7962bd2d` | Exact immutable End retries approved endpoints/pins after refusal or stall, preserving time for replacements; real pinned fixtures defined, execution deferred |
| Linux Docker bridge hosting | Profile and recipe integrated through `6ab554659` | Rust check/Clippy and actual Compose render/preflight passed, including explicit default gateway; runtime qualification remains open |
| Common ingress custody | Source `d32be9ca8` and B `2e4fd9e21` integrated | Actual accepted-driver identity/closure, bounded per-principal registration and immutable retry identity; Both forwarding paths wired; concurrent actual closure under one deadline, serialized acknowledgments and exact lost-reply retries |
| Receiver ingress authority | Runtime and fixture integrated through `696ffb9ec` | Fresh receiver read proof, independent ingress member floor and explicit backend identity; guarded registration/cleanup metadata; no physical closure inferred from Store reads |
| Custody schema and upgrade floor | Integrated `69876dfce` | SQLite 93; replicated baseline 72 / Source 73; frozen Source layout 71 unchanged; old held obligations refuse upgrade; compiled, execution deferred |
| Active upgrade harness | Integrated through `cdf18d7f5`; unexecuted | Historical/candidate/restored Local HLS, actual video decode and live encoder drain; does not qualify Shared relay or active principal rebuild |
| Main integration compilation | Passed at `4d0aa44b3` / tree `993422d0` | Rust workspace/all-target Clippy 1m59s; Core contract-feature compile 1m02s; Windows all-target check 5m26s; iOS/tvOS and Android app/test-source compilation; static web/catalog/history/mobile policy. Normal hook passed. No test execution |
| Independent adversarial review | One P1 found at `4d0aa44b3` | Initial Apple Shared seek needs the existing bounded owner; correction and verification receipts tracked in PR #828 before test execution |
| Compiler and lint | Earlier effort batch compilation passed | Final combined hook passed at `0ba500735` (2m24s); Core contract-feature targets compiled in 45.89s without execution; Rust 1.97.1 check/Clippy. iOS/tvOS app and test targets, Android app/unit sources compile at `32a556f7b`; Final Windows MSVC workspace/all-target check passed on `0ba500735` in 2m09s on lab6, Rust 1.97.1, 4 CPUs / 8 GiB; compiler warnings remain. No tests executed |

## Parallel builders and management audit

Paul requested parallel GPT-6.1 Sol builders on 2026-10-06. Root manages the
integration branch, reviews implementation and evidence, and keeps this page
current. The completed batch used isolated builder clones. Promotion conflict resolution
uses one owned integration clone with exclusive file ownership per builder.

| Builder | Owned work | State |
|---|---|---|
| Source lifecycle | Main schema/migration/store reconciliation and Source compatibility | Integrated and compiled in `4d0aa44b3`; private-lineage refusal and Local recovery principal definitions retained |
| Clients / integration | Main integration ownership, native/web, docs and combined compiler loop | Integration compilation complete; fixing the review's bounded initial Apple seek |
| Receiver/cluster | Main media-session, HLS, transcode and VOD integration | Integrated and compiled; Windows check passed on the exact integration tree |
| Root | Coordination, status, promotion PR and review/validation | PR #829 holds the integration; PR #828 tracks review resolution and final qualification. Initial draft run 4233 skipped; no tests at `4d0aa44b3` |

Rust builders install source and compile inside one owned lock on lab4. Remote checks
use one canonical source directory with checksum copies and current write
timestamps, retaining the warm dependency target. Separate source paths had
reused stale workspace artifacts; the two workspace packages were cleaned
once before this transition. Formatting checks the exact staged archive on
the pinned local compiler without waiting for the remote lock. Six obsolete
source extractions (about 636 MiB) were removed. Apple and Android compile independently. The old maca address
`10.42.5.115` timed out on 2026-10-06; this is an unavailable build surface,
not a client-code failure. Inventory identifies lab6 as `10.42.4.14`.

**Audit findings being addressed:** failed Source Start cleanup cannot be
inferred from task errors; B must forward a durable remote session to its
actual live owner; a predecessor on shared H2 must not hard-cut a successor
merely to prove retirement; clients and B must agree on explicit burn
selection. The native audit additionally found missing Continue Watching and
history updates before frame evidence; those fixes are integrated. Cluster
forwarding cannot report End until the outer ingress writer, as well as the
owner's internal writer, has actually closed. The builders share one custody
mechanism for that boundary. Docker's planned bridge recipe was not implemented;
the explicit profile, recipe and gateway selection are now integrated. The runtime fixes cover reconnect gaps, independent admission proof for
each owner of a shared rendition, and explicit closure signals rather than new
per-connection SQL polling. Cold-file placement uses a signed read-only file
observation, independent of the legacy MPEG-TS offer and warm index state.
Returned physical closure receipts are authenticated as exact member responses.
No claim that the whole effort is correct or
releasable is made.

## Qualification tracker

These are acceptance boundaries, not compiler claims. The live outcome and
source-bound runtime receipts are maintained in PR #828; this table records
what each cell must establish.

| Requirement | Next acceptance boundary |
|---|---|
| Failed or ambiguous Source Start | Runtime qualification of fresh invocation custody, private factory refusal and exact g0/g1 cleanup |
| Source worker forwarding | Implemented and fixture defined; runtime and remote-only mount qualification deferred |
| B cluster ingress and owner transition | Implemented and fixture defined; actual-owner, member-loss and full topology execution deferred |
| Endpoint and pin changes | Authenticated rotation preserves cleanup reachability |
| Cluster revocation and limits | Three-voter cases; preparations count toward four grant and eight Source slots |
| Upgrade and restore | Historical binaries and active-session behavior; restore requires disable or re-pair |
| Prepared handoff client gaps | Integrated refusal recovery awaits runtime/device qualification; physical Android TV, Apple direct play and handoff |
| Busy predecessor transport | Integrated graceful drain awaits actual H2 regression execution |
| Docker bridge hosting | Running-host isolation/private-egress qualification of the explicit bridge and default gateway |
| Operator, API, security and Developer lifecycle | [Operator guide](SHARED-LIBRARIES-OPERATIONS.md) and security boundary updated; retain advisory readiness until qualification |
| Live matrix | Tailscale, two NATs, relay, devices, cluster loss, revoke bounds and sustained playback remain unproved |

## Decisions and constraints

1. **Defer test execution until main promotion.** Paul explicitly confirmed
   this on 2026-10-06, overriding the per-task test requirements. Compile and
   lint during development. Retain existing passing evidence; after the main
   adversarial review, execute the required tests once on the merging code
   and rerun only failures. The optional effort CI workflow also executes
   contract tests despite its compile-only description, so its dispatch is
   deferred; compilation and lint run separately. No green CI gate is inferred
   from those local compiler receipts.
2. **Keep the effort isolated.** Batch commits into effort PRs. Paul lifted
   the handoff hold on 2026-10-06; main promotion is now authorized after
   review, fixes and passing fast-lane evidence.
3. **Preserve the Tailscale acceptance contract.** The proposed lab4/lab6 pair
   has no Tailscale according to the handoff. A pinned-TLS namespace fixture
   does not establish Tailscale or two-home acceptance. Use separate instance
   ports and data directories for qualification; do not replace fleet services.
4. **Fix ownership at its cause.** No new playback watchdogs, polling where a
   signal exists, or tasks without a named owner and exit. Optional features
   use Settings → Developer with advisory readiness, never hidden gates.
5. **Record only observed evidence.** Earlier Claude receipts stay attributed
   to their source revision. The final source, review resolution, test outcomes
   and any unqualified live cells are recorded in PR #828.

## Cleanup ledger

The Source and receiver builder clones were removed after their clean commits
and final corrections were verified in published `0ba500735`; their owned
patch/check scratch was removed too. Retain the clean Root checkout and shared
compiler cache through promotion. Final platform scratch is removed after
its receipt is recorded.
Claude's host workspaces listed in the handoff remain preserved until their
unmerged work and receipts have been incorporated. Do not remove files owned
by other sessions or Paul's transfer directory.
