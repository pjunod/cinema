# Shared libraries — build status and remaining acceptance

**Status:** building on the effort branch · **Updated:** 2026-10-06 ·
**Owner:** Sol, continuing Claude's Root lane · **Promotion:** held for Paul.

**Batch:** [draft PR #827](http://192.168.4.7:3000/noirr/plurx/pulls/827).

Companion to [the implementation contract](SHARED-LIBRARIES-IMPLEMENTATION.md)
(the authority, ownership and acceptance rules). This page records what is
integrated, what is being built and what is still unproved. A compiler pass
is not playback or topology evidence.

## Current work

The isolated checkout is `/private/tmp/plurx-shared-sol/repo`, on
`codex/shared-libraries-completion`. Source-only archives compile on nuc4 in
`~/work/codex-shared-sol/compiler-source` using the verified Rust 1.97.1 compiler and the
existing warm target. Paul's checkout is not used for changes.

| Work | State | Evidence or next action |
|---|---|---|
| B relay, Source playback, ownership repair | Merged into effort | PR #794; earlier receipts in the contract |
| Crash recovery, assets and direct play | Merged into effort | PR #804 |
| Directed reopen and native controls | Merged into effort | PR #809 |
| Prepared successor, catalogue and protocol parity | PR #816 open | Head `551f18c8a`; run 4159 passed Rust, Android, web and preflight; Windows failed, Apple cancelled |
| Web prepared successor | Integrated into the new batch | Claude's `5fe0b1782` series, retained as proper commits |
| Purpose-key census, TLS permissions, deterministic receiver fixtures | Integrated into the new batch | Three completed commits from the unitfix branch |
| Partial fd/artwork fixture repair | Integrated in `d4d2ec8e4` | Exact file-identity assertions and deterministic artwork timing; compiled/linted, execution deferred |
| Burned subtitles and HDR | Implemented in `d4d2ec8e4`; runtime unqualified | Source-owned extraction/fonts; precise PQ/HLG claims; typed DV refusal; native parity integrated |
| Native/web handoff and burn/HDR | Integrated through `32a556f7b` | Native plan parity, web refusal reopen under original account, Android renderer failure cleanup |
| Native catalogue/history audit | Integrated through `32a556f7b` | Frame evidence before progress; Source-separated Continue Watching UI; next-episode membership race fixed |
| Android frame identity follow-up | Integrated `a062ce299` | Prepared successor carries its proved frame; late old attachment callbacks cannot mark the new attachment; Android sources compiled |
| Failed Source Start | Integrated through `e0fd7b898` | Fresh invocation claim precedes fallible preparation; private pre-admission factory receipt and exact g0/g1 cleanup; ambiguous failures retain custody; execution deferred |
| Busy predecessor transport | Integrated `9789741b2` | Graceful H2 drain preserves existing successor writes; actual closure still required; no runtime receipt yet |
| Approved endpoint cleanup | Integrated through `f7962bd2d` | Exact immutable End retries approved endpoints/pins after refusal or stall, preserving time for replacements; real pinned fixtures defined, execution deferred |
| Linux Docker bridge hosting | Profile and recipe integrated through `6ab554659` | Rust check/Clippy and actual Compose render/preflight passed, including explicit default gateway; runtime qualification remains open |
| Common ingress custody | Primitives integrated `cebc6d2e5`; adapters still building | Actual accepted-driver identity/closure, bounded per-principal registration and immutable retry identity; no forwarded playback admitted by this checkpoint |
| Custody schema and upgrade floor | Integrated `69876dfce` | SQLite 93; replicated baseline 72 / Source 73; frozen Source layout 71 unchanged; old held obligations refuse upgrade; compiled, execution deferred |
| Active upgrade harness | Integrated through `cdf18d7f5`; unexecuted | Historical/candidate/restored Local HLS, actual video decode and live encoder drain; does not qualify Shared relay or active principal rebuild |
| Compiler and lint | Per-checkpoint passes; integrated checks continue | Normal hooks passed on integration `1e3496056`; Rust 1.97.1 check/Clippy. iOS/tvOS app and test targets, Android app/unit sources compile at `32a556f7b`; Windows MSVC all-target check passed at that agent revision. No tests executed |

## Parallel builders and management audit

Paul requested parallel GPT-6.1 Sol builders on 2026-10-06. Root manages the
integration branch, reviews implementation and evidence, and keeps this page
current. Each builder has an isolated clone from `d4d2ec8e4`.

| Builder | Owned work | State |
|---|---|---|
| Source lifecycle | Fresh Start invocation custody, Source ledger and coordinated schema/floor migration | Migration and fresh invocation checkpoints integrated; Source custody adapter and fresh worker placement next |
| Source forwarding / completed clients | Dedicated Source owner transport; native/web, hosting and Windows compiler | Client/Docker/harness checkpoints complete; reassigned to Source forwarding in parallel |
| Receiver/cluster | Shared accepted-driver custody, B owner forwarding and endpoint cleanup | Common driver/registration checkpoint integrated; endpoint fallback integrated; building B custody/forwarding |
| Root | Integration, API/operator/security docs, central ownership census, status and review | Auditing completed behavior against the contract; preserving unproved acceptance cells |

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
the explicit profile, recipe and gateway selection are now integrated. No claim that the whole effort is
correct or releasable is made.

## Remaining build and qualification

| Requirement | Next acceptance boundary |
|---|---|
| Failed or ambiguous Source Start | Runtime qualification of fresh invocation custody, private factory refusal and exact g0/g1 cleanup |
| Source worker forwarding | Authenticated non-owner ingress reaches its assigned physical worker |
| B cluster ingress and owner transition | Physical ownership proof; SQL metadata alone cannot authorize adoption |
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
   and rerun only failures.
2. **Keep the effort isolated.** Batch commits into effort PRs. Main promotion
   remains held by the handoff's explicit instruction until Paul lifts it.
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

Retain active source, compiler cache and evidence until the batch is safely
published. Remove session scripts and source extractions at completion.
Claude's host workspaces listed in the handoff remain preserved until their
unmerged work and receipts have been incorporated. Do not remove files owned
by other sessions or Paul's transfer directory.
