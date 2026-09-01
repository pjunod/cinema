# Activity peer-read fix plan — coalesce bursts and report failures truthfully

**Status:** Implemented and focused validation complete; rollout not started ·
**Executes:** C1–C5 and the accepted decisions in §11 · **Written:** 2026-09-01 ·
**Re-pinned:** 2026-09-01 · **Code:** `origin/main` at `69d3f3de`

This is the standalone implementation plan for the intermittent **Activity is
incomplete** warning observed on `nynuc`. It treats the corrected
`make docker-up` deployment as a build-stamping repair, not as proof that the
runtime defect disappeared. The plan first turns the suspected burst path into
a deterministic regression, then makes one daemon share its clustered Activity
read, and finally gives authentication refusal and network failure different
public outcomes.

Fable approved the architecture with five required corrections. Those
corrections are folded into the controlling contracts, tests, metrics, and
operator boundary below; §11 records the accepted review answers. The current
base is 28 commits ahead of the original `1a64fa8a` plan pin. Across the files
this plan targets, that delta adds only eight unrelated lines to `index.html`;
the Activity functions and named tests are unchanged. Re-verify the exact
branch after rebasing anyway.

Read the cluster Activity contract in [OPERATIONS.md](OPERATIONS.md) before
implementation. Preserve every security and transport bound in §3. If the fix
appears to require raising an authorization rate limit, forwarding a household
bearer to another node, hiding an incomplete-read warning, or returning stale
Activity indefinitely, stop and return the design for review.

## 1. Decision requested — accept the reconciled sender-side repair

Fable approved the architecture with the corrections now incorporated. Paul
authorized implementation on 2026-09-01. No tracking pull request is created;
opening any eventual review request is a separate action after the code and
focused evidence are complete. The implementation must deliver these outcomes:

| Outcome | Required result |
|---|---|
| Burst control | Concurrent `/activity` and `/activity/detail` reads on one daemon cause one physical peer fan-out, not one fan-out per HTTP request. |
| Freshness | A completed peer result may be reused for at most one second; the existing two-second physical fan-out deadline remains. |
| Security | The receiving peer still verifies the durable per-node signature and live committed-voter authority, admits at most two authority checks per sender per second, and globally bounds cold-key lookups at four per second. |
| Failure truth | A refused signed request is not reported as an unreachable node; HTTP failure, malformed success, timeout, stale heartbeat, and transport failure remain distinguishable. |
| Compatibility | SQLite, never-joined, native `sessions`, household privacy, response byte bounds, and peer ordering retain their current contracts. |
| Evidence | A real-daemon concurrent regression fails on the pinned base and passes after the fix; web and affected-surface gates retain the result. |

This remains one independently reviewable change. It does not need an
`effort/*` integration branch unless review materially expands the scope.

### 1.1 Incident evidence and epistemic status

The current diagnosis is strong, but the first milestone must convert its last
inference into a deterministic proof.

| Evidence from 2026-09-01 | What it proves |
|---|---|
| The Activity page intermittently named `nuc3`, `nuc4`, and `m6` as unreachable together, then returned to healthy without intervention. | The failure is burst-shaped rather than a stable loss of one named host. |
| Every node answered `/readyz`; every tested node-to-node TCP path reached `/_internal/v1/activity-snapshot` in 1–3 ms; an unsigned request returned the expected `401`. | The peers and the protected routes were reachable during the investigation. It does not prove what status the failed signed requests received. |
| Fifteen pre-restart samples and fifteen post-restart samples kept 4/4 heartbeats fresh. | A stale membership heartbeat was not present in those observation windows. |
| The warning appeared in 4 of 12 browser samples over about 36 seconds before the restart. | The symptom can occur between healthy samples at the page's normal polling cadence. |
| Hiqlite logged bursts in which a completed consistent `SELECT 1` acknowledgement could not be sent. | Authority reads were being abandoned or completed after their caller disappeared. This supports, but does not by itself prove, the refusal path. |
| `make docker-up` replaced an unstamped `unknown` image with `v0.3.0-108-g1a64fa8a`; the warning has not reappeared in the short post-recreate sample. | The served build is now attributable. Restarting also reset process-local timing windows, so absence after restart is not a code fix. |

The source-level defect is already provable: valid Activity requests can exceed
the receiving peer's fixed admission window, the internal route turns that
refusal into `401`, and the caller turns every non-success status into
`unreachable`. Milestone 0 must prove that this exact chain accounts for the
concurrent public-read case before production code changes.

## 2. Current path — independent polls multiply one security check

