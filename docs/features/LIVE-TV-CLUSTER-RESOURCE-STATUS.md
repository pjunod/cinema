# Live TV cluster resource — implementation status

**Status:** review addressed; final result tracked on PR #537 · **Updated:** 2026-09-27 · **Branch:**
`codex/live-tv-cluster-resource` · **Base:** current main integrated before promotion

The [implementation contract](LIVE-TV-CLUSTER-RESOURCE-IMPLEMENTATION.md)
and [accepted design review](LIVE-TV-CLUSTER-RESOURCE-REVIEW.md) define the
work. This page records completed work and remaining evidence. The active
checkout is a separate clone at `/private/tmp/plurx-live-tv-resource-01a0da60`.
The user's existing checkouts are not build or edit workspaces for this work.

## Current progress

| Milestone | State | Evidence / next action |
|---|---|---|
| Isolated workspace and pinned compiler | Complete | Fresh Forgejo clone; Rust 1.97.1; offline cargo check -p plurxd --all-targets passed in 95 s. |
| Replicated admission and request recovery | Implemented | Shared SQLite/Hiqlite transition engine, durable intent identity and cross-ingress routing. Reviewed; final CI result linked below. |
| Worker placement and lifecycle | Implemented | Candidate placement, shared-channel claims, boot/epoch/generation and monotonic lease fencing. |
| DVR claims, storage and finalization | Implemented | Pre-I/O claims, storage identities, independent finalization, immutable publication and storage-local deletion. |
| Guide, Activity and reminders | Implemented | Source-refresh lease, persistent guide copies and local worker Activity are integrated; validation pending. |
| Developer enablement and clients | Implemented | Advisory Developer control and protocol 5 intents wired on web, Apple and Android. iOS/tvOS and Android compilation passed. |
| One adversarial code review | Complete | Five findings corrected and verified in the same independent review pass; approved for final lane. |
| Fast lane and merge | Live result | [PR #537 checks and merge state](http://192.168.4.7:3000/noirr/plurx/pulls/537) are the authoritative result; one unit/fast-lane set, then failures only. |

## Current-main integration

Main `8227b9bc8` is integrated. Its background-work migrations precede this
feature at SQLite 83 / replicated 61. Its reserved-tuner warm-restart repair
is preserved. Main's temporary owner-relay placement is superseded by durable
resource admission; media-pool load ranking remains an advisory worker preference.
Protocol 5 and `v5_` tickets distinguish durable admission from main's protocol 4.
The advertised list is `[1, 2, 3, 5]`; old owner-relay nodes cannot receive new
resource claims. Apple build 193 and Android build 132 preserve main's client
fixes. The existing reviewer approved this integration after correcting a legacy
media-pool capability advertisement. Legacy relay capability is false; durable
resource capability is separate and defaults false for older peers. Pinned
compilation, web regressions, TypeScript, documentation and source inventories
pass. The required fast lane records the final result on PR #537. The replacement
Apple runner is available.

### Final integration evidence

Pinned Rust 1.97.1 workspace/all-target Clippy, formatting and served JavaScript
syntax passed after the merge. The focused integration checks passed: 19 schema
checks, 10 ledger/migration inventory checks and five Live TV checks (protocol
negotiation, incompatible-peer placement, removed relay routes, warm restart
seat retention and two-segment startup). The removed-route assertion now checks
the intentional HTML app-shell fallback and absence of admitted tuner work.
Web Live TV, 34 settings checks, API/document/mobile contracts and TypeScript
passed. The review is closed; PR #537 is ready for its blocking fast lane and
merge. This status update also triggers the ready PR's synchronize event on
Forgejo, whose title-based draft change did not enqueue a ready-for-review run.

The first complete run on the integration passed 1,365 core tests, 179 SQLite
contracts, 3,045 daemon tests and both serial VOD restart checks, plus every
platform gate. Promotion rejected the stale base after main accepted Apple-only
seek completion changes. Those changes are now integrated; Rust, web and Android
source trees are byte-identical to the passed candidate. Apple build 193 claims
the next available number. The PR records the final freshness qualification.

## Delivery decisions

- 2026-09-25: User requests proper batched commits and one larger PR, with
  adversarial code review only at merge readiness, followed by the fast lane.
  The current development pipeline's opening correction already specifies
  that review/CI sequence. Clarification was requested for remaining older
  per-task test and full-promotion requirements. The user confirmed: one unit
  test run on the completed PR before merging. This overrides older per-task
  test requirements for this effort; test execution was deferred until final review.
- 2026-09-25: Compiler, formatting and lint checks establish build correctness
  while test execution is deferred. The default Homebrew compiler is 1.98.0;
  commands explicitly use the installed 1.97.1 toolchain instead.
- 2026-09-25: The user's latest feature direction replaces the plan's temporary
  feature-mode gate. Developer settings will expose enablement with advisory
  prerequisite observations. Runtime operations still enforce authentication,
  committed claims and stale-worker publication safety.
- 2026-09-25: Credentials remain outside the clone. No token or deployment key
  is copied into source, commits, status, PR bodies or compiler archives.

- 2026-09-25: Current main already shares channel transports between viewers
  and recordings. Preserve this behavior: the cluster counts channel ingests,
  and viewer/recording consumers join the same durable assignment.
- 2026-09-25: The persistence implementation uses typed, indexed records and
  one revision CAS shared by both backends. Only changed records enter Raft;
  terminal request history is counted without downloading it on renewals.

## Commits and release evidence

The implementation and review corrections are committed on PR #537. Pinned
Rust 1.97.1 workspace/all-target compilation, Clippy, formatting and served
JavaScript syntax passed. The ready PR's preflight, operations/web contracts,
and Windows/Apple/Android compilation passed in run #3180.

The first Rust unit set ran after the final adversarial review was addressed.
It reported 1,281 core passes with seven failures, 145 SQLite contract passes
with one failure, and 2,911 daemon passes with fifteen failures. Corrections
cover schema and SQL inventories, downgrade fixtures, protocol 4 and advisory
settings expectations, durable-claim session fixtures, legacy duplicate
recovery, guide-lease error accounting and a deterministic slow-sink check.
Focused core and daemon reruns pass. Main `abb6fe647` is integrated; its
catalogue migration precedes the Live TV schema at SQLite 83 / replicated 61.
Android build 130 preserves main's tablet fullscreen fix. The merged schema
and ledger checks pass, as do web settings and the task/timer inventory.
The integration also fixes macOS-only warnings in main's new child-priority
helper so pinned Clippy remains usable on this host. The PR records subsequent validation
and the merge result without requiring a status-only source commit.

Physical HDHomeRun and actual predecessor-binary runtime qualification remain
unverified. No fleet deployment is claimed. The unchanged requirement is one
final unit/fast-lane set, followed by the reruns needed to correct failures;
no additional broad local unit suite is being run.
