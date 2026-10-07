# Shared libraries — build status and remaining acceptance

**Status:** main integration in progress · **Updated:** 2026-10-06 ·
**Owner:** Root coordinating GPT-6.1 Sol builders · **Promotion:** authorized.

**Batch:** [completion PR #827](http://192.168.4.7:3000/noirr/plurx/pulls/827).
**Promotion:** [draft PR #828](http://192.168.4.7:3000/noirr/plurx/pulls/828).

Companion to [the implementation contract](SHARED-LIBRARIES-IMPLEMENTATION.md)
(the authority, ownership and acceptance rules). This page records what is
integrated, what is being built and what is still unproved. A compiler pass
is not playback or topology evidence.

## Current work

Paul lifted the inherited promotion hold on 2026-10-06 and instructed the
team to continue through completion. PR #827 is merged into
`effort/shared-libraries` at `de2ece8e3`. The promotion integration merges
current main `cca4a09b9` in the owned checkout
`/private/tmp/plurx-shared-sol-promotion-integration`.

The merge exposed 59 conflicted paths. Sol builders are resolving separate
Core/schema, server playback, and native/web slices concurrently. Main and
the effort allocated different migrations to the same version numbers;
the composed sequence is SQLite 104/105 and replicated sharing 80, custody
81, activated Source 82. Frozen Source installation layout 71 remains a
separate identity, independent of migration order. Main's newer rolling playback
and native link receipts must also survive the integration.

Next: compile the combined candidate, run one independent adversarial review,
address its findings, then run the fast lane and retry only failed tests.
No promotion test has run yet. The previous compiler receipts below describe
the effort batch, not the newly merged candidate.

The isolated checkout is `/private/tmp/plurx-shared-sol/repo`, on
`codex/shared-libraries-completion`. Source-only archives compile on nuc4 in
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
| Main integration compilation | In progress | Merged static web checks and catalog lint pass; Android app/unit sources compile. Apple prepared playback now shares main's actual frame-cadence evidence; compilation in progress. Rust compilation follows the resolved shared tree; no tests run |
| Compiler and lint | Earlier effort batch compilation passed | Final combined hook passed at `0ba500735` (2m24s); Core contract-feature targets compiled in 45.89s without execution; Rust 1.97.1 check/Clippy. iOS/tvOS app and test targets, Android app/unit sources compile at `32a556f7b`; Final Windows MSVC workspace/all-target check passed on `0ba500735` in 2m09s on m6, Rust 1.97.1, 4 CPUs / 8 GiB; compiler warnings remain. No tests executed |

## Parallel builders and management audit

Paul requested parallel GPT-6.1 Sol builders on 2026-10-06. Root manages the
integration branch, reviews implementation and evidence, and keeps this page
current. The completed batch used isolated builder clones. Promotion conflict resolution
uses one owned integration clone with exclusive file ownership per builder.

| Builder | Owned work | State |
|---|---|---|
| Source lifecycle | Main schema/migration/store reconciliation and Source compatibility | Resolving composed migration versions and historical-layout refusal |
| Clients / integration | Main integration ownership, native/web, docs and combined compiler loop | Static web/catalog checks and Android source compilation pass; Apple/server integration continues |
| Receiver/cluster | Main media-session, HLS, transcode and VOD integration | Preserving main's rolling-generation ownership alongside Shared ingress custody |
| Root | Coordination, status, promotion PR and eventual review/validation | Draft #828 opened; main-fast-lane run 4233 skipped because the PR is draft |

Rust builders install source and compile inside one owned lock on nuc4. Remote checks
use one canonical source directory with checksum copies and current write
timestamps, retaining the warm dependency target. Separate source paths had
reused stale workspace artifacts; the two workspace packages were cleaned
once before this transition. Formatting checks the exact staged archive on
the pinned local compiler without waiting for the remote lock. Six obsolete
source extractions (about 636 MiB) were removed. Apple and Android compile independently. The old mba address
`192.168.5.115` timed out on 2026-10-06; this is an unavailable build surface,
not a client-code failure. Inventory identifies m6 as `192.168.4.14`.

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

## Remaining qualification

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
3. **Preserve the Tailscale acceptance contract.** The proposed nuc4/m6 pair
   has no Tailscale according to the handoff. A pinned-TLS namespace fixture
   does not establish Tailscale or two-home acceptance. Use separate instance
   ports and data directories for qualification; do not replace fleet services.
4. **Fix ownership at its cause.** No new playback watchdogs, polling where a
   signal exists, or tasks without a named owner and exit. Optional features
   use Settings → Developer with advisory readiness, never hidden gates.
5. **Record only observed evidence.** Earlier Claude receipts stay attributed
   to their source revision. This resumed session has not yet run playback,
   device or main-promotion qualification.

## Cleanup ledger

The Source and receiver builder clones were removed after their clean commits
and final corrections were verified in published `0ba500735`; their owned
patch/check scratch was removed too. Retain the clean Root checkout and shared
compiler cache through promotion. Final platform scratch is removed after
its receipt is recorded.
Claude's host workspaces listed in the handoff remain preserved until their
unmerged work and receipts have been incorporated. Do not remove files owned
by other sessions or Paul's transfer directory.
