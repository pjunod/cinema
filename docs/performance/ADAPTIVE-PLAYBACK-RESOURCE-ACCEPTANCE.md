# Adaptive resource acceptance — prove service and ownership on each path

**Status:** open · planned evidence; no implementation qualification claimed ·
**Written:** 2026-10-20.

Companion to [the implementation plan](ADAPTIVE-PLAYBACK-RESOURCE-PLAN.md).
Milestones M0–M7 and section references below refer to that plan. Keep this
acceptance contract and its implementation ledger together; a passed plan
review is not a runtime pass.

## 1. Acceptance — failure cases count as much as capacity gains

The following are required test specifications, not tests claimed to exist.
M0 assigns actual module/test names before implementation. No zero-test success
or skipped platform case counts as a pass.

| Test class | Required observations |
|---|---|
| Policy and replay | Fake clock; unknown/stale/corrupt samples; non-finite/overflow inputs; changes of recipe/backend/quota; duplicate observations; bounded history; explicit caps and zero semantics |
| Atomic ownership | Simultaneous starts; family/predecessor overlap; cancellation in every phase; panic, failed wait, child descendants; UnknownHeld on panic/drop; no permit release before reap; no double count of shared audio or DVR transport |
| Measurement | Startup/steady separation; paced live input; full-buffer holds; slow client vs slow source; cached responses; blocked output; sample loss; external contention; no falsely inferred spare capacity |
| Trial protection | Warm VOD incumbents and newcomer; no fresh runway; unknown memory; startup timeout; cleanup timeout; workload complexity spike; thermal slowdown; cooldown only on real service failure; buffer-paced 0× never causes a false abort; four/six-role overlap and seventh-role refusal |
| Full processing coverage | SDR transcode, hardware/software decode/encode, HDR/DV, subtitles, audio-only, copy/remux, shared continuous renditions, Live TV, timeshift and recording with background indexing |
| Cluster correctness | Local refusal with healthy peer; local trial cannot consume reserved peer allowance; requested quality preserved; remote prepared output on cold peer; missing tools/source; all peers full; stale offers; concurrent ingresses; timeout/partition during each transaction phase; mixed versions |
| Delivery bottlenecks | Shared NAS load, saturated ingress egress, remote worker saturated read path, cold/warm cache; relocating processing never falsely clears relay/drain commitments |
| Service semantics | Manual/exact request vs Auto fallback; seek and resume; source mutation; capability/settings change; successful and cancelled replacement; generation-bound badges and truthful diagnostics |
| Maintenance | Drain during offer/reservation/prepare/adopt; new work fenced; active recording, relay, provisional/unadopted/expired preparations remain blockers; Prepare/Adopt reject drain; no false reboot-ready state |

M7 uses licensed/public or synthetic reproducible sources, never requires private
movies. Run single-stream and mixed two/three-stream scenarios on Linux Intel,
AMD/NVIDIA where the backends are supported, macOS Apple Silicon, Windows with
its supported backends, CPU-only, and a resource-limited container. Physical
cases establish only the tested configurations; algorithm tests and portable
fallbacks support unknown installations. Retain an explicit unavailable-platform
list rather than claiming universal hardware validation.

Initial release criteria: no correctness/ownership regressions; no missed DVR
capture attributable to trials; no incumbent rebuffer attributable to a trial
in the controlled campaign; all induced overloads stop new expansion before
protected runway is consumed; same-quality peer placement passes when feasible.
Relative to the baseline, p95 start/seek latency must stay within the existing
route SLA and within 10% or 250 ms, whichever additional allowance is greater;
any exception needs a documented trade-off before promotion. Telemetry overhead
must remain below 1% of one CPU core and 16 MiB per node in the 100-workload
synthetic observer load test, with bounded queues and label cardinality.
Baseline procedure: same pinned source corpus, hardware/driver/settings, source
location and client path; compare prior release with the candidate in alternating
order. For each platform/workload/start-vs-seek cell, collect 100 measured events
per build after five warmups, separately for cold and warm caches. Record failures
and timeouts as failures, never omit them from latency distributions. Use
nearest-rank p95 and report counts plus bootstrap confidence intervals. A noisy
or insufficient cell remains unqualified; do not average it into passing cells.
These are acceptance targets to measure, not current performance claims.

Evidence includes source and binary hashes, platform/driver/effective limits,
input identities, full graph recipe, actual producer count, elapsed service and
runway traces, worker/ingress/source placement, refusal reasons, exit codes and
physical cleanup proof. Do not claim capacity improvements from raised limits
without sustained concurrent playback measurements.

## 2. Build-time commands and current planning validation

These are the existing workflow entry points; focused implementation tests must
be filled in by each milestone, never replaced by the generic docs checks:

```bash
scripts/quiet python3 -m unittest discover -s tests/operations -p test_docs_index.py
scripts/quiet python3 -m unittest discover -s tests/operations -p test_infra_names.py
make paperwork
```

Before Rust changes follow [the agent compile loop](../ci/AGENT-COMPILE-LOOP.md)
and [the development pipeline](../DEVELOPMENT_PIPELINE.md). Replicated storage
regressions require `make unit-core` or `--features hiqlite-store`. API/settings
and client changes update their reference docs, release counters and indexes in
the same implementation commits.

Planning validation and review dispositions are recorded in
[the review ledger](ADAPTIVE-PLAYBACK-RESOURCE-REVIEW.md). No runtime behavior has
been changed by these documents.
