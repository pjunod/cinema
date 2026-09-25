# Live TV cluster resource — implementation status

**Status:** building · **Updated:** 2026-09-25 · **Branch:**
`codex/live-tv-cluster-resource` · **Base:** `8ae8cab1e`

The [implementation contract](LIVE-TV-CLUSTER-RESOURCE-IMPLEMENTATION.md)
and [accepted design review](LIVE-TV-CLUSTER-RESOURCE-REVIEW.md) define the
work. This page records completed work and remaining evidence. The active
checkout is a separate clone at `/private/tmp/plurx-live-tv-resource-01a0da60`.
The user's existing checkouts are not build or edit workspaces for this work.

## Current progress

| Milestone | State | Evidence / next action |
|---|---|---|
| Isolated workspace and pinned compiler | Complete | Fresh Forgejo clone; Rust 1.97.1; offline cargo check -p plurxd --all-targets passed in 95 s. |
| Replicated admission and request recovery | Building | Shared SQLite/Hiqlite transition engine compiles; global request identity and routing are being integrated. |
| Worker placement and lifecycle | Building | Candidate placement and shared-channel assignment replace configured-owner routing; lease fencing is being connected. |
| DVR claims, storage and finalization | Building | Atomic pre-I/O capture claims, storage identities and fenced publication are being integrated. |
| Guide, Activity and reminders | Building | Source-refresh lease, persistent guide copies and local worker Activity are integrated; validation pending. |
| Developer enablement and clients | Building | Advisory Developer control and protocol 4 intents wired on web, Apple and Android. iOS/tvOS and Android compilation passed. |
| One adversarial code review | Deferred | Only when the complete main-bound PR is ready. |
| Fast lane and merge | Deferred | After review fixes; no repeated unit-suite runs during implementation. |

## Delivery decisions

- 2026-09-25: User requests proper batched commits and one larger PR, with
  adversarial code review only at merge readiness, followed by the fast lane.
  The current development pipeline's opening correction already specifies
  that review/CI sequence. Clarification was requested for remaining older
  per-task test and full-promotion requirements. The user confirmed: one unit
  test run on the completed PR before merging. This overrides older per-task
  test requirements for this effort; no tests have been run.
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

Commits: `02b173d8f` (reviewed contract/status), `1f5ba9c81` (replicated
claims and worker integration). Follow-up lifecycle, client and guide changes
are being completed. Pinned Rust checks pass; the normal hook also passed
catalog lint, formatting, workspace Clippy and served JavaScript syntax.
Apple iOS/tvOS builds and Android application/test-source compilation pass.
No test methods were executed by these compiler checks.
No PR yet. No runtime tests, hardware acceptance,
code review, fast-lane result, merge or deployment is claimed.