The contracts below were verified at `1a64fa8a` and re-pinned after a targeted
diff to `origin/main` at `69d3f3de`; re-verify them on the implementation
branch after rebasing.

```text
visible browser or client
        │
        ├── GET /api/v1/activity          header pill, every 4 s
        └── GET /api/v1/activity/detail   Activity page, every 3 s
                    │
                    ▼
           peer_activity(state)
                    │
                    ▼
       PeerActivityClient::snapshots()
                    │
                    ├── read peer directory
                    └── start a fresh fan-out to every peer
                                      │
                                      ▼
                  /_internal/v1/activity-snapshot
                                      │
                         verify per-node signature
                                      │
                    admit at most 2 authority reads
                         per sender / fixed 1 s window
                                      │
                 excess valid request ──▶ HTTP 401
                                      │
                                      ▼
                      every non-2xx ──▶ `unreachable`
```

One Activity-page browser suppresses its own header-pill poll while on that
page, but the server has no process-wide owner for the read. Several visible
clients, tabs, reverse-proxy retries, or the two public endpoints can still
arrive together behind the same sending node.

One public read sends exactly one signed request to every peer. The receiving
peer admits two authority checks per sender in its fixed window, so a third
concurrent public read is refused by every peer at once. That arithmetic
explains why `nuc3`, `nuc4`, and `m6` appeared together and recovered together:
it is the signature of one sender-side burst, not three independent link
failures.

### 2.1 The conflicting contracts

| Boundary | Current behavior | Problem to correct |
|---|---|---|
| Public polling | The Activity page polls detail every three seconds; the global pill polls summary every four seconds when eligible. | Poll ownership is browser-local, so several clients can align. |
| Sender aggregation | `PeerActivityClient::snapshots` resolves the directory and fans out for every call. | There is no shared in-flight read or completed-result reuse across handlers. |
| Receiver admission | `MAX_ACTIVITY_AUTH_CHECKS_PER_SECOND` is `2`, keyed by sender node in a fixed one-second process-local window. | A legitimate sender can exhaust the receiver's budget with concurrent page reads. |
| Signature-key miss admission | `MAX_ACTIVITY_KEY_LOOKUPS_PER_SECOND` is `4` in one global fixed window and applies when the sender's public key is absent from the receiver's local verifier set. | A cold or evicted key can be refused before the per-sender authority guard, but the wire still says only `401`. |
| Authority | A valid signature is followed by a consistent proof that the sender is a live, non-removed voter. | This is intentionally expensive and fail-closed; it must not be bypassed. |
| Internal HTTP | Missing, invalid, over-budget, or authority-error requests all return `401`. | The sender can know that HTTP answered, but not the private refusal reason. |
| Caller classification | `Ok(_)`, including `401` and `503`, maps to `PeerActivityOutcome::Unreachable`. | The UI states a network fact that the response disproves. |
| Web warning | Every non-`answered` node makes Activity visibly incomplete. | The warning is correct; only its reason is currently misleading. |

The fix belongs on the sender because redundant fan-out is unnecessary work
even if the receiver admitted more of it. The receiver's admission budget is a
security boundary, not a throughput knob.

## 3. Invariants and non-goals — fix load without weakening authority

### 3.1 Required invariants

- Keep the durable per-node Ed25519 proof bound to sender, target, timestamp,
  and the legacy Activity context.
- Keep the consistent live-voter authority query on every admitted request.
  The legacy Activity route has no authority cache; do not add one that turns
  voter removal, stale heartbeat, or key rotation into a time-only decision.
- Keep `MAX_ACTIVITY_AUTH_CHECKS_PER_SECOND = 2` and its per-sender keying.
- Keep `MAX_ACTIVITY_KEY_LOOKUPS_PER_SECOND = 4` and its global key-miss
  protection. The coalescer removes redundant valid lookups; it does not make
  missing-key verification unbounded.
- Keep the common two-second deadline, eight-way peer concurrency, 64-peer
  bound, redirect refusal, exact 256 KiB response bound, and node-identity
  check.
- Keep `Cache-Control: private, no-store` on the internal response. The new
  reuse is process-local application state, not an HTTP, proxy, or browser
  cache.
- Keep household bearer tokens off the peer route. The peer read stays
  cluster-scoped and read-only.
- Keep `sessions` byte-compatible for native clients. The additive
  `activity_nodes` field remains a web/server contract not consumed by the
  current Apple or Android clients.
- Keep SQLite and never-joined installs on the current local-only path without
  constructing peer work.
- Keep the warning whenever any expected peer did not answer. The repair must
  not make incomplete data look complete.

### 3.2 Explicit non-goals

