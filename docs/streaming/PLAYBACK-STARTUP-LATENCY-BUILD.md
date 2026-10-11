# Playback startup build — Sol's execution contract through a qualified merge

**Status:** review findings addressed; preparing exact-head fast lane ·
**Written:** 2026-09-29 EDT · **Builder:** Sol ·
**Executes:** [the reviewed implementation plan](PLAYBACK-STARTUP-LATENCY-IMPLEMENTATION.md)
and [R1–R3 review dispositions](PLAYBACK-STARTUP-LATENCY-REVIEW.md).

This is the execution handoff. The implementation plan owns the measured
incident, runtime contracts and acceptance matrix. This document owns the
workspace, current user rulings, ordered work, delivery and completion.
Maintain [the status page](PLAYBACK-STARTUP-LATENCY-STATUS.html) after each
meaningful milestone or blocker. Continue until the work is implemented,
reviewed, fast-lane green and merged, or a concrete external blocker prevents
further useful progress. Do not stop after the investigation or a draft PR.

## 1. Current user rulings supersede the earlier delivery workflow

The user explicitly requested proper commits, a larger batched PR, one
adversarial implementation review only when ready to merge to main, review
fixes before fast-lane tests, and merge after those tests pass. Another
process handles broad full-unit failures later in batches. Minimize unit
test execution and waiting time.

For this effort these later instructions supersede the earlier per-task
review/PR structure, local focused-test-before-push requirement, and any
interpretation that requires running full suites during development:

1. Make coherent normal commits on `codex/playback-startup-latency` in the
   isolated clone. Batch M0–M5 into one draft main-bound PR.
2. Establish and use pinned Rust 1.97.1 check, formatting and Clippy during
   development. Compile checks are not unit-test runs; CI is not a compiler.
3. Author focused regressions alongside behavior but defer their execution
   and unit-suite execution until the final review is addressed and the
   fast lane runs. Do not claim tests passed before that point.
4. Run the bounded playback measurements and policy experiments needed to
   choose a correct implementation. These are the M0/M2 design evidence,
   not repeated unit-suite runs. Reuse captures and deterministic traces;
   measure each materially different candidate once unless results are
   insufficient or the implementation changes.
5. When all implementation and measurement work is ready, obtain one
   independent adversarial **implementation** review of the complete PR.
   The earlier plan review is not a review of future code. Address every
   actionable finding before marking the PR ready and starting the fast lane.
6. Fix fast-lane failures and rerun the affected required lane. Merge only
   when the required checks pass on the current candidate. Do not expand
   into unrelated full-unit repair campaigns or bypass a blocking failure.
   If a required failure is demonstrably unrelated, document it and resolve
   its disposition without claiming green or merging through red.

Use ordinary tracked hooks; do not add test invocations to them or bypass
them. Required compiler/lint evidence remains mandatory. If another binding
instruction cannot be reconciled with these user rulings, name the precise
conflict and continue unaffected work while the user resolves it.

The user delegates routine decisions when absent: choose the best supported
option, record alternatives and reasons in the decision ledger, and keep
building. No hidden feature gate. If a new optional/unfinished production
mode is indispensable, it needs an explicit Settings → Developer enable
control with advisory prerequisites that never override the saved choice.
Prefer a completed correction with no additional setting.

## 2. Workspace — the user's checkout is not the build workspace

The builder's repository is:

```text
/private/tmp/plurx-fast-start-build-20261002
branch: codex/playback-startup-latency
current build base: c6385996 (integrated 2026-10-02)
```

This is an independent clone of the configured remote. It contains only the
planning work transferred by this chat, not the user's uncommitted changes.
Execute every repository command with this explicit working directory.
Do not edit, stage, switch branches, compile in, or store artifacts under
`~/code/plurx`. The dispatching agent removed its two prior
untracked planning documents and their two index rows from that checkout.

The build clone is owned by this effort. Keep it until merge and transfer of
all retained evidence. Push commits to the draft PR regularly so temporary
storage is not the only copy. Clean only resources this effort created.

Read authentication from the existing local file
`~/code/plurx-agent/forgejo_token`. SSH uses the existing
`~/code/plurx-agent/.ssh-deploy-key`. Never print either value,
embed credentials in a remote URL, commit them, or transfer them with source
archives. Use Forgejo API headers through a client reading the token file.

