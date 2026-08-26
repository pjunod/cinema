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
was amended before unit tests were started; the PR remains draft until the
test and CI gates pass.

The implementation deliberately remains behavior-neutral. It advertises an
optional fenced control capability, renews the compatibility engine's real
activity clock only for a newly accepted sequence, reports joined delivery
facts, and returns `action: none`. It does not remove a watchdog or activate
automatic replacement policy in M1.

## Findings and remediation

| Severity | Finding | Resolution |
| --- | --- | --- |
| P1 | Durable owner validation and the later local activity touch formed a check/use race with retirement, takeover, and deletion. | Added an authoritative route re-read as the mutation linearization point. Live sessions hold the existing `child_transition` gate from that read through sequence acceptance and touch. VOD uses a lifecycle gate shared by control, terminal end, and idle reap. Exact generation, owner node, epoch, active state, and unexpired owner lease are rechecked while the gate is held. |
| P1 | A finished transcode-cache session reported the five-minute VOD control lifetime even though its owning rolling registry reaps it after 60 seconds. | `StartInfo` and the internal start response now carry the explicit timeout enforced by the owning registry. Immutable VOD reports 300,000 ms; live recovery and finished transcode-cache sessions report 60,000 ms. Idempotent owner-epoch refresh preserves that engine choice. |
| P1 | Equal/stale traffic could bypass the owner-local sequence rate limit and still force route lookups and relays. | Added bounded fixed-window admission before durable work at public ingress and again after exact authentication at owner ingress. It permits transport retries, caps one-session traffic, caps node-wide random-capability probes, evicts bounded stale entries, and returns typed `429 control_rate_limited`. |
| P1 | A peer could hold the relay response open, return an oversized body, or return arbitrary JSON; the local handler also lacked one full-exchange deadline. | Both public and owner-local exchanges have a four-second envelope. Remote control uses a bounded full-body request, a 64 KiB response cap, strict response/error deserialization, status/code pairing, owner-tuple checks, and reconstruction of a minimal JSON/no-store response. |
| P1 | Delivery facts mixed session-relative frontiers with film-time client positions, inferred producer health, and echoed requested rather than delivered height. | Live frontiers are translated through the durable media origin. Producer state is reported from failed/cached/held/running/complete or VOD producer facts. Effective height comes from the persisted start response that describes the delivered session. |
| P1 | An active durable route with no local worker returned an ordinary 404, hiding a placement transition. | It now returns typed `425 owner_transition` with a bounded retry hint. Terminal durable state remains `410 session_ended`; revalidation/storage failure is `503 control_unavailable`. |
| P1 | Route, lease, replay-renewal, and relay acceptance behavior lacked focused tests. | Added contract tests for explicit engine lifetimes, first-sequence capability snapshots, sequence replay/fencing/rate behavior, route generation/owner/lease/end authority, ingress admission, bounded relay schemas, status/code pairing, and metric dimensions. The tests were written before the first test execution. |
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

## Validation order

The required order for PR #602 is:

1. adversarial review;
2. remediate every accepted finding and record the disposition here;
3. run focused unit tests for the protocol, route authority, admission, relay,
   and engine lifetime contracts;
4. run the repository's broader unit and merge gates;
5. fix failures, re-run until green, mark the PR ready, and merge only when it
   is mergeable.

At the time this record was written, formatting, whitespace validation, and a
compile-only `cargo check --locked -p plurxd` pass were clean. Unit tests had
not yet been run.