- Do not raise, remove, or dynamically tune the receiver admission limit.
- Do not cache authorization independently of the snapshot it authorized.
- Do not persist Activity snapshots or share them between daemon processes.
- Do not merge `/api/v1/activity` and `/api/v1/activity/detail`; they may reuse
  one peer result while retaining their distinct public payloads.
- Do not alter heartbeat reachability, membership, Raft, readiness, or voter
  rollout semantics.
- Do not add retry loops around `401`. Retrying a refusal inside the same
  admission window amplifies the defect.
- Do not suppress the banner, relabel every failure as a timeout, or claim that
  an HTTP `5xx` proves the host is unreachable.
- Do not treat a short clean interval after a restart as acceptance evidence.
- Do not modify the unrelated Android wrapper changes or untracked documents
  present in the current working tree.

## 4. Target contracts — one physical read and truthful typed outcomes

### 4.1 A shared gate owns every peer fan-out in one daemon

Add a process-shared read gate to `PeerActivityClient`. Every clone must share
the same state through `Arc`; constructing or cloning an HTTP handler must not
construct a second cache.

The proposed internal shape is illustrative, but the semantics are required:

```rust
const ACTIVITY_SNAPSHOT_REUSE: Duration = Duration::from_secs(1);

type SharedPeerActivity =
    Arc<[(String, PeerActivityOutcome)]>;

struct PeerActivityReadGate {
    last_started: Option<tokio::time::Instant>,
    completed: Option<(tokio::time::Instant, SharedPeerActivity)>,
}

pub struct PeerActivityClient {
    membership: MembershipManager,
    transport: PeerTransport,
    reads: Arc<tokio::sync::Mutex<PeerActivityReadGate>>,
}
```

`snapshots()` follows this contract:

1. Acquire ownership of the shared gate.
2. If an `Ok` snapshot set completed less than one second ago, return its
   `Arc` and perform no directory or network work.
3. Otherwise enforce at least one second between physical fan-out starts. Set
   `last_started` before the first await that may issue a peer request.
4. Resolve the directory and perform exactly the existing sorted, bounded
   fan-out under the existing common two-second peer deadline.
5. Store an `Ok` vector at its monotonic completion time, including any typed
   per-peer failures, and return the shared `Arc`.
6. Return a directory `MembershipError` without storing it as a completed
   snapshot. Retain the start reservation so cancellation or error cannot
   create an immediate retry storm.

Holding one async mutex across the bounded physical read is acceptable for the
first implementation: there is no recursive path, waiters need the same result,
and dropping a canceled owner releases the mutex. The `last_started`
reservation survives cancellation, so the next owner waits out the one-second
sender floor before trying again. Use this mutex design for the first
implementation. Do not substitute a detached actor unless the §5.2
cancellation test makes the mutex contract unprovable; an actor otherwise adds
a shutdown obligation without improving the accepted bound.

The bounds are explicit:

| Condition | Maximum added age or wait |
|---|---:|
| Reused completed snapshot | Less than 1 s after completion |
| Physical peer fan-out | Existing common 2 s deadline |
| Normal concurrent waiter | Remaining time of the in-flight fan-out |
| Retry after a canceled owner | Less than 1 s sender floor, then the existing 2 s fan-out |
| Stored memory | One bounded sorted result set per daemon; use shared ownership rather than cloning up to 64 bounded responses per caller |

One-second completed reuse is deliberately shorter than the three-second
Activity-page poll. Under normal polling every new page refresh can obtain a
new sample, while concurrent consumers of that refresh share the work. The
start floor bounds receiver load; the completed-result TTL bounds staleness.
Steady demand starts at most one fan-out per second, while receiver-window
misalignment can place at most two starts in one fixed window — exactly the
unchanged budget, never more.

### 4.2 Public status distinguishes transport, HTTP, and payload failure

Extend the internal outcome enum and the additive `activity_nodes[].status`
projection with `refused` and `http_error`.

| Status | Exact meaning | Web text |
|---|---|---|
| `answered` | A bounded `2xx` payload decoded and named the expected node. | No warning entry. |
| `unhealthy` | The peer directory's heartbeat projection was stale, so no HTTP call was made. | `unhealthy` |
| `unreachable` | DNS, connection, TLS, or socket transport failed before an HTTP response. | `unreachable` |
| `timed_out` | The common peer deadline expired. | `timed out` |
| `refused` | The protected route answered `401` or `403`. | `refused the activity request` |
| `http_error` | The peer or an intermediary answered another non-success HTTP status. | `returned an HTTP error` |
| `invalid_response` | A nominally successful response violated the byte, decoding, redirect, or expected-node contract. | `sent an invalid response` |
| `unavailable` | The sending node could not obtain the cluster peer directory; this remains the node-less pseudo-entry. | `directory unavailable` |

