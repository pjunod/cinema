# Playback-start fleet acceptance — measured results and control 503 attribution

**Status:** requested reading complete; qualification gaps below · **Owner:** Codex · **Updated:** 2026-11-14.
Companion to [the continuation ledger](PLAYBACK-START-STATUS.md),
[E1 record](PLAYBACK-START-E1-RECORD.md) and
[D3b record](PLAYBACK-START-D3B-RECORD.md). This records runtime observations;
it does not turn source tests or elapsed idle time into fleet acceptance.

## Historical E1/D3 rollout and comparable canary interval

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

The historical `882d4b691` control handler dropped its route error and built
responses without the existing ApiErrorCode extension. The observer recorded
an untyped 503, despite a typed client body. This is the confirmed attribution
failure; it is separate from the unproven historical refusal branch.

The landed #1582 follow-up retains bounded route-failure stages, safe Store error classes,
protocol code and crate-relative callsite in the existing diagnostic record.
Deterministic before-read and after-read release tests preserve the public503
body and fail-closed ordering while asserting the diagnostic distinction.
No release status, retry policy, timer, watchdog or enable switch changes.

## Remaining runtime observations stay explicit

#1573 landed the missing Create plan card and asynchronous Applied view badge
as `746cb0500` after fast lane 7447 and Main promotion. The historical fleet still
ran `882d4b691`. Developer at 19:22:14 UTC reported proof/image met, settings
223 view / 1 quorum, routes 367 view / 0 quorum, and last fallback floor; the old
renderer showed a stale badge and omitted Create plan.

The later diagnostic rollout and its pending post-rollout hour are recorded
below. Create plan stale-replan/refusal-close
atomics are not exposed by Prometheus; absent readings are not zero evidence.
The learner's server-info 503 explicitly reports learner_route_ineligible;
that policy refusal is not evidence of a cluster outage.

## Historical 882 fleet endpoint — every process exceeds one hour

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
These checks complete the requested post-hour fleet reading. They did not
qualify unexercised ingress paths, causal before/after improvement, the exact
historical 503 branch, or the renderer/diagnostics that were not yet deployed.

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

## Bounded response capture distinguishes a later subtitle refusal

Paul approved a passive capture of at most 30 HTTP 503 response packets for
15 minutes, stored privately. It stopped at the 30-packet bound at 20:37:39 UTC.
Translated-interface duplicates were collapsed by request ID and exact payload:
10 unique responses remain, 9 learner-policy refusals and 1 leader startup_timeout.
The capture point is the leader; it includes incoming replies from peers and
must not be described as only leader-generated responses. Raw headers,
identifiers and bearer data are not committed to this repository.

The startup_timeout response says the required subtitle sidecar is still being
built because its source is still being read. Correlated extraction starts
at 20:34:58.598277 UTC; the create refuses at 20:35:03.630356; the HTTP response
is 503 at 20:35:03.633412 after 5415 ms. The phase budget is 87 ms Store and 5294 ms
VOD. This identifies a later create/sidecar-readiness refusal, separate from
the original 27 ms control/deletion overlap. It does not establish why the track
read was unfinished at the deadline; that remains a subtitle-path follow-up.
No control_unavailable response appeared in this bounded capture. Its IPv4
HTTP/1 header-packet filter and time/packet limits do not prove global absence.

## Source qualification and live evidence remain distinct

Independent final review and the bookkeeping follow-up review are clean.
Rust 1.97.1 compiled the diagnostic patch and test targets; Clippy passed.
The backend-privacy regression passed initially. The first interleaving case
then overflowed the default test-thread stack: the direct command had omitted
the existing unit harness's RUST_MIN_STACK=8388608 setting. Reusing the same
compiled binary with that standard setting ran only the failed and unrun
interleaving cases; both passed in 0.96 s. The passed privacy case was retained.
No source change or recompilation was used for the failed-only rerun.

The timer inventory and doc-budget regressions passed after their targeted
bookkeeping fixes; the other 14 paperwork checks were retained. PR #1582 landed
as `65493acc8` after exact-head fast lane 7477
(API run 7690), including Main promotion, passed. Its three regression fields
and the local Clippy/three-regression evidence are retained.
The completed learner compiler and task-owned duplicate volumes are removed;
shared cache seeds and all durable evidence remain preserved.

## Diagnostic rollout — four healthy nodes, release stage now observed

Paul owns and confirmed the full-fleet rollout. At 23:03:46 UTC all four nodes
run `483d2bcf3`, including #1573 and #1582; both server and discovery containers
are healthy. Every node reports quorum, known leader, replicated applied-view
proof, stable image and zero failed replacements. The process starts below are
container StartedAt, not ReadyAt; new-process counters are not subtracted from
an older build.

| Role | Server start UTC | Discovery start UTC |
|---|---|---|
| Learner | 22:16:53 | 22:17:29 |
| Voter A | 23:01:56 | 23:02:32 |
| Voter B | 23:01:49 | 23:02:15 |
| Leader | 22:09:22 | 22:09:58 |

