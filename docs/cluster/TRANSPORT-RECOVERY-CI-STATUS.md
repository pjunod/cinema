# Transport recovery CI — implementation status and evidence

**Status:** M6 final candidate unit correction verified; full PR qualification next · **Owner:** Codex · **Updated:** 2026-09-09

Companion to
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (how this effort reaches
`main`) and
[TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md](TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md)
(what the retained leak statistic asserts) — this page says what has actually
been built, reviewed, and proved. It is updated with each milestone so a green
checkbox means retained evidence, not intent.

## Current position — the one M6 adversarial review is complete

The effort begins at remote `main`
`9fcd151c98485707d2dc28195adf9c0c1e4a6d4c`, the same source reviewed by the
implementation handoff. Work is isolated on `effort/transport-recovery-ci`;
reviewable milestones use `codex/transport-recovery-ci-m*` branches and the
effort development gate.

Forgejo run
[#1034](http://192.168.4.7:3000/noirr/plurx/actions/runs/1034) remains the
timing baseline: about 123 seconds before the campaign, about 100 minutes for
voter warmup plus 20 recoveries, then a resource-envelope failure. The newer
main run
[#1100](http://192.168.4.7:3000/noirr/plurx/actions/runs/1100) had not reached
a qualification verdict; its old combined campaign was cancelled on
2026-09-08 at the operator's request. The older queued run
[#1098](http://192.168.4.7:3000/noirr/plurx/actions/runs/1098) was cancelled at
the same time. No smoke or partial run is recorded as qualification.

**How to read it:** M1–M5 development stays on the compile-and-policy effort
lane, so it does not start the old four-hour campaign. M6 deliberately restores
and runs the full voter and learner qualification once the final candidate is
frozen.

Draft promotion PR
[#191](http://192.168.4.7:3000/noirr/plurx/pulls/191) contains the current
`main` merge and remains unqualified. Its automatically started pre-review run
[#1214](http://192.168.4.7:3000/noirr/plurx/actions/runs/1214) was cancelled
before expensive work so it cannot be mistaken for the one final candidate
run. Exactly one adversarial agent review is complete. Its nine findings are
implemented in the local promotion candidate: a frozen Make execution ID;
Cargo freshness and an embedded qualification SHA; cancellation latched
through cleanup, publication, and the role handoff; summary-before-report
publication with failure repair; best-effort cleanup of every owned process;
a recursively closed role schema; deepest failing-phase preservation;
UTF-8-safe diagnostic truncation; and role-schema validation routing. The
reviewed candidate passed the focused Rust suite (65 tests), the full local
operations suite (294 tests), the full local validation suite (198 tests with
one platform skip), the pinned Rust 1.97.1 check and Clippy gate, formatting,
documentation indexing, history audit, and an exact-source Linux run of all 67
transport-recovery tests.
Current `main` moved after that review completed. The candidate includes
`2e77e2e1`, including the decoder selection and recovery promotion and the
Apple terminal-settlement retry. Forgejo run
[#1256](http://192.168.4.7:3000/noirr/plurx/actions/runs/1256) stopped at fast
preflight because the final status-only commit `0247c428` lacked the required
non-runtime history-ledger entry. Run
[#1259](http://192.168.4.7:3000/noirr/plurx/actions/runs/1259) then reached the
ordinary matrix but never started either 20-cycle role. It exposed a full
Apple-suite teardown race and current-base Store migration failures; the fast
Rust and web jobs stopped at a runner disk floor, the Android emulator stopped
after boot, and one cluster-daemon case failed once before passing an exact
focused rerun on pinned Rust 1.97.1. The affected runner reclaimed 10.62 GB of
builder cache. Apple finalization now has a completion boundary before an
injected transport is invalidated, and all 78 affected reporter and session
tests pass on the exact merged tree. Store migration fixtures now remove every
post-marker schema object in reverse migration order, preserve the v32
idempotence boundary, and seed current cache-publication identity explicitly.
On exact archived commit `ac9b6ffc`, all four focused Store regressions pass on
Rust 1.97.1: the v5 migration chain, v32 producer-recovery migration and replay,
analysis stale-marker replay, and current SQLite import. The same source passes
the all-target `plurx-core` check and Clippy with warnings denied. Formatting and
whitespace checks are clean. Final-candidate run
[#1281](http://192.168.4.7:3000/noirr/plurx/actions/runs/1281) stopped in the
mobile-version preflight because the Apple source correction still claimed
current `main` build 124. No transport role started. The repository-owned build
tool has now claimed build 125 across every generated release surface. The next
run
[#1286](http://192.168.4.7:3000/noirr/plurx/actions/runs/1286) passed mobile
versioning, preflight, WAL, Android JVM and instrumented UI, and both package
architectures before it was cancelled. Its cluster-daemon job never executed a
test because a second runner sat below the 25 GB disk floor; restarting its idle
Docker daemon released deleted data and restored 43.4 GB. Its Rust gate passed
2,141 tests and exposed one current-main fixture contradiction: a generated
Main10/PQ file was handed to the strict resolver with an 8-bit SDR decode-facts
snapshot. Commit `20c047ec` makes that test helper describe the file it actually
generated. The exact HDR10 restart regression, all-target `plurxd` check, and
Clippy with warnings denied pass on archived Rust 1.97.1 source. Neither
20-cycle role started in either rejected run. The next push is the frozen
candidate for the one full PR qualification run; no second adversarial review
is planned or required.

## Milestones — evidence closes the checkbox

- [x] **M0 — establish the base.** Rust 1.97.1 was invoked explicitly because
  Homebrew shadows the repository pin with 1.98.0. `cargo fmt --all --check`,
  package check, and package Clippy with denied warnings passed before edits.
- [x] **M1 — independent role reports.** Typed plan, strict CLI, shared
  validation, compatible aliases, and canonical assembly pass focused tests
  and one real Linux warmup-plus-one smoke for each role.
- [x] **M2 — durable diagnostics.** Checkpoints, timings, failure classes,
  cancellation, and owned-child cleanup evidence.
- [x] **M3 — split CI.** Independent voter and learner jobs, retained aggregate
  job ID, current-run artifacts, and exact receipt integration.
- [x] **M4 — frozen candidate semantics.** Promotion runs survive unrelated
  superseding events and stale candidates cannot issue a receipt.
- [x] **M5 — measured optimization.** Three-cycle, same-runner comparisons
  decide whether any profile or hashing change is retained.
- [ ] **M6 — current-tree qualification.** One attempt produces both 20-cycle
  role reports, the canonical artifact, and the transport and promotion
  receipts.

## Source map — one owner per contract

| Area | Responsibility in this effort |
|---|---|
| `crates/plurx-cluster-check/src/transport_recovery.rs` | Role runner, report validation, assembly, journals, and timings |
| `crates/plurx-cluster-check/src/lib.rs` | CLI dispatch and process lifecycle plumbing |
| `benchmarks/` | Closed role-report schema; canonical schema remains version 2 |
| `Makefile` | Contracts, role, smoke, and compatible full targets |
| `.github/workflows/ci.yml` | Independent jobs, aggregate evidence, and promotion cancellation policy |
| `validation/` | Same-run, first-attempt, exact-candidate receipts |
| `tests/operations/` and `tests/validation/` | Static scheduling, schema, receipt, and stale-candidate regressions |
| Maintained docs | Commands, output interpretation, budgets, and freeze procedure |

## M1 evidence — both roles now finish independently

Commit `b9547a2b03bc5bd5a499a5437b51575ef1c5f359` was archived without
repository metadata or credentials and built in the Rust 1.97.1 Linux image
on `nynuc`. One shared execution ID, `m1-linux-b9547a2b`, produced two closed
smoke reports:

| Role | Warmup | Measured cycles | Recovery | Envelope | Report SHA-256 |
|---|---:|---:|---:|---|---|
| Voter | 1 | 1 | 258,310 ms | Recorded, not asserted | `70653f81105b001c0a4d35960cce5270e7df8f792c25e15cdc89c7beeda92d3b` |
| Learner | 1 | 1 | 241,880 ms | Recorded, not asserted | `d03bf574e40b4aedbcf9eb7d6b752a21911855f43670333353fa474633ba78a3` |

Both roles used separate persistent source clusters, replaced node 4, moved
acknowledged writes through the existing external TLS writer, validated the
exact installed snapshot, shut down their children, passed the closed role
schema and shared semantic validator, and published atomically. The learner
completed admission before its warmup and measured cycle. These are M1 smoke
results only; neither can assemble or stand in for 20-cycle qualification.

The first acceptance attempt exposed two harness-boundary defects. Whole-
millisecond event ages sampled in separate processes could invert causal order
by one millisecond, and the continuous writer could advance the current
snapshot after the purge boundary had already been satisfied. The correction
allows only two milliseconds of cross-process timestamp quantization and
records the specific snapshot boundary whose purge was awaited; exact
transferred snapshot identity, bytes, digest, and transport completion remain
separate mandatory evidence. Focused package tests now contain 51 cases.

The node's 31 GiB `/tmp` tmpfs filled during the initial cold link. The exact
source-only rerun used `/var/tmp`, which had 108 GiB available. This is runner
capacity evidence, not a product or campaign failure.

## M2 evidence — success and failure remain observable

Commits `0d163adff4b50a40651c08d2813cfc7d81f11f83` and
`df8b324fc07cf5b91103435c20544fb6c4b7b34f` were archived without repository
metadata or credentials and built in a local Linux ARM64 container with the
repository-pinned Rust 1.97.1 toolchain. The common role runner now writes
create-new `events.jsonl` and
`cycles.jsonl` journals plus an atomic terminal `summary.json` outside the
temporary cluster root. Records include named phase and learner-admission
subphase timings, every resource-sampling attempt, source and replacement
process identities, synchronized completed-cycle evidence, classified failure
context, and bounded cleanup results. A passing role report is still withheld
until orderly teardown and diagnostic summary publication succeed.

SIGINT, SIGTERM, and the optional role runtime limit now interrupt the active
role, drop its in-flight protocol exchange, and enter a 30-second cleanup path
that targets only registered PID/start-time identities. External cancellation
suppresses the second role in the compatible paired wrapper; an ordinary role
failure still does not. The pinned package check, formatting, Clippy with
warnings denied, and 58 focused transport-recovery tests pass on macOS. The
Linux suite contains one additional real-process SIGTERM regression and all 59
tests pass.

One shared execution ID, `m2-local-df8b324f`, completed a warmup and three
measured cycles for both roles on the same 16-vCPU local Docker runner. Both
role reports passed the offline schema and semantic validators:

| Role | Total wall | Dominant phase median / max | Role SHA-256 | Summary SHA-256 |
|---|---:|---|---|---|
| Voter | 537,896 ms | snapshot purge 98,595 / 108,835 ms | `1ebc9dd51d1a76412a3b7e77fcba878b347d57dcebf6380c2d5dd2ad9f6b94dd` | `046eae8e7617d98c50eb60724a3579eecfb8f0c7ff62520c38958b9c23370b36` |
| Learner | 753,120 ms | snapshot purge 150,621 / 169,163 ms | `431a3860a45e56a179e3d44a874370b5c6f79c6d631a4bd6ad67b449db54718d` | `cca687adb5199f1d8bda875535fef38d3434b9cc3133befc7d094b2fc110738e` |

The learner summary also contains every named admission subphase. A separate
one-second bounded run failed during `cluster_start` as
`orchestration_timeout`, recorded cycle zero and zero completed cycles, and
reaped all four registered processes within the 30-second budget with no
survivors. Its terminal summary SHA-256 is
`4c97661cd4aff2d59be3f4b56106536e75e9ee363db5fa01b61290f212e040e1`;
no passing role report was published. That deliberate failure exposed an
early-return path which initially skipped teardown; `df8b324f` fixes it and the
evidence above comes from an exact-source rerun after the correction.

## M3 evidence — each role has its own outcome and budget

Commit `b5aeaee4` replaces the combined CI process with four explicit jobs: a
30-minute Linux contract gate, independent 240-minute voter and learner jobs,
and a 30-minute aggregate retaining the existing
`cluster_transport_recovery` job ID. Neither role depends on the other. Each
builds the exact candidate first, then receives the remainder of a 13,800-
second role budget so ten minutes remain for diagnostics and artifact upload.
Role reports, journals, summaries, and logs are retained for 14 days even when
the role fails.

The aggregate runs under `always()`, rejects any unsuccessful prerequisite or
download, and accepts only the two named artifacts from its own workflow run.
It assembles reports sharing `ci-${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}` and
then sends the canonical artifact through the existing semantic validator and
exact-SHA lane receipt. Local `cluster-transport-recovery-check` preserves the
compatible entry point but attempts both roles independently and assembles
nothing unless both pass.

The complete 55-test operations contract suite, 20 lane-receipt and
qualification tests, YAML parsing, history audit, and whitespace checks pass.
An archived M3 Make tree at `c2771841` then ran the split smoke entry point in
Rust 1.97.1 Linux with execution ID `m3-local-c2771841`; M3 changes no Rust, so
the reused runner binary correctly retained parent build SHA `df8b324f`.

| Role | Measured cycles | Total wall | Role SHA-256 | Summary SHA-256 |
|---|---:|---:|---|---|
| Voter | 1 | 149,368 ms | `3b972608193279cae5b99283b2948ff3220fd535e205658821b9416c6576de56` | `9b3178b1c29de1fc439bd2bbf042477e4907a3622321c610357840a526b3b86d` |
| Learner | 1 | 473,463 ms | `58525bf89423e5718b9dbea7a98405a60b7ce0e6272cad4a2e2e4b4ec942404f` | `79c76e494e27b2a93463b327eb5455fe7dcabacf2d74082fd169b1a13a1732cc` |

This is smoke evidence, not qualification: the resource envelope is recorded
but not asserted below ten closing samples, and no canonical artifact or
receipt was issued. The former combined campaign remains suppressed; the
split 20-plus-20 lane is the implementation that will run once on the frozen
M6 promotion candidate.

## M4 evidence — retained runs cannot qualify stale refs

Commit `ee3f2241` makes workflow cancellation conditional from pull-request
event context, the only context available before jobs exist. Superseded effort
task PRs, ordinary main-bound PRs, and `main` pushes still cancel. Final
`effort/**` and nonempty `integration/*-into-main` promotion PRs do not, and
immutable tags remain uncancelled. The policy matches Forgejo's documented
workflow-level concurrency behavior; job-level settings cannot rescue a
cancelled parent workflow.

Immediately before writing a promotion receipt, the gate force-fetches the
live head and base refs into fixed internal refs. The receipt builder now
requires those current SHAs and refuses either one when it differs from the
event-time head or base. The existing exact checked-out SHA, complete fan-out,
and workflow-attempt-1 requirements remain unchanged. Nine focused receipt
tests cover moved head and moved base; the 56-test operations suite pins all
six cancellation cases and the fixed-ref fetch.

The conditional cancellation mechanism was exercised on installed Forgejo
16.0.3 with a temporary workflow isolated to two `codex/m4-concurrency-probe-*`
branches:

| Policy | First run | Superseding run | Observed result |
|---|---|---|---|
| `cancel-in-progress: false` | [#1194](http://192.168.4.7:3000/noirr/plurx/actions/runs/1194) | [#1195](http://192.168.4.7:3000/noirr/plurx/actions/runs/1195) | Both passed; the second waited for the first's 30-second hold |
| `cancel-in-progress: true` | [#1196](http://192.168.4.7:3000/noirr/plurx/actions/runs/1196) | [#1198](http://192.168.4.7:3000/noirr/plurx/actions/runs/1198) | The first was cancelled when the second started; the second passed |

The exact temporary branches were deleted after the results were retained;
no real user run or repository workflow was cancelled for this test. The
integration freeze remains an operator convention rather than a YAML merge
queue: moved refs fail closed instead of being locked.

## M5 evidence — measured code is fast enough to qualify

Commit `948a14c5` adds a dedicated `transport-recovery` Cargo profile with
optimization level 2, debug information, overflow checks, and debug assertions.
The exact archived source built with Rust 1.97.1 in 197.862 seconds on the same
local 16-vCPU Linux ARM64 Docker runner used for the M2 baseline. The runner
had 8,318,709,760 bytes of memory, used the `overlay2` storage driver, and had
no competing Docker containers when the comparison evidence was collected.
The binary stamps its actual build settings into every role report; Cargo
exposes the custom profile to build scripts as the `debug` family, so the
closed descriptor is
`profile=debug;opt-level=2;debug=true;debug-assertions=on`.

One shared execution ID, `m5-opt-948a14c5`, then completed a warmup and three
measured cycles for each role. Both reports passed the closed schema and
semantic validators, including snapshot digest, installed-state, write-path,
process-identity, cleanup, and report-integrity assertions:

| Role | Total wall | Change from M2 | Snapshot purge median | Source hash median | Role SHA-256 | Summary SHA-256 |
|---|---:|---:|---:|---:|---|---|
| Voter | 70,995 ms | -86.8% | 449 ms | 445 ms | `f94bcb520703939a8daadf8710294dcf0861be96060573fdca6f1702c788d1c2` | `88ff76a996d10dee7bf0a94bea798c5fbd76f7a77c30017afc130f76e5df9427` |
| Learner | 415,169 ms | -44.9% | 83,610 ms | 235 ms | `fb6cdb2e1ac8fe9b211454c52eea413d8901f38ed90ceb06a4d67a9c23310c99` | `29a1daf91119436f2c855b55c8e43125385c68c2b5cb7863a003477766855811` |

Installed-state verification fell from 21,870 to 3,088 ms for the voter and
from 11,866 to 2,024 ms for the learner. Source hashing is now below half a
second for both roles, so no hashing implementation change is justified or
retained. Learner snapshot purge remains the dominant phase and is preserved
as observable timing rather than hidden by a looser assertion. The resource
envelope was recorded but not asserted for this three-cycle comparison, as
required by the existing minimum-sample contract.

The pinned macOS loop passed formatting, package check, Clippy with warnings
denied, and 59 focused tests. The exact Linux source passed all 60 focused
tests plus the vendored Hiqlite transport and snapshot-worker regressions. The
optimized profile is therefore retained for both independent CI role jobs and
the aggregate validator; the 20-plus-20 M6 run remains the only qualification.

## Guardrails — this effort changes observability, not production transport

The 1,500-second recovery deadline, 60-second resource cleanup horizon,
three-second sampling interval, stability rule, image sizes, 20-cycle role
qualification, writer checks, and resource allowances remain unchanged. A
short smoke proves recovery and integrity only; it cannot assemble the
canonical artifact or qualify a candidate. Production heartbeat ownership and
transport lifecycle work remain a separate, measured follow-up.
