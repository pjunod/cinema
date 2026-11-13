# Playback-start fleet acceptance — measured results and control 503 attribution

**Status:** open · **Owner:** Codex · **Updated:** 2026-11-13.
Companion to [the continuation ledger](PLAYBACK-START-STATUS.md),
[E1 record](PLAYBACK-START-E1-RECORD.md) and
[D3b record](PLAYBACK-START-D3B-RECORD.md). This records runtime observations;
it does not turn source tests or elapsed idle time into fleet acceptance.

## E1/D3 landed and reached all four nodes

#1568 landed as `882d4b691` after fast lane 7420 and Main promotion. Its
16 regression fields are present in the landing commit. The 19:22 UTC fleet
capture reports this build on three voters and one learner, all with quorum,
replicated proof and stable images. Deployment happened independently of this
investigation. The youngest voter had 1360 seconds uptime; a later reading is
required to establish an uninterrupted hour on every process.

The original canary process restarted near 18:21 UTC. Its recreation cause is
not retained. The new process reached 3605 seconds uptime without another reset.
Only its post-reset 18:23:25–19:21:37 UTC samples are compared:

| Measurement | Comparable canary interval |
|---|---:|
| Playback requests, all methods/status classes | 1278 |
| Authority / local reads | 5287 / 393 |
| Authority / combined reads per request | 4.14 / 4.44 |
| Planning view / authority floor reads | 10 / 4 |
| Route view / authority reads | 355 / 0 |
| Settings view / authority floor reads | 218 / 1 |
| Failed image replacements | 0 |

The old 5.7 authority-reads/request baseline used different traffic. This
interval does not establish a causal reduction against that baseline.

## The 503 overlaps a durable deletion, while physical Store errors stay zero

One playback-control POST returned 503 at 18:44:13.346736 UTC after 27 ms.
Its affected session has these independent observations:

| UTC timestamp | Observation |
|---|---|
| 18:44:13.319, approximate | Control request starts, derived from elapsed time |
| 18:44:13.339 | Durable deletion invocation timestamp retained in the ended row |
| 18:44:13.346736 | Control response 503; serving ready, loss generation unchanged |
| 18:44:13.356263 | Local owner logs Deleted |
| 18:44:13.893919 | Later control returns 410 |

Read-only SQLite confirms state ended, terminal reason deleted, owner epoch 1,
and completed projection. No bearer capability is exported. The two bracketing
metrics samples have zero physical local-read, authority-read and write errors;
control Unavailable rises 0→1 and outbound control relays remain zero. TimedClient
counts physical errors before retry, so nearby heartbeat RPC timeouts do not
establish a failed database operation for this request. Embedded decoder or
other semantic errors can occur after the physical timer reports success.

The strongest explanation is concurrent release reconciliation: control reads
authority between two pending-release checks, and release installs its fence
before Store End. A pending fence becomes a generic 503. The .339 timestamp
falls inside the request and matches that sequence. The exact historical branch
is still unproven: successful DELETE ingress and its process-local fence were
not logged. A remote coordinator or semantic actor failure cannot be excluded
solely from the retained response log. The raw response body is not retained.
The original external summary's 18:48:32 timestamp was a bitrate-dispatch warning;
the table above uses the actual request-correlated HTTP response.

## Diagnostics must preserve the refusal without changing the release fence

The deployed control handler drops its route error and constructs responses
without the existing ApiErrorCode extension. The HLS observer therefore records
an untyped 503, despite a typed client body. This is the confirmed attribution
failure; it is separate from the unproven historical refusal branch.

The follow-up retains bounded route-failure stages, safe Store error classes,
protocol code and crate-relative callsite in the existing diagnostic record.
Deterministic before-read and after-read release tests preserve the public503
body and fail-closed ordering while asserting the diagnostic distinction.
No release status, retry policy, timer, watchdog or enable switch changes.

## Remaining runtime observations stay explicit

#1573 landed the missing Create plan card and asynchronous Applied view badge
as `746cb0500` after fast lane 7447 and Main promotion. The observed fleet still
runs `882d4b691`. Developer at 19:22:14 UTC reports proof/image met, settings
223 view / 1 quorum, routes 367 view / 0 quorum, and last fallback floor; the old
renderer still shows a stale badge and omits Create plan.

Final post-hour fleet results, diagnostic qualification and the exact deployment
candidate are recorded here when measured. Create plan stale-replan/refusal-close
atomics are not exposed by Prometheus; absent readings are not zero evidence.
The learner's server-info 503 explicitly reports learner_route_ineligible;
that policy refusal is not evidence of a cluster outage.

## Final fleet endpoint — every process exceeds one hour

The 20:00:14 UTC read reports the same `882d4b691` build everywhere. Compared
with 19:22:38–39, all uptimes advance and counters have no reset. Parent
calculation independently reproduced these deltas from raw metrics:

| Role | Uptime seconds | Playback requests in interval | New playback 5xx / control Unavailable |
|---|---:|---:|---:|
| Learner | 6626 | 0 | 0 / 0 |
| Voter A | 3615 | 0 | 0 / 0 |
| Voter B | 6394 | 0 | 0 / 0 |
| Leader | 5921 | 1503 | 0 / 0 |

Every node reports replicated proof, stable image, quorum available, known
leader and zero failed replacements. No legacy-layout fallback appears.
The leader's interval has 6085 authority and 457 local reads: 4.04857 authority
and 0.30406 local reads per HTTP request. The current route view has 823 view
and zero authority reads, including 821 view reads at the routing site. Control,
status bracket and verdict continue to read authority as required. Settings
has 267 view / 1 authority-floor read; planning 12 view / 4 authority-floor reads.

The observed endpoint interval is 2255 seconds, not a continuously sampled
hour. Process uptime exceeds an hour, but idle learner/other-voter ingress
cannot establish normal playback coverage. The historical 503 remains cumulative;
no new 503 or control-unavailable result occurred in the comparable interval.
These checks complete the requested post-hour fleet reading. They do not
qualify unexercised ingress paths, causal before/after improvement, the exact
historical 503 branch, or the not-yet-deployed renderer/diagnostics.

### Site and method attribution in the comparable interval

| Route site | View | Authority |
|---|---:|---:|
| routing | 446 | 0 |
| status_first | 0 | 0 |
| status_bracket | 0 | 288 |
| control | 0 | 662 |
| verdict | 0 | 1164 |
| receipt_fence / receipt_candidate | 0 / 0 | 4 / 4 |

The status_first site had no activity in this interval. Its runtime improvement
cannot be inferred from routing's view hits. The top playback authority methods
are `media_session_route` 2113, `get_setting` 1254 and `authenticate_token_for` 671.
Cluster scope reports `get_setting` 2239 authority, `queue_sql` 546 local and 242
authority, then `analysis_file_labels` 210 authority. Owner-start scope is included
in cluster by design; a higher cluster counter is not alone a regression.