Do not put raw status codes, peer URLs, hostnames, signatures, or authority
details into household-visible JSON. The two new stable strings are enough for
the UI to tell the truth. The banner still leads with **Activity is
incomplete** and still warns that streams may be missing.

### 4.3 Low-cardinality metrics make recurrence diagnosable

Add process-lifetime counters to the existing unauthenticated, count-only
`/metrics` exposition:

```text
plurx_cluster_activity_aggregations_total{path="fanout"}
plurx_cluster_activity_aggregations_total{path="reuse"}
plurx_cluster_activity_aggregations_total{path="directory_error"}
plurx_cluster_activity_peer_outcomes_total{outcome="answered"}
plurx_cluster_activity_peer_outcomes_total{outcome="unhealthy"}
plurx_cluster_activity_peer_outcomes_total{outcome="unreachable"}
plurx_cluster_activity_peer_outcomes_total{outcome="timed_out"}
plurx_cluster_activity_peer_outcomes_total{outcome="refused"}
plurx_cluster_activity_peer_outcomes_total{outcome="http_error"}
plurx_cluster_activity_peer_outcomes_total{outcome="invalid_response"}
plurx_cluster_activity_auth_admission_refusals_total
plurx_cluster_activity_key_lookup_refusals_total
```

`peer_outcomes_total` increments only for a physical fan-out, not every reuse,
so it measures wire outcomes rather than browser demand. The admission counter
increments only when a cryptographically valid sender reaches the two-per-
second guard and is declined. It must not carry a sender, target, URL, user,
file, item, or session label. The key-lookup counter increments when the global
four-per-second cold-key guard refuses work before signature verification; it
is separate because that request never reaches the per-sender authority guard.

Document the receiver-side interpretation in [OPERATIONS.md](OPERATIONS.md):

| Page shows `refused` | Receiver counter movement | Interpretation |
|---|---|---|
| Yes | `auth_admission_refusals_total` rises | A verified sender exceeded the per-sender authority-check window. |
| Yes | `key_lookup_refusals_total` rises | Cold or evicted signature-key lookup pressure reached the global guard; this may be valid churn or forged miss traffic. |
| Yes | Neither rises | Check clock skew against the 30-second proof window, sender key validity, committed live-voter authority, and receiver errors. The protected route deliberately does not reveal which condition failed. |

The metrics path performs no Store read and acquires no async Activity lock.
Saturating atomics and a fixed label vocabulary follow the existing metrics
style. A healthy, normally polled cluster should show reuse under concurrent
demand and no continuing admission-refusal increase.

## 5. Regression design — fail on the current tree before correcting it

### 5.1 Real-daemon burst proves the root cause and the repair

Extend [`cluster_activity.rs`](../crates/plurxd/tests/cluster_activity.rs) and
its existing two-daemon HTTP proxy. Add a proxy request counter and a barrier
that releases eight authenticated `GET /api/v1/activity/detail` requests
against node A at once. The proxy also needs a bounded collection gate: hold
internal requests until all eight arrive or a short timeout expires, then
forward the collected requests together. The timeout lets the corrected
single request proceed instead of waiting forever, while the gate keeps the
pre-fix receiver checks inside one real admission window. Give that collection
timeout at least 500 ms; real-daemon timing assertions must not sit directly on
a scheduler boundary.

Before the burst, wait at least 1.5 seconds so setup reads cannot consume either
fixed one-second window. Node B must own a visible delivery. Then require:

- all eight public responses are successful JSON;
- all eight report node B as `answered`;
- all eight contain the same node-B delivery;
- the proxy observes exactly one new internal Activity request for the wave;
- an immediate `/api/v1/activity` and `/api/v1/activity/detail` pair reuses the
  same completed peer set without another internal call; and
- a call at least 1.5 seconds after completion clears both unaligned windows,
  starts exactly one new fan-out, and again reports `answered`.

On the pinned base, the proxy count will exceed one and the receiving daemon's
two-per-second admission will refuse part of the wave. That is the required
pre-fix failure. Do not weaken the assertion to “at most two answered”; the
purpose is to retain the corrected behavior.

**Cold-key variant:** run the same eight-caller wave through a receiver whose
local `activity_public_keys` verifier set is deliberately empty. Use an
in-process membership fixture or a `cluster-integration-tests`-only hook; do
not add a production route, setting, or environment control for clearing
security state. The corrected wave must create one internal request, consume
one allowed key lookup, return `answered` to all callers, and increment neither
refusal counter.

