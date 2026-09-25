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
| Replicated admission and request recovery | Pending | Reconcile current Store and session contracts with reviewed design. |
| Worker placement and lifecycle | Pending | Remove permanent device ownership from ingest. |
| DVR claims, storage and finalization | Pending | Implement review F2/F3 with durable authority. |
| Guide, Activity and reminders | Pending | Remove owner routing dependencies. |
| Developer enablement and clients | Pending | Advisory readiness; no prerequisite-gated enable toggle. |
| One adversarial code review | Deferred | Only when the complete main-bound PR is ready. |
| Fast lane and merge | Deferred | After review fixes; no repeated unit-suite runs during implementation. |

## Delivery decisions

- 2026-09-25: User requests proper batched commits and one larger PR, with
  adversarial code review only at merge readiness, followed by the fast lane.
  The current development pipeline's opening correction already specifies
  that review/CI sequence. Clarification was requested for remaining older
  per-task test and full-promotion requirements; no tests have been run.
- 2026-09-25: Compiler, formatting and lint checks establish build correctness
  while test execution is deferred. The default Homebrew compiler is 1.98.0;
  commands explicitly use the installed 1.97.1 toolchain instead.
- 2026-09-25: The user's latest feature direction replaces the plan's temporary
  feature-mode gate. Developer settings will expose enablement with advisory
  prerequisite observations. Runtime operations still enforce authentication,
  committed claims and stale-worker publication safety.
- 2026-09-25: Credentials remain outside the clone. No token or deployment key
  is copied into source, commits, status, PR bodies or compiler archives.

## Commits and release evidence

No implementation commit or PR yet. No runtime tests, hardware acceptance,
code review, fast-lane result, merge or deployment is claimed.