The leader records five natural control 503s at 22:13:59, 22:20:27,
22:35:29, 22:53:53 and 23:41:44 UTC. All five explicitly report `control_unavailable`,
`release_pending_before_read`, and `http/hls/control.rs:1765`, taking 10–30 ms.
Serving readiness remains true and loss generation remains zero. Two sessions
explicitly settle retirement as `client_released` after the refusal, followed
by control 410. The other two already return ended/owner-lost 410s before their
503; no new release initiation is retained. Successful DELETE ingress and its
caller remain absent from these logs. The fifth session had already settled
retirement 47 ms before its 503: requested reason superseded by cluster session,
winning reason `authority_fence`; control 410 follows 539 ms later. This proves the new failures' pre-read
release-fence stage; it does not retroactively prove the original 18:44 branch.
The original concurrent-deletion timing and inspected fence-before-End source
ordering remain strong evidence with that historical attribution limit.

Two further 503s are status GETs for one rolling session at 22:41:50 and
22:41:52 UTC: `response_state_changed`, 48/33 ms, ready true and unchanged loss
generation zero. Its producer subsequently ends and requests return 410.
No create/startup or segment 5xx appears in that bounded window; the producer's
termination cause is not retained. A later rolling session at 23:31:42–43 UTC
returns status `response_state_changed` (31 ms) and playlist
`playlist_state_changed` (4 ms). Both retain readiness and loss generation zero.
Retirement then settles with requested reason file replaced and winning reason
`pause_expired`, followed by 410s. These are separate from control Unavailable.

At 22:13:01 UTC, the actual Settings → Developer UI on the new leader shows
Applied view met, proof met and stable image, with settings 12 view / 0 quorum,
routes 91 view / 0 quorum and last fallback none. Create plan is present and
shows unknown: no create has planned on that node since start. Its zero counts
are unexercised, not stale-plan/refusal-close runtime qualification. The receipt
was captured during partial rollout; the later fleet observation establishes
the full rollout, not new UI exercise. Future enhancements have separate owners.

## Final post-rollout hour reading — completed November 14

Final metrics at 00:04:00 UTC and container inspection at 00:04:17 retain all
four exact `483d2bcf3` revisions and the server/discovery starts above, running
and healthy. The 23:03:21 baseline spans 3638–3639 seconds of same-process
uptime. Health endpoints span about 60 minutes 31 seconds from the initial
23:03:46 inspection, with five-minute metric samples between them. This is
sampled health over an hour, not continuous monitoring; StartedAt is not ReadyAt.
All samples retain quorum, known leader, replicated proof, stable image and
zero failed replacements, with no counter resets.

| Role | Final server uptime seconds | Hour playback HTTP requests | New playback 5xx / control Unavailable |
|---|---:|---:|---:|
| Learner | 6396 | 0 | 0 / 0 |
| Voter A | 3693 | 0 | 0 / 0 |
| Voter B | 3711 | 0 | 0 / 0 |
| Leader | 6847 | 5124 | 3 / 1 |

The leader ends at nine cumulative playback 5xx and five control Unavailable:
five pre-read release-fence control refusals, three status
`response_state_changed` refusals and one playlist `playlist_state_changed`.
The hour's three are the later status/playlist pair and fifth control refusal
described above. Other processes end at zero for both counters. This was not
an error-free hour. Earlier `882d4b691` counts remain separate.

The leader's hour has 20696 authority and 1395 local playback reads: 4.03903
and 0.27225 per HTTP request. Route view reads are 1029 / 0 authority,
settings 838 / 0 and planning 3 / 0. Authority-only route sites are
`status_bracket` 1453, `control` 1667, `verdict` 4782, `receipt_fence` 472 and
`receipt_candidate` 798; `routing` has 1029 view / 0 authority. Remaining sites
have no reads, so their shares stay undefined. Other nodes' planning reads are
learner 3 view / 0 authority, voter A 2 / 1 (floor), voter B 3 / 0.
No other authority fallback reason occurs, including `legacy_layout`; the
conditional schema-82 principal inspection was therefore not triggered.

Top leader playback authority methods are `media_session_route` 8221,
`get_setting` 7380 and `authenticate_token_for` 1195; local route reads are 1029
and staged-session reads 832. Top cluster authority methods are `get_setting`
3363, `analysis_file_labels` 335 and `queue_sql` 32. Ratios sum HTTP requests
across method/status classes and describe uncontrolled traffic; they do not
establish a causal reduction from an earlier workload.

The actual Developer UI at 00:05:42 UTC shows Applied view met, proof met,
image generation zero/stable, settings 1261 view / 2 quorum, routes 1802 / 0
and last fallback floor. Create plan is met: seven of seven plans from replica
(100%), zero quorum and last reason none. Binding mismatch, legacy unbound,
peer catalog, stale refusal, replan and refusal-close counts remain zero.
These are process-lifetime UI counts sampled after the metrics endpoint,
not hour deltas. Seven natural plans exercise planning source; the zero
peer/refusal/replan paths remain unexercised and unqualified.

The requested rollout, longevity, sampled health and actual UI reading are
complete. Only the leader received playback ingress; idle learner/voter paths
are not qualified by these readings. The original 18:44 exact refusal stage,
older status-session termination cause and unfinished subtitle-sidecar source
read remain unresolved. No synthetic playback, new capture, settings changes,
extra tests or builds were used for this final observation. Native, queue and
future enhancement work retain their separate owners and acceptance gaps.