In a focused companion case, consume the four global key-lookup admissions
before that cold-key request. The protected route must return `401`, the sender
must publish `refused`, `key_lookup_refusals_total` must increase, and
`auth_admission_refusals_total` must not. This pins what the page and counters
mean even though the wire intentionally hides the private refusal reason.

### 5.2 Cancellation and freshness belong in focused unit tests

Factor the gate so a fake fan-out closure can prove:

- eight simultaneous callers execute the closure once and receive one shared
  result;
- a completed result is reused inside one second and refreshed outside it;
- a directory error is returned and is not installed as a snapshot;
- canceling the leading caller releases the gate, retains the start floor, and
  lets a follower complete within the documented three-second worst case; and
- the one-second age and start calculations use Tokio's monotonic clock, with
  paused-time tests rather than wall-clock sleeps.

If the mutex cancellation case cannot be made deterministic, stop and return
that evidence for review before replacing the accepted design with an actor.

### 5.3 The proxy pins every failure classification

Extend the real-daemon proxy modes and focused transport tests so that:

- `401` and `403` produce `refused`, never `unreachable`;
- `503` produces `http_error`, never `unreachable`;
- a stopped listener or unused address produces `unreachable`;
- the existing ten-second hang returns `timed_out` inside the common bound; and
- malformed, oversized, redirected, or wrong-node `2xx` responses remain
  `invalid_response`.

The current `UNREACHABLE` proxy mode returns HTTP `503`; rename it as part of
the test correction rather than preserving a misleading test name.

### 5.4 Web and API fixtures retain the user-facing contract

Update [`activity-node-names.test.js`](../tests/web/activity-node-names.test.js)
to paint the exact `refused` and `http_error` text with a hostname and fallback
node id. Update
[`page-read-budget.test.js`](../tests/web/page-read-budget.test.js) to retain
one detail poll, one banner, generation fencing, and no second client-side
retry loop.

Handler tests must retain:

- the exact existing local-only response shape;
- native `sessions` without `node_id` additions;
- admin-only `node_hostnames` and hostname absence for household readers;
- one concurrent admin detail read and one household detail read reuse exactly
  one cached peer wave while retaining different projections: the admin
  response contains `node_hostnames` even when the map is empty, the household
  response omits the field, and both see the same permitted deliveries;
- one status row per expected peer in stable node-id order; and
- the new stable status strings without internal HTTP details.

The shared object contains only peer Activity outcomes. Hostnames continue to
come from the per-request membership projection outside that object. The
admin/household assertion is the privacy fence that prevents a later refactor
from moving the admin projection into the reusable wave.

## 6. Milestones — one pull request with reviewable proof boundaries

Before the first Rust edit, establish the repository's required compiler loop:

```bash
rustc --version                 # must report the pinned Rust 1.97.1 toolchain
```

