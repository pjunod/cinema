# Apple pause/resume — implementation and promotion status

**Status:** promotion ready under owner-approved Windows waiver · **Updated:** 2026-09-18 ·
**Branch:** `codex/apple-pause-resume` · **Base:** `6fb0901d3d18` ·
**Issue:** [#359](http://192.168.4.7:3000/noirr/plurx/issues/359) ·
**PR:** [#360](http://192.168.4.7:3000/noirr/plurx/pulls/360)

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
| Apple release metadata | Claimed | Issue #359 owns Apple build 168 and its issue-keyed release fragment. |
| Tests | Complete | Focused Apple test builds and the selected resume/control regressions pass 16/16 on iOS and 16/16 on tvOS; the first iOS run's incorrect watchdog-rate expectation was corrected without production changes. |
| Implementation adversarial review | Complete; five findings addressed | Final Pause/background/deadline fence stale repair attachments; shared detectors bypass holds after admission; item failures keep transport/HDR/compatibility classification; publication tasks are cancelled; controller coverage now drives the established owner. |
| Fast lane | Accepted with explicit Windows waiver | On corrected head `056e1aa4`, validation scope, mobile release version, policy/contracts, Rust, and Apple passed. Windows stopped before compilation because runner cache pruning could not restore its configured bound; the owner explicitly directed that result and the dependent aggregate gate be ignored for this PR. |
| Merge and cleanup | Authorized | Merge the reviewed implementation plus this waiver record, verify Forgejo's merge state, then remove temporary credentials and clone. |
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
| Apple compile-only check | `make apple-build` passed for generic iOS and tvOS simulator targets after all adversarial-review corrections; no test action ran. |
| Implementation commit | `4ebd156e` (`feat(apple): bound pause resume presentation`). |
| Adversarial review | One required pass completed against frozen head `46c196dd`; all five findings corrected before opening the test window. |
| Focused promotion tests | `PlayerResumeTests` plus the rapid Pause/Resume publication regression passed 16/16 on iOS and 16/16 on tvOS after the one required review. No full suite was run. |
| First fast-lane result | Failed only `history-check`: commits `4ebd156e` and `8ecf363a` lacked client-fix ledger anchors. The candidate correction adds durable production-to-regression mappings. |
| Corrected fast-lane result | Release version, policy/contracts, Rust, and Apple passed on `056e1aa4`. Web and Android were correctly skipped. Windows failed during pre-compile cache maintenance (`ci-cache-prune`), and the owner explicitly waived Windows plus the dependent Main promotion gate. |
| Physical hardware | Not yet available to this implementation session; acceptance remains open unless a device run is completed. |

## Completion rule

The implementation is complete only when the PR contains the bounded
resume owner, regression coverage, synchronized lifecycle documentation,
issue-keyed Apple build note, and advisory Developer setting; one adversarial
review has been addressed; selected fast-lane evidence passes or carries an
explicit recorded owner exception; and Forgejo records the merge. Physical
latency acceptance remains separately truthful if hardware cannot be exercised.
