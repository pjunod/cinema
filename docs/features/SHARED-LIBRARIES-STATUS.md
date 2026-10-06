# Shared libraries — build status and remaining acceptance

**Status:** building on the effort branch · **Updated:** 2026-10-06 ·
**Owner:** Sol, continuing Claude's Root lane · **Promotion:** held for Paul.

Companion to [the implementation contract](SHARED-LIBRARIES-IMPLEMENTATION.md)
(the authority, ownership and acceptance rules). This page records what is
integrated, what is being built and what is still unproved. A compiler pass
is not playback or topology evidence.

## Current work

The isolated checkout is `/private/tmp/plurx-shared-sol/repo`, on
`codex/shared-libraries-completion`. Source-only archives compile on nuc4 in
`~/work/codex-shared-sol/src` using the verified Rust 1.97.1 compiler and the
existing warm target. Paul's checkout is not used for changes.

| Work | State | Evidence or next action |
|---|---|---|
| B relay, Source playback, ownership repair | Merged into effort | PR #794; earlier receipts in the contract |
| Crash recovery, assets and direct play | Merged into effort | PR #804 |
| Directed reopen and native controls | Merged into effort | PR #809 |
| Prepared successor, catalogue and protocol parity | PR #816 open | Head `551f18c8a`; run 4159 passed Rust, Android, web and preflight; Windows failed, Apple cancelled |
| Web prepared successor | Integrated into the new batch | Claude's `5fe0b1782` series, retained as proper commits |
| Purpose-key census, TLS permissions, deterministic receiver fixtures | Integrated into the new batch | Three completed commits from the unitfix branch |
| Partial fd/artwork fixture repair | Awaiting completion | Review the retained WIP; no WIP commit will be landed |
| Burned subtitles and HDR | Next implementation | Review Source-owned artifact and child lifetimes before adopting the partial branch |
| Local compiler loop | Baseline passed | Rust 1.97.1: `cargo check -p plurxd -p plurx-core --all-targets --locked`, 2m 06s; burn/HDR draft compiling; no tests executed |

## Remaining build and qualification

| Requirement | Next acceptance boundary |
|---|---|
| Failed or ambiguous Source Start | Private per-invocation negative-admission proof and g0/g1 cleanup |
| Source worker forwarding | Authenticated non-owner ingress reaches its assigned physical worker |
| B cluster ingress and owner transition | Physical ownership proof; SQL metadata alone cannot authorize adoption |
| Endpoint and pin changes | Authenticated rotation preserves cleanup reachability |
| Cluster revocation and limits | Three-voter cases; preparations count toward four grant and eight Source slots |
| Upgrade and restore | Historical binaries and active-session behavior; restore requires disable or re-pair |
| Prepared handoff client gaps | Refused commit after switch; physical Android TV, Apple direct play and handoff |
| Busy predecessor transport | Retirement must respect other sessions sharing the H2 connection |
| Operator, API, security and Developer lifecycle | Describe implemented behavior and retain advisory readiness until qualification |
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