Read [AGENTS.md](../../AGENTS.md), [the docs index](../README.md),
[the pipeline](../DEVELOPMENT_PIPELINE.md) and the reviewed plan. Reverify
symbols against the freshly fetched intended base. Other sessions are
working on Safari seeking and the same publication owners: inspect current
main/open PR changes before integration and preserve both contracts.
Do not message another chat without user authorization to that destination;
read-only inspection and source integration are sufficient for ordinary
coordination.

## 3. First actions — make the handoff durable and establish the loop

1. Confirm the explicit working directory, branch and clean/dirty state;
   preserve any commits made by the dispatching agent. Fetch remote main
   and note whether it advanced from the recorded base.
2. Verify `rustup run 1.97.1 rustc --version`. The investigating Mac's default
   `rustc` was Homebrew 1.98.0, so PATH alone is not evidence of the pin.
   Establish the [source-only compiler loop](../ci/AGENT-COMPILE-LOOP.md) if
   local pinned compilation is unavailable. Do this before Rust edits.
3. Confirm normal hook installation and required lint commands. Record the
   actual source SHA/compiler/platform, without running unit suites yet.
4. Push the existing planning commit and open one draft PR to main. Its
   initial title may describe the build; rewrite title/body around the final
   observable behavior before review. Attach the PR to the Sol chat using
   the app's pull-request attachment tool.
5. Update the status page with clone, branch, base, PR link and current phase.
   Use `fix(playback): ...` or `perf(playback): ...` subjects for observable
   corrective commits; preserve actual `Regression-Test:` landing lines.

## 4. Execute M0–M5 without guessing the shipping threshold

| Step | Work and deliverable | Condition to advance |
|---|---|---|
| M0 | Add bounded monotonic phase timing and an isolated replay of the six-segment/7.965 s lead incident; capture actual client route and indexed control | Trace separates creation, first decodable media, writer gate, served snapshot, response admission and first frame |
| M1 | Trace exact index identity, eligibility, claims, resource holds and hydration over multiple cycles; fix only a demonstrated preparation defect | Classified cause, deduplication/cancellation regressions authored, or explicit no-change diagnosis |
| M2 | Run the qualified runway/cadence experiment with fixed target and shipped transports; write a numeric policy decision receipt | Legal update/reload timing, sufficient post-position coverage, bounded ActiveLowReserve stability and real client evidence |
| M3 | Implement the selected policy coherently in publication, flow, scratch and actor seams | Compiler/lint clean; regression coverage authored for early release, state changes, attempts and accounting |
| M4 | Remove proven pre-segment overhead or document its source/GOP lower bound | Phase evidence explains the original nearly-six-second first-segment interval without weakening source or decoder checks |
| M5 | Complete client/route performance comparison, lifecycle evidence and reference docs; consolidate PR | All required measurements recorded with limits; no unsupported all-client claim |

The measured incident is a 14.2-second Safari H.264 **hls.js** resume, not
proof that Safari used native HLS. A missing index selected rolling fallback.
Its sixth segment crossed requested-position-plus-48-seconds; the first
segment was already about six seconds after the click. Preserve these
qualifications in the PR and measurements.

M2 is an implementation dependency, not permission to stop building after
writing another proposal. Resolve it with evidence and judgment. If no
smaller fixed-16-second-target policy works for a class, retain its proven
policy, implement the demonstrated preparation/pre-segment improvements,
and make the remaining performance limit explicit. If finishing the requested
outcome requires the plan's larger target/packaging redesign, develop that
concrete extension and document it; keep implementation review at the final
main-bound PR as the user requested. Do not silently change target duration
mid-session or trade video quality for latency.

## 5. Non-negotiable runtime checks from the adversarial plan review

**R1 — distinct lifetimes.** Awaiting first presentation uses the current
actor's 30-second deadline. ActiveLowReserve is bounded normal playback,
not renewed startup protection; it may persist if sustainable. Ordinary
steady cadence is an optional promotion only when its next complete cycle
is covered. At 1.05× production, growing 24 seconds of reserve takes at least
480 seconds: do not retire healthy playback at a startup deadline because
reserve growth is slow.

**R2 — legal protocol timing.** For target 16 seconds, non-final updates are
8–24 seconds apart; actual client discovery still includes changed/unchanged
reload timing. Do not remove a prefix leaving a non-final playlist below
three targets. Prove native initial selection; a faster server poll alone
does not prove faster client startup.