If the checkout host cannot run that toolchain, use the source-only cloud loop
in [DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md#4-compile-before-ci-send-source-to-the-compiler-never-credentials).
Archive committed source with `git archive`, transfer no `.git` directory or
credential, keep the remote `target/` warm, and rerun the loop against the
exact rebased branch before pushing. CI is not the first compiler.

### 6.1 Milestone 0: land the failing reproducer in the branch

**Files:**
[`cluster_activity.rs`](../crates/plurxd/tests/cluster_activity.rs) and focused
test helpers under
[`internal_activity.rs`](../crates/plurxd/src/http/internal_activity.rs)

Add the eight-request wave, proxy count, 1.5-second rate-window settling,
500-millisecond collection margin, cold-key variant, and status fixtures.
Record the exact assertion that fails when the production correction is
absent.

**Acceptance:** reverting the later coalescer causes the burst case to fail
because the proxy sees more than one internal call and at least one response
does not report the remote node as answered.

### 6.2 Milestone 1: add the bounded shared read gate

**Files:**
[`internal_activity.rs`](../crates/plurxd/src/http/internal_activity.rs),
[`state.rs`](../crates/plurxd/src/state.rs), and
[`system.rs`](../crates/plurxd/src/http/system.rs)

Add `Arc<[(String, PeerActivityOutcome)]>` ownership inside the existing
`PeerActivityRead::Peers` arm, one-second completed reuse, one-second start
spacing, and cancellation-safe mutex serialization. Cached bounded responses
must not be deep-cloned for each public request.

**Acceptance:** every §5.1 and §5.2 assertion passes; the physical fan-out
still sorts peers, starts no more than eight at once, and shares one two-second
deadline.

### 6.3 Milestone 2: make failure states truthful and observable

**Files:**
[`internal_activity.rs`](../crates/plurxd/src/http/internal_activity.rs),
[`system.rs`](../crates/plurxd/src/http/system.rs),
[`membership.rs`](../crates/plurx-core/src/cluster/membership.rs),
[`index.html`](../crates/plurxd/src/web/index.html), the §5 tests,
[`OPERATIONS.md`](OPERATIONS.md), and
[`points.toml`](../validation/points.toml)

Add `Refused` and `HttpError`, map them to stable public strings, update the web
copy, publish the four fixed-vocabulary counter families, and document how an
operator distinguishes reachability, per-sender authority admission, global
key-lookup admission, and other refusal. Extend the `cluster.membership`,
`cluster.page-reads`, and `web.experience` contracts rather than creating a
duplicate functionality point.

**Acceptance:** every §5.3 and §5.4 case passes; `/metrics` contains only the
documented labels and its scrape performs no Store or peer read.

### 6.4 Milestone 3: retain evidence and qualify the exact tree

After the corrective commit has a stable hash, add
`validation/regressions.d/<commit>-activity-peer-read.toml` mapping it to:

- `cluster.membership` and `cluster.page-reads` through `cluster-auth`; and
- `web.experience` through `web-static`.

Run the affected-surface and focused commands in §7 on the exact proposed
tree. Do not stage or overwrite unrelated working-tree files to manufacture the
selection.

**Acceptance:** the validation plan selects the three intended functionality
points, history audit finds the retained regression receipt, and every selected
check passes.

## 7. Validation matrix — each claim has one retained owner

| Claim | Primary evidence | Focused command |
|---|---|---|
| One concurrent wave makes one physical call | Real two-daemon burst and proxy counter | `cargo test --locked -p plurxd --features cluster-integration-tests --test cluster_activity -- --nocapture` |
| Cache age, start floor, and cancellation are bounded | Paused-time gate unit tests | `cargo test --locked -p plurxd internal_activity -- --nocapture` |
| A cold key uses one lookup; saturated lookup admission is a distinct refusal counter | Membership key-cache and coalesced cold-key fixtures | `cargo test --locked -p plurx-core activity_key -- --nocapture` |
| Signature, live-voter authority, byte, redirect, and deadline bounds survive | Existing cluster Activity and peer-transport cases plus new classification cases | `make cluster-daemon-check` |
| One shared wave cannot leak the admin hostname projection | Concurrent admin/household real-daemon response comparison | `cargo test --locked -p plurxd --features cluster-integration-tests --test cluster_activity -- --nocapture` |
| Refusal and HTTP error render truthfully | Activity node-name fixture | `node tests/web/activity-node-names.test.js` |
| Poll ownership and page failure behavior do not regress | Page read-budget fixture | `node tests/web/page-read-budget.test.js` |
| Embedded JS remains valid and themes remain readable | Complete static web gate | `make web-check` |
| Rust, history, operations, catalog, and benchmark baseline passes | Repository baseline | `make check` |
| Path selection covers the intended contracts | Staged validation explanation | `make validate-plan` |
| All selected task surfaces pass | Ordinary task gate | `make validate-staged` |
| Patch has no malformed whitespace | Git check | `git diff --check` |

The pull-request description records the focused regression command and the
failing-with-revert observation. A green unit test without the real-daemon
burst is insufficient because the defect crosses public HTTP, the shared
client, signed peer HTTP, and the receiving authority limiter.

## 8. Operator-owned rollout — the implementation session stops at merge

This section belongs to Paul or his designated fleet executor. The implementing
session finishes after Milestone 3 is reviewed and merged; it reports what is
ready to deploy and does not run fleet commands, recreate containers, or infer
deployment authority from this plan.

Roll out one voter at a time through the supported procedure in
[OPERATIONS.md](OPERATIONS.md). Use an exact image digest or `make docker-up`
from the intended checkout so the daemon reports its source build. Plain
`docker compose build` is not acceptance evidence because it does not provide
the repository's build stamp.

For each voter:

1. Record the old build identity and current counter values.
2. Replace only that voter and require `/readyz` success before advancing.
3. Confirm the roster, leader, and apply-lag facts remain healthy.
4. Confirm the new build identity on the same backend.
5. Leave the other voters running; do not recreate all fixed admission windows
   together.

During this mixed-build interval, record both refusal counters but do not use
their movement alone as a rollback trigger. An unupgraded sender can still
burst into an already-upgraded receiver, so a mid-rollout increase is expected
evidence of old behavior, not evidence that the new coalescer failed. Readiness,
payload correctness, and cluster health remain blocking at every step.

After the last voter runs the exact accepted build, wait at least 1.5 seconds
for both fixed windows to clear. Only then snapshot the counters, keep Activity
visible on one backend while at least two other signed-in clients keep ordinary
pages visible for two minutes, and require:

- every expected node remains `answered` across 40 three-second samples;
- `plurx_cluster_activity_aggregations_total{path="reuse"}` increases when
  clients overlap;
- `plurx_cluster_activity_auth_admission_refusals_total` does not increase;
- `plurx_cluster_activity_key_lookup_refusals_total` does not increase;
- no peer is labeled unreachable while its internal route returned HTTP; and
- the build identity remains the accepted commit on every node.

Perform transport-negative classification in the real-daemon harness, not by
cutting production network access. The retained harness must still show one
named node as `unreachable` when its listener is genuinely unavailable and
must clear that warning after restoration.

Rollback is a serial rollout of the preceding exact image. It changes no
schema or membership and deletes no cached state. Trigger rollback if
readiness fails, peer-read latency exceeds the §4 bound, Activity omits an
answered delivery, either refusal counter continues rising under normal polling
after the last upgrade and 1.5-second settle, or native/local-only payload
compatibility changes.

## 9. Risks and trade-offs

| Risk | Why it matters | Mitigation or rollback trigger |
|---|---|---|
| One-second reuse briefly shows an ended or new stream late | Activity is a live operational view. | The age is monotonic, process-local, shorter than the page poll, and visible data already refreshes on a three-second cadence. |
| A canceled gate owner delays the next physical read | The start floor can add up to one second before the existing deadline. | Retained paused-time test and explicit three-second worst-case bound; stop for design review if the accepted mutex contract cannot meet it. |
| Sharing a failed peer vector repeats a transient warning for one second | Typed failures are cached with the completed wave. | Keep the same one-second bound; do not selectively retry or hide failure. |
| New status strings surprise a strict consumer | `activity_nodes` is additive and not consumed by current native clients, but external users may inspect it. | Document stable strings, retain old strings, and treat additions as API-compatible typed outcomes. |
| One `401` covers several private refusal reasons | The page cannot name the exact receiver guard from the wire response alone. | Keep `refused` intentionally broad; correlate the two receiver counters, clock state, voter authority, and logs as specified in §4.3. |
| Metrics accidentally expose fleet identity | Peer labels would reveal node cardinality or names. | Fixed vocabularies only; tests reject node, URL, user, and media labels. |
| Coalescing hides abusive browser demand | Fewer peer requests could make demand invisible. | `aggregations_total{path="reuse"}` retains demand count without charging authority or network work. |
| The leading diagnosis is wrong | Healthy live sampling does not capture the exact failed response. | Milestone 0 must fail on the current tree through the receiver's real admission guard before production code changes are accepted. |

## 10. Definition of done — the page is complete when peers answer, and honest when they do not

The implementation is ready to merge only when all of the following are true:

- the deterministic eight-request case fails on the pinned behavior and passes
  with exactly one physical peer request after the correction;
- a valid concurrent wave never consumes more than one authority check per
  receiving peer from its sending daemon;
- a cold-key wave performs one allowed lookup, while a saturated cold-key
  lookup produces `refused` and moves only its dedicated refusal counter;
- the two-per-second authority limit, four-per-second key-lookup limit, and
  live consistent authority proof are unchanged;
- completed reuse is less than one second, process-local, non-persistent, and
  cancellation-bounded;
- `unreachable` is emitted only for a transport failure, while `401`/`403`,
  other HTTP failure, timeout, stale heartbeat, and malformed success retain
  distinct typed outcomes;
- Activity continues to warn and omit rows whenever a peer did not answer;
- one shared wave preserves the admin-present and household-absent
  `node_hostnames` projection without changing permitted deliveries;
- local-only and native response contracts are byte-compatible where promised;
- counters make fan-out, reuse, peer outcome, valid-sender authority refusal,
  and global cold-key refusal distinguishable without high-cardinality labels;
- Rust 1.97.1 check, Clippy, formatting, and focused tests pass on the exact
  rebased branch through the local or source-only compiler loop; and
- the exact affected-surface validation and corrective-history receipt pass.

The implementing session then stops and reports the local implementation and
validation result. Incident
closure additionally requires Paul or his designated executor to complete the
serial exact-build rollout and the post-last-upgrade two-minute observation in
§8 with neither refusal counter moving and no false unreachable warning.

### 10.1 Local implementation evidence — 2026-09-01

| Check | Result |
|---|---|
| Rust 1.97.1 source-only `cargo check --locked -p plurxd --all-targets` | Pass |
| Rust 1.97.1 source-only Clippy for all daemon targets with `cluster-integration-tests` and `-D warnings` | Pass |
| Focused `plurx-core` Activity/authentication tests | 7 passed |
| Focused `plurxd` internal Activity tests | 14 passed |
| `/metrics` no-Store/no-manager contract | Pass |
| Real two-daemon `cluster_activity` regression with ffmpeg | 2 passed |
| `activity-node-names.test.js` and `page-read-budget.test.js` | Pass |
| Broad `make web-check` | Activity contracts passed; unrelated playback-policy harness stopped later with `PLAYER is not defined` under local Node v26 |

No pull request, push, merge, or fleet rollout was performed while producing
this evidence.

## 11. Review decisions — Fable approved the bounded mutex design

1. **The root-cause evidence is sufficient.** Do not add a temporary
   diagnostic branch; Milestone 0's real-daemon failure closes the final link
   before the production correction is accepted.
2. **Keep both one-second constants.** The start floor bounds receiver load;
   the completed-result TTL bounds staleness. Misaligned windows still admit
   no more than the receiver's existing two checks.
3. **Use the mutex.** Drop-release plus the surviving start reservation is a
   complete cancellation contract within the accepted three-second worst
   case. An actor adds lifecycle work without adding correctness.
4. **Add both `refused` and `http_error`.** Collapsing `5xx` into refusal would
   retain a smaller version of the reachability lie this work removes.
5. **Keep the proposed metrics and add the key-lookup refusal counter.** This
   closes the global cold-key path without adding peer labels or Store work.
6. **Put an `Arc` slice inside `PeerActivityRead::Peers`.** Existing consumers
   iterate the result, so a separate named wrapper is optional rather than a
   contract.
7. **Use one ordinary PR.** The coalescer, truthful vocabulary, tests, metrics,
   and docs form one rollback boundary; no `effort/*` branch is warranted.
8. **The missing cases are now explicit.** Cold-key admission (§5.1), shared
   admin/household projection (§5.4), 500-millisecond real-clock margins
   (§5.1), and post-last-upgrade counter acceptance (§8) are controlling.

## 12. Source map — implementation anchors and controlling contracts

| Area | Repository source |
|---|---|
| Peer snapshot transport, outcomes, bounds, and client | [`internal_activity.rs`](../crates/plurxd/src/http/internal_activity.rs) |
| Public Activity aggregation and metrics exposition | [`system.rs`](../crates/plurxd/src/http/system.rs) |
| Shared client construction | [`state.rs`](../crates/plurxd/src/state.rs) |
| Signature, live-voter proof, and admission window | [`membership.rs`](../crates/plurx-core/src/cluster/membership.rs) |
| Browser polling, banner, and status text | [`index.html`](../crates/plurxd/src/web/index.html) |
| Real two-daemon proof | [`cluster_activity.rs`](../crates/plurxd/tests/cluster_activity.rs) |
| Hostname and failure-copy fixture | [`activity-node-names.test.js`](../tests/web/activity-node-names.test.js) |
| Browser request ownership | [`page-read-budget.test.js`](../tests/web/page-read-budget.test.js) |
| Operational Activity and rollout contract | [`OPERATIONS.md`](OPERATIONS.md) |
| Functionality points and checks | [`points.toml`](../validation/points.toml) |
| Contributor merge policy | [`DEVELOPMENT_PIPELINE.md`](DEVELOPMENT_PIPELINE.md) |

## 13. Decision ledger — Paul records final acceptance

| Decision | Fable recommendation | Paul decision | Date |
|---|---|---|---|
| Root-cause evidence sufficient | Sufficient; no diagnostic branch; M0 remains fails-first | Accepted; implement | 2026-09-01 |
| One-second TTL and start floor | Approve both; floor bounds receiver load | Accepted | 2026-09-01 |
| Mutex owner or detached actor | Mutex | Accepted | 2026-09-01 |
| Add `http_error` with `refused` | Add both | Accepted | 2026-09-01 |
| Metrics vocabulary | Approve plus key-lookup refusal counter | Accepted | 2026-09-01 |
| Shared result representation | `Arc` slice inside existing `PeerActivityRead::Peers` | Accepted | 2026-09-01 |
| One PR or two | One ordinary PR | No tracking PR; any later review PR is created only after complete evidence | 2026-09-01 |
| Missing negative or rollout case | C1 cold key · C3 privacy · C4 timing · C5 operator boundary, all incorporated | Accepted; rollout remains operator-only | 2026-09-01 |
