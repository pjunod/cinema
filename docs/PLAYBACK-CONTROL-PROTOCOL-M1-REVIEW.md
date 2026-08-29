# Playback control protocol M1 adversarial review

This records the pre-test adversarial review of implementation PR #602 and
the remediation applied before its first unit-test run. The review evaluated
the implementation against
[PLAYBACK-CONTROL-PROTOCOL-PLAN.md](PLAYBACK-CONTROL-PROTOCOL-PLAN.md) and the
M1 contract in
[PLAYBACK-CONTROL-PROTOCOL-M1.md](PLAYBACK-CONTROL-PROTOCOL-M1.md).

## Verdict

The first review requested changes. It found no P0 issue, seven P1 issues, and
two P2 contract/observability issues. Every finding was accepted. The branch
was amended before unit tests were started. Two subsequent adversarial passes
found four more P1 concurrency/deadline defects in that remediation; those
were also accepted and corrected. The PR remains draft until the final review,
test, and CI gates pass.

The implementation deliberately remains behavior-neutral. It advertises an
optional fenced control capability, renews the compatibility engine's real
activity clock only for a newly accepted sequence, reports joined delivery
facts, and returns `action: none`. It does not remove a watchdog or activate
automatic replacement policy in M1.

## Findings and remediation

| Severity | Finding | Resolution |
| --- | --- | --- |
| P1 | Durable owner validation and the later local activity touch formed a check/use race with retirement, takeover, and deletion. | Added an authoritative route re-read as the mutation linearization point. Live sessions hold the existing `child_transition` gate from that read through sequence acceptance and touch. VOD uses a per-session lifecycle gate shared by control, terminal end, and idle reap. Exact generation, owner node, epoch, active state, and unexpired owner lease are rechecked while the gate is held. |
| P1 | A finished transcode-cache session reported the five-minute VOD control lifetime even though its owning rolling registry reaps it after 60 seconds. | `StartInfo` and the internal start response now carry the explicit timeout enforced by the owning registry. Immutable VOD reports 300,000 ms; live recovery and finished transcode-cache sessions report 60,000 ms. Idempotent owner-epoch refresh preserves that engine choice. |
| P1 | Equal/stale traffic could bypass the owner-local sequence rate limit and still force route lookups and relays. | Added bounded fixed-window admission before durable work at public ingress and again after exact authentication at owner ingress. It permits transport retries, caps one-session traffic, caps node-wide random-capability probes, evicts bounded stale entries, and returns typed `429 control_rate_limited`. |
| P1 | A peer could hold the relay response open, return an oversized body, or return arbitrary JSON; the local handler also lacked one full-exchange deadline. | Public ingress mints one absolute four-second deadline and the exact-write relay carries that deadline to the owner. The owner bounds authentication, its authoritative Store read, and local mutation to the remaining budget instead of restarting it. Remote control also uses a bounded full-body request, a 64 KiB response cap, strict response/error deserialization, status/code pairing, owner-tuple checks, and reconstruction of a minimal JSON/no-store response. |
| P1 | Delivery facts mixed session-relative frontiers with film-time client positions, inferred producer health, and echoed requested rather than delivered height. | Live frontiers are translated through the durable media origin. Producer state is reported from failed/cached/held/running/complete or VOD producer facts. Effective height comes from the persisted start response that describes the delivered session. |
| P1 | An active durable route with no local worker returned an ordinary 404, hiding a placement transition. | It now returns typed `425 owner_transition` with a bounded retry hint. Terminal durable state remains `410 session_ended`; revalidation/storage failure is `503 control_unavailable`. |
| P1 | Route, lease, replay-renewal, and relay acceptance behavior lacked focused tests. | Added contract tests for explicit engine lifetimes, first-sequence capability snapshots, sequence replay/fencing/rate behavior, real live/VOD activity clocks, per-session VOD lifecycle independence, cancellation and retirement races through the real live manager, missing-worker HTTP mapping, route generation/owner/lease/end authority, inherited relay budget, ingress admission, bounded relay schemas, status/code pairing, and metric dimensions. The first set was written before the first test execution. |
| P2 | Validation allowed a buffer frontier behind its anchor, did not bound a disconnected following range, required error detail whenever an error code existed, and did not require capabilities for the first owner sequence. | Buffer-through must reach the playhead/seek anchor and a following range may begin at most one bounded target duration later. Error detail is optional but cannot exist without a typed code. Sequence 1 requires capabilities, and owner-local state independently requires the first accepted sequence to be 1 with a fixed platform snapshot. |
| P2 | Internal early exits and remote relays were under-instrumented; client platform was not joined to successful outcomes. | Internal owner mismatch, stale generation, ended/expired route, admission, and storage exits now record bounded outcomes. Added accepted/replay-by-platform counters plus remote relay result counters and a fixed-bucket full-relay latency histogram. |