**R3 — immutable versus derived state.** Freeze policy version, qualification
class and target per attempt. Derive current bounds from accepted position
and rate, covering 0.25×–4× and clamp edges. Rate changes do not reset
availability or deadlines, revoke advertised bytes, shrink existing grants
retroactively, or authorize successor media with predecessor evidence.

Retain the plan's exact response admission, init validation, origin math,
EOF proof, scratch accounting, resource cleanup, cancellation and quality
contracts. Default compatibility behavior is not a new configurable feature
gate; do not disguise an unqualified experiment as a production mode.

## 6. Evidence, status and final review

Keep small sanitized receipts in the repository with index rows. Store bulky
logs/media in an effort-owned evidence directory outside the user's checkout,
referenced by path and hash. Do not retain private media paths in public-facing
fixtures or credential-bearing command lines in receipts.

The status page must show: last update, phase, source SHA, branch/PR, compiler
evidence, measurement result, regression execution state, review disposition,
fast-lane status, decisions and blockers. Say **authored, not run** for tests
until the final lane executes. Only mark physical devices verified after
actual physical playback. Do not equate a synthetic replay with fleet proof.

When ready, give the adversarial reviewer the complete diff, selected M2
policy, failure cases, retained measurements, authored regressions and known
limits. Ask specifically about early stalls, native holdback, long/open GOPs,
unbounded low-reserve operation, timeline/attempt fencing, scratch release,
queue fairness, compatibility and evidence quality. Resolve findings in
normal commits, then run the required fast lane and correct failures.

Before merging, confirm exact current head/base and required green checks;
preserve regression lines in Forgejo's `MergeMessageField`. After merge,
record the landing SHA and actual achieved results on the status page,
remove the remote task branch if no longer needed, and clean owned temporary
clones, processes and large captures once retained outputs are durable.
Never remove shared tools, credential files or another session's resources.

Production deployment is a separate operation from this authorized build
and merge. Prepare the candidate and rollout evidence; do not represent
merged code as deployed or the user's actual startup as repaired without
a deployment and playback observation.

## 7. Decision ledger

| Date | Decision | Why / remaining evidence |
|---|---|---|
| 2026-09-29 EDT | One batched draft PR, one final implementation review, then fast-lane tests | Latest explicit user delivery instruction supersedes earlier per-task/local-unit workflow |
| 2026-09-29 EDT | Separate clone; planning files removed from user checkout | User requires their working tree contain none of this effort's additions |
| 2026-09-29 EDT | Preserve evidence-gated numeric policy selection | A smaller constant alone can exchange startup latency for a later stall |
| 2026-09-29 EDT | Existing plan review retained as design history | Future implementation still needs its own final adversarial review |

## 8. October 2 native-startup amendment build

The user authorized the revised native-startup incident packet with “ok build
it” on October 2. This resumes the contracts against fresh main, preserves
the prior conservative settings correction, and builds Track N plus the
ordinary-HLS publication candidate and preparation deadline alignment.
The source clone above is the current owned build workspace.

The candidate freezes transport qualification at create, preserves the
12-second writer gate, fixed 16-second target and steady 48-second production
allowance, and derives a separate first-snapshot runway. Numeric selection
remains dependent on M2 evidence; the current 32-second-minimum candidate is not a
claim of qualification. Unknown/legacy clients retain the conservative
policy. Apple/Android physical qualification remains open; their absent
transport hint retains the conservative policy.

Native readiness, same-session reload and compatible-route admission reuse
Track N. Track S's joinable encoded-source preparation is separate outstanding
work and is not claimed delivered by this amendment. No production deployment,
queue reset, digest migration or live reference-film acceptance has occurred.

### Final admission disposition

The independent review's source-class blocker is resolved by its conservative
alternative: every production transport retains Conservative before the first
response. WebFixedHlsV1 and its selector are test-only. There is no production
experimental mode or additional setting. The native preparation/error repair
remains active. Two focused regressions prove conservative admission and
first publication; the five affected candidate fixtures continue to qualify
the isolated experiment only. The bounded current-client Safari smoke starts
at 12.858 seconds. The couple-second target remains future packaging/source
work, and no deployment or live reference-film acceptance is claimed.
