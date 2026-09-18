# Apple pause/resume — implementation and promotion status

**Status:** M1–M2 built and compiled; promotion packaging in progress · **Updated:** 2026-09-18 ·
**Branch:** `codex/apple-pause-resume` · **Base:** `6fb0901d3d18`

Executes the reviewed
[implementation handoff](APPLE-PAUSE-RESUME-IMPLEMENTATION-HANDOFF.md).
The retained [design review](APPLE-PAUSE-RESUME-HANDOFF-REVIEW.md) constrains
the build but does not replace the one adversarial review required for the
finished implementation. Work happens in an independent Forgejo clone under
`/private/tmp`; no user checkout is modified.

## Delivery ledger

| Milestone | State | Evidence / next action |
|---|---|---|
| M0 · current base and contract | Complete | Forgejo `main` is `6fb0901d3d18`; revised handoff and retained review imported. |
| M1 · ordered intent and buffered immediate play | Built; iOS/tvOS compile green | One playback-request setter serves UI and remote commands; ordered intent publication fences repair; seek/replacement suppresses predecessor playback; eligible runway uses immediate play once. |
| M2 · shared resume deadline and presentation proof | Built; iOS/tvOS compile green | One immutable attempt ID and 15-second budget survive successor attachment; fresh timestamps plus 250ms continuing motion prove video; all detectors share one repair admission. |
| Developer enablement advisory | Built; iOS/tvOS compile green | Developer settings exposes an operable enable switch and lists met/unmet requirements; readiness never rewrites or disables the switch. |
| Tests | Deliberately deferred | Run the single fast-lane window only after implementation review findings are addressed. |
| Implementation adversarial review | Queued | Review the finished draft PR once, immediately before final validation. |
| Fast lane | Queued | Mark the reviewed PR ready, fix failures, and rerun only when the candidate changes. |
| Merge and cleanup | Queued | Merge only the reviewed current head with green required checks; remove temporary credentials and clone afterward. |
| Physical Apple TV acceptance | Pending hardware | A merged simulator-tested build does not satisfy the matched fresh-open measurements in handoff §8. |

## Decisions made without waiting

- Preserve Pause and Resume as viewer actions that abort a staged successor.
  Changing staged-successor ownership is a separate lifecycle design, while
  the current action epoch is required to fence stale callbacks.
- Keep the repair client-only unless new evidence requires server work. The
  incident already establishes retained client media and server time hold.
- Scope the 15-second deadline to an established on-demand item and its one
  owned same-delivery repair. Cold startup, Live TV, and unrelated seek or
  preparation budgets remain unchanged.
- Add no code-level rollout gate. Developer settings may expose an enable
  control and readiness information, but readiness is advisory only.
- Follow the user's newer test instruction where it conflicts with handoff M3:
  no development test runs before the final adversarial review is addressed.

## Evidence ledger

| Evidence | Result |
|---|---|
| Independent clone | `/private/tmp/plurx-apple-pause-resume-20260918` from Forgejo; credential-free remote URL. |
| Reviewed source | `6fb0901d3d18c1b181f7299f4faddfb73994fd1a` (Apple build 167 in the reviewed contract). |
| Rust scope | None planned; no Rust file has changed. |
| Apple compile-only check | `make apple-build` passed for generic iOS and tvOS simulator targets after the final lifecycle edits; no test action ran. |
| Unit or UI tests | Not run during implementation by explicit instruction. |
| Physical hardware | Not yet available to this implementation session; acceptance remains open unless a device run is completed. |

## Completion rule

The implementation is complete only when the draft PR contains the bounded
resume owner, regression coverage, synchronized lifecycle documentation,
issue-keyed Apple build note, and advisory Developer setting; one adversarial
review has been addressed; the current candidate passes the fast lane; and
Forgejo records the merge. Physical latency acceptance remains separately
truthful if hardware cannot be exercised.