## Preserved properties

The review found these areas sound and they remain unchanged in intent:

- control uses a dedicated exact-write-authenticated internal route rather
  than inheriting read-relay authorization;
- request and relay bodies are hard-capped, strict, and deny unknown fields;
- generation, owner epoch, client instance, and sequence are all explicit
  mutation fences;
- an equal sequence replays the prior outcome without renewing activity;
- capability advertisement is replicated, default-off, and persisted in the
  durable start response; and
- metrics and logs use fixed dimensions and hashed session correlation rather
  than bearer capabilities or other unbounded identifiers.

## Re-review findings

The first re-review, at commit `5b244b9d`, found three P1 issues in the first
remediation:

- a node-wide VOD lifecycle mutex serialized every VOD control and made every
  rolling control perform an unnecessary durable VOD lookup;
- serving fences could set the live `retired` bit between the final check and
  the activity-clock write; and
- the exact-write owner endpoint started a fresh local timeout after its first
  durable Store read.

The replacement uses per-session VOD lifecycle gates and resolves VOD registry
ownership before Store I/O. Live retirement and activity mutation now share a
small `retirement_activity` gate independent of encoder transition work. The
relay envelope carries the ingress's absolute deadline, and the owner applies
only its remaining bounded budget to authentication, Store I/O, and mutation.

The second re-review, at commit `4416a158`, found one remaining P1: live
`ControlState` advanced the sequence before awaiting the activity clock, so a
deadline cancellation could consume a sequence without renewing its lease.
Live control now acquires every asynchronous lock before mutation, then commits
the synchronous sequence decision and activity-clock update together. The
manager-level regression deliberately blocks that clock, cancels the exchange,
and proves the identical sequence remains newly acceptable; it also drives a
serving fence through the same window and proves neither fact commits.

The final adversarial pass at commit `d4f3e3a8` approved the implementation as
mergeable with no remaining or newly introduced P0/P1 finding. It confirmed
the live lock order (`last_request` → `retirement_activity` → synchronous
`ControlState`), the absence of a cancellation point after sequence
acceptance, the durable and registry fences before that atomic commit, and the
deterministic cancellation/retirement regression coverage.

## Validation order

The required order for PR #602 is:

1. adversarial review;
2. remediate every accepted finding and record the disposition here;
3. run focused unit tests for the protocol, route authority, admission, relay,
   and engine lifetime contracts;
4. run the repository's broader unit and merge gates;
5. fix failures, re-run until green, mark the PR ready, and merge only when it
   is mergeable.

The required first-review-before-tests order was preserved. After the first
review and its remediation, the complete local Rust lane passed formatting,
workspace Clippy with warnings denied, 754 core tests, 30 Store-contract tests,
994 server tests with three ignored, and all remaining package and doc tests.
One unrelated thumbnail-cancellation timing test failed once in an earlier
run, passed immediately in isolation, and the complete lane then passed from
start to finish. Later re-review remediations added the focused engine,
endpoint, deadline, and cancellation tests described above. After the final
adversarial verdict, the complete lane passed again: formatting, workspace
Clippy with warnings denied, 754 core tests, 30 Store-contract tests, 998 server
tests with three ignored, and every remaining package and doc test.
