# Clock-skew guard — measure the offset, bound it, and refuse the dangerous side

**Status:** design-only draft; runtime work not started · **Executes:** S9 / F-sc-10 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 · **Revised:** 2026-09-21 against `main` @
`9deb58a2`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Read §2 first: every wall-clock comparison that decides ownership is listed
there with its current line. Then §3, which is a design, not a diff — it
fixes the measurement, the uncertainty, the three states an observation can
be in, and exactly which decisions consult it. This plan PR ends at that
design boundary. Section 5 is the executable contract for later runtime work,
not permission to implement it here. If a later step seems to require putting
a clock inside a replicated statement, deriving an offset from
`cluster_nodes.last_seen_at`, terminating the process on a bad clock, adding
an enablement switch, or making `/readyz` do a Store round trip, stop and flag
it — each is explicitly refused in §4 and each has a reason.

**Correction to the review:** four, all in plurx's favour.

1. The line numbers moved. S9 cites `media_sessions.rs:3888-3899, 4384-4388`;
   on `0f02b7ea` the renewal clock is
   [media_sessions.rs:3895-3906](../../crates/plurxd/src/media_sessions.rs)
   and the takeover clock is
   [media_sessions.rs:4390-4396](../../crates/plurxd/src/media_sessions.rs).
   Same code, shifted. Re-verify at build time.
2. **Half of the timestamp exchange already exists on the wire.** Every
   signed peer request carries `x-plurx-cluster-time-ms`
   ([peer_transport.rs:15](../../crates/plurxd/src/http/peer_transport.rs)),
   signed into the authentication message
   ([membership.rs:10307-10335](../../crates/plurx-core/src/cluster/membership.rs)).
   That is `t1`, authenticated, today, but `PeerTransport::request` does not
   return the value to its caller. The response proof already authenticates
   `(status, body)` and binds it to the request nonce and route
   ([peer_transport.rs:156-217](../../crates/plurxd/src/http/peer_transport.rs));
   putting `t2` and `t3` in the JSON body therefore needs no new signature
   format. The missing pieces are a clock route and a transport result that
   exposes the exact signed `t1` and the receive-side `t4`.
3. **The code already assumes a two-second bound and never checks it.**
   `RELAY_DEADLINE_CLOCK_SKEW_ALLOWANCE = 2 s`
   ([media_sessions.rs:112-116](../../crates/plurxd/src/media_sessions.rs))
   is a one-sided allowance added to a relayed deadline. The number S9 asks
   us to enforce is therefore already load-bearing in one place and
   unverified in all of them.
4. **The wire has two freshness windows, and the five-second one is already
   load-bearing.** General exact peer requests use
   `ACTIVITY_AUTH_WINDOW_MS = 30_000`
   ([membership.rs:961, 6355](../../crates/plurx-core/src/cluster/membership.rs));
   high-volume idempotent internal reads use the narrower
   `INTERNAL_READ_AUTH_WINDOW_MS = 5_000`
   ([membership.rs:1009, 6564](../../crates/plurx-core/src/cluster/membership.rs)).
   A node five seconds out already loses exact peer reads, while the 30-second
   control path can still carry the probe. The clock route deliberately uses
   the existing 30-second exact-request authorizer, never widens either
   window, and reports a failed/expired exchange as `Unknown` rather than
   pretending it measured zero skew.

## 1. Objective

Board id **K-06**. A node that steps its clock must not be able to steal
every session in the fleet, and an operator must be able to see the offset
before it does. Concretely: each node continuously measures its clock offset
against each committed peer with an explicit uncertainty; exports it; and
refuses session takeover, expiry-based recovery and membership change when
the measurement — or the absence of one — cannot rule out an offset above a
stated bound. Renewals, self-fencing and serving are untouched, because a
node with a bad clock must fail closed on what it *takes*, not on what it
already owns.

## 2. Contract today

Re-verify every line below at build time.

### 2.1 The three wall clocks that decide ownership

```rust
// crates/plurxd/src/media_sessions.rs:3150
pub(crate) fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}
```

**Renewal** — the owner decides whether its own lease still has room, on its
own clock:

```rust
// crates/plurxd/src/media_sessions.rs:3895-3906
let renewal_now_ms = unix_ms();
let renewals = chunk
    .iter()
    .filter(|route| {
        route.lease_expires_at_ms
            > renewal_now_ms.saturating_add(LEASE_RENEWAL_MIN_REMAINING_MS)
    })
```

**Takeover** — the *acting* node decides which of somebody else's leases has
expired, on its own clock:

```rust
// crates/plurxd/src/media_sessions.rs:4390-4396
let now_ms = unix_ms();
let routes = match tokio::time::timeout(
    Duration::from_secs(3),
    state.store.expired_media_sessions(now_ms, scan_cursor.clone(), TAKEOVER_BATCH),
)
```

**Atomic publication** — the SQLite backend compares the replacement lease
against the executing process's clock:

```rust
// crates/plurx-core/src/store/sqlite/mod.rs:1607-1619
let execution_time_ms = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_err(|error| {
        StoreError::Task(format!("system clock precedes unix epoch: {error}"))
    })?
    .as_millis()
    .min(i64::MAX as u128) as i64;
let replacement_valid = replacement.resource == lease.resource
    && …
    && replacement.expires_at_unix_ms > execution_time_ms;
```

The relevant constants ([media_sessions.rs:89-95, 116, 133-134](../../crates/plurxd/src/media_sessions.rs)):
`LEASE_INTERVAL = 3 s`, `LEASE_TTL_MS = 12_000`,
`TAKEOVER_CLAIM_LEASE_TTL_MS = 24_000`,
`RELAY_DEADLINE_CLOCK_SKEW_ALLOWANCE = 2 s`,
`LEASE_RENEWAL_DEADLINE = 4 s`, `LEASE_RENEWAL_MIN_REMAINING_MS = 4_000`.

### 2.2 Why heartbeat age cannot see a two-second offset

```rust
// crates/plurx-core/src/cluster/membership.rs:72
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
// :57
const NODE_REACHABLE_WINDOW_MS: i64 = 30_000;
```

`commit_heartbeat` takes `let now = unix_ms()?`
([membership.rs:4307](../../crates/plurx-core/src/cluster/membership.rs))
and binds it as `$5` of

```sql
-- crates/plurx-core/src/cluster/membership.rs:83-91
INSERT INTO cluster_nodes
  (node_id, raft_id, raft_address, api_address, last_seen_at, removed_at, role)
  VALUES ($1, $2, $3, $4, $5, NULL, $6) …
```

A reader computing `local_now − peer.last_seen_at` measures

```
  age = (true offset)  +  (time since that peer last wrote)  +  (apply lag)
                          \__________________________________________/
                                    0 … 10 s, unobservable
```

The second term is uniform over a ten-second cadence and is not separable
from the first from a single sample. A two-second threshold on that quantity
rejects every healthy node whose last heartbeat is more than two seconds old
— which, at a ten-second cadence, is most of them, most of the time. This is
assessment correction 3 and it is why the review's first remedy was
withdrawn. Nothing in this plan reads `last_seen_at` as a clock signal.

### 2.3 Why a clock inside replicated SQL is replayed per voter

```rust
// crates/plurx-core/src/store/replicated.rs:1-5
//! Guardrails shared by every replicated-store slice.
//!
//! hiqlite replays SQL and bound parameters on each voter. Values that depend
//! on the executing connection, wall clock, or random generator therefore do
//! not belong in that dialect: the leader computes them once and binds them.
```

`FORBIDDEN_IDENTIFIERS`
([replicated.rs:879-897](../../crates/plurx-core/src/store/replicated.rs))
already contains `now`, `unixepoch`, `strftime`, `datetime`, `julianday`,
`random` and nine more, and `validate_sql`
([hiqlite.rs:4140-4145](../../crates/plurx-core/src/store/hiqlite.rs)) runs
it before every replicated statement. The review's second withdrawn remedy —
`unixepoch('subsec')` as a column default in the state machine — is refused
by the code that already exists. This is assessment correction 12. The rule
this plan adds is only the positive half: a leader-assigned time is computed
once and bound as a parameter (§3.7).

### 2.4 What is already on the wire, and what is not

```rust
// crates/plurxd/src/http/peer_transport.rs:13-18
pub(crate) const NODE_HEADER: &str = "x-plurx-cluster-node";
pub(crate) const TARGET_HEADER: &str = "x-plurx-cluster-target";
pub(crate) const TIMESTAMP_HEADER: &str = "x-plurx-cluster-time-ms";
pub(crate) const NONCE_HEADER: &str = "x-plurx-cluster-nonce";
pub(crate) const SIGNATURE_HEADER: &str = "x-plurx-cluster-signature";
pub(crate) const RESPONSE_SIGNATURE_HEADER: &str = "x-plurx-response-signature";

// :303-308
pub(crate) fn signed_response_payload(status: u16, body: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(2 + body.len());
    payload.extend_from_slice(&status.to_be_bytes());
    payload.extend_from_slice(body);
    payload
}
```

Request timestamps are signed
([membership.rs:6320-6338](../../crates/plurx-core/src/cluster/membership.rs)).
`authorize_internal_peer_request` accepts them for 30 seconds and keeps each
nonce for that whole monotonic replay window; the separate
`authorize_internal_peer_read_request` accepts high-volume reads for five
seconds and has its own larger replay cache. The same header therefore has
different freshness semantics according to the route's authorizer. The clock
probe uses the 30-second exact-request path because a five-second authorizer
would become unavailable only three seconds beyond the guard's proposed
two-second refusal threshold.

The response proof already covers the body and is additionally bound to the
request nonce and path by `sign_internal_peer_response`; timestamps in that
body are authenticated without changing `signed_response_payload`. What the
transport lacks is timing metadata in its return value: it currently captures
the signed request time internally and returns only status and bytes.

### 2.5 The only defence today

```
docs/OPERATIONS.md:2141-2150 — "Synchronize clocks before cluster work.
All voters and the external load generator must run NTP/chrony … Treat an
absolute offset above 250 ms, loss of synchronization, or an offset outside
the bound recorded in the benchmark artifact as a go/no-go failure for
membership changes, failure drills, or performance runs."
```

Prose, addressed to an operator, for benchmark runs. Nothing in the daemon
reads it.

### 2.6 Readiness, as it stands

```rust
// crates/plurxd/src/http/mod.rs:1026-1049 (elided)
pub(crate) async fn evaluate_readiness(state: &AppState) -> ReadinessEvaluation {
    if state.membership.local_maintenance_active() { … Maintenance … }
    // A fresh quorum watermark is already a recent replicated-store proof.
    // Do not turn readiness into another multi-second Store request exactly
    // when an isolated node needs to self-fence promptly.
    if state.serving.is_quorum_managed() { … QuorumUnavailable … }
    match state.store.ping().await { … }
}
```

Readiness is deliberately cheap. Any clock input to it must be a local read
of an already-taken observation.

## 3. Change

### 3.1 The measurement

One bounded round trip per peer, on the existing signed transport, with four
timestamps. `t1`/`t4` are read on the prober, `t2`/`t3` on the responder.

```
prober (node A)                               responder (node B)
     |                                              |
 t1  |  GET /_internal/v1/clock  (t1 in the         |
     |      signed x-plurx-cluster-time-ms)         |
     | -------------------------------------------> |  t2 = B's clock on
     |                                              |       receipt
     |                                              |  t3 = B's clock just
     |  200 { t2, t3 }, signature over               |       before writing
     |  (status, body, request nonce)               |
     | <------------------------------------------- |
 t4  |  captured immediately after the bounded      |
     |  response bytes arrive, before signature     |
     |  verification or membership lookup           |
     v                                              v

 offset      = ((t2 - t1) + (t3 - t4)) / 2       // B's clock minus A's
 round_trip  =  (t4 - t1) - (t3 - t2)            // network time only
 uncertainty = round_trip / 2 + 1 ms             // exact half-ms + quantization
```

The uncertainty term is the whole point and is the part the review's first
remedy lacked. One-way delays are non-negative, so the true offset lies in
`[offset − uncertainty, offset + uncertainty]` whatever the asymmetry.
The extra millisecond covers integer division and the four millisecond-grained
wall-clock samples. This is a conservative interval, not a confidence band.
A half-millisecond result is representable: subtract timestamps before
scaling, then store `offset_us = numerator * 500` and
`uncertainty_us = round_trip_ms * 500 + 1_000`. Saturating checked arithmetic
turns overflow into `Unknown`; it never wraps an epoch-sized input into a safe
offset.
A sample with `t3 < t2`, `round_trip < 0`, or
`round_trip > CLOCK_PROBE_MAX_RTT_MS` (2 000, the existing peer deadline in
[internal_activity.rs:29](../../crates/plurxd/src/http/internal_activity.rs))
is unusable and makes that peer `Unknown` for the current round.

`PeerTransport` must return the exact `timestamp_ms` it put in
`x-plurx-cluster-time-ms`; taking another wall-clock reading outside the
transport is not `t1`. It also captures `t4` immediately after the bounded
body is read and before it awaits response-signature or live-membership
verification, because local verification latency is not network delay. A
failed signature or membership proof discards all four timestamps. The
specialized return value is transport metadata, not a second timestamp header
or a new signature format.

NTP's clock filter is adopted for the same reason NTP has it: the minimum-
delay sample in a window is the least contaminated by queueing. Keep the last
`CLOCK_FILTER_DEPTH = 8` usable samples per peer (80 s at a 10 s cadence) and
report the one with the smallest `round_trip`, together with its uncertainty.
A candidate whose `round_trip` exceeds four times the window minimum is
unusable for that round rather than silently leaving an old sample authoritative.

A minimum-delay window must not hide a clock step for 80 seconds. Before a
usable candidate enters the window, compare its conservative interval with
the selected sample's interval. If the intervals do not intersect, clear the
window and make the candidate its first sample. Stable clocks under asymmetric
delay still intersect because both intervals contain the true offset; a
disjoint interval is evidence that the offset moved (or that the observation
model stopped holding). M4 tests this reset directly.

`t2` and `t3` are distinct on purpose: `t3 − t2` is the responder's own
service time and must be excluded from the network term, or a busy peer looks
like a skewed one. The route captures `t2` before awaiting authorization and
`t3` after authorization immediately before serializing the body, so signature
verification and a cold live-membership check are service time, not apparent
clock skew.

### 3.2 The route

A new peer route, `GET /_internal/v1/clock`, answered by
`crates/plurxd/src/http/internal_clock.rs`. The handler itself reads no Store
and holds no application lock, but its existing live-member authorization may
perform the same consistent authority check as any other exact peer request;
M0 measures that cost rather than claiming it is free. It returns

```json
{ "node_id": "lab2", "received_unix_ms": 1789..., "sent_unix_ms": 1789... }
```

It uses `PeerAuthMode::ExactRequestAndMemberResponse` (nonce + method + path +
body digest on the request, then status + exact body + request nonce + path on
the response) and the responder's existing
`authorize_internal_peer_request`. That is the 30-second exact-request window,
not the five-second high-volume read window. No freshness window is widened
and no second clock-specific authorizer is introduced. The two-second guard
threshold is therefore observable before the five-second internal-read
assumption fails, while a gross skew above 30 seconds fails closed as
`Unknown`.

An authentication refusal, unsigned error, unsupported `404`, timeout,
invalid body, or invalid response proof yields `Unknown`, never a one-sided
offset estimate. A timestamp from an unauthenticated refusal is attacker input
and cannot become safety evidence. The metrics distinguish `unknown` from a
zero offset so the operator can still see the failure mode even when the exact
offset is outside the authentication envelope.

Cadence: `CLOCK_PROBE_INTERVAL = 10 s`, aligned with `HEARTBEAT_INTERVAL`,
fan-out bounded by the existing activity-peer budget
(`MAX_ACTIVITY_PEERS = MAX_COMMITTED_ROSTER_MEMBERS = 64`,
[membership.rs:969-970](../../crates/plurx-core/src/cluster/membership.rs))
with `PEER_CONCURRENCY = 8` as
[internal_activity.rs:45](../../crates/plurxd/src/http/internal_activity.rs)
already uses. Cost per node per day: 8 640 requests × peers and no consensus
entry from the clock route itself. Existing exact authorization can add one
authority read per accepted inbound probe; the M0 readout must report that
rate, and a later implementation must not hide it behind the phrase
"no Store".

### 3.3 The state, which has three values and not two

```rust
// crates/plurx-core/src/cluster/clock.rs (new shared policy state)
pub(crate) enum PeerClockOffset {
    /// A filtered sample exists and is fresh.
    Bounded { offset_us: i64, uncertainty_us: i64, observed_at: Instant },
    /// A peer exists but this round produced no usable authenticated sample,
    /// or the probe loop has not completed a round recently enough.
    Unknown,
}

pub(crate) enum ClusterClockState {
    /// No committed peer to measure against: standalone SQLite, a sole
    /// voter, or a node whose roster is only itself.
    NoPeers,
    /// Every peer has a fresh bounded sample.
    Bounded { worst_abs_upper_us: i64 },
    /// At least one peer is Unknown.
    Incomplete { unknown_peers: usize, worst_abs_upper_us: i64 },
}
```

The policy types and one `Arc<ClusterClockGuard>` live in `plurx-core` so
`MembershipManager`, media-session recovery and HTTP readiness consult the
same local snapshot. The daemon owns transport and filtering and publishes one
complete roster snapshot into that handle; `plurx-core` never performs HTTP.
Putting the state only in `plurxd` would force membership mutation either to
bypass the guard or to depend upward on the daemon crate.

Initialization fails closed. A standalone SQLite process starts `NoPeers`; a
replicated process starts `Incomplete { unknown_peers: 1, ... }` until it has
read the committed roster. It publishes `NoPeers` only after that roster proves
there is no remote member. A newly committed peer enters as `Unknown` before
its first probe, and a removed peer disappears only after the committed roster
no longer contains it. Startup must never transiently publish `NoPeers` merely
because discovery has not finished.

`worst_abs_upper_us` is `max over peers of |offset| + uncertainty` — the
largest offset magnitude the authenticated interval cannot rule out. A
decision allows only when that upper bound is at or below the configured
limit. Comparing `|offset| − uncertainty` would answer a different question
("is violation certain?") and would allow an interval that crosses the safety
bound, contradicting the objective. A congested link therefore produces
`Unknown` or a conservative refusal, not false evidence of synchrony.

`Unknown` is not zero. A node that has just started, whose current probe
failed, or whose peers all run an older binary has no current evidence of
synchrony and must be treated as such by each decision in §3.5 — but
"treated as such" differs per decision, which is the point of separating
them. One failed round makes that peer `Unknown` immediately; retaining a
previous sample for six failed rounds would spend another node's lease using
evidence known to be stale.

`CLOCK_OBSERVATION_MAX_AGE = 25 s` is only a watchdog for a stalled probe
loop: two complete 10-second intervals plus five seconds of scheduler margin.
It is not a grace period after an explicit probe failure.

### 3.4 The bound, and where it comes from

`CLOCK_OFFSET_REFUSAL_MS = 2_000`. It is a source constant, not a setting or
an enablement gate. Not chosen: derived.
`RELAY_DEADLINE_CLOCK_SKEW_ALLOWANCE` is already 2 s
([media_sessions.rs:116](../../crates/plurxd/src/media_sessions.rs)), and
`LEASE_INTERVAL` is 3 s — an offset that consumes a whole renewal interval
is past the point where an owner can defend its lease. Two seconds is below
that and equal to the allowance the relay path already grants. If the fleet's
measured offsets make 2 s noisy, the number moves by measurement (M0), not
by argument.

This is the *relative* bound the protocols actually need. If M0 shows the
authenticated interval's upper bound routinely approaches 2 seconds on a
healthy fleet, implementation stops: changing the constant requires a new
protocol argument against the 3-second renewal interval, not a dashboard
toggle. OPERATIONS.md's
250 ms is an *absolute* bound to UTC and stays where it is: a
go/no-go rule for benchmark and drill runs. M4 adds one sentence there
pointing at the metric, and does not change the number.

### 3.5 Where the guard fires — and where it does not

| Decision | `NoPeers` | bounded upper ≤ 2 s | bounded upper > 2 s | `Incomplete` |
|---|---|---|---|---|
| Session takeover (initiate) | allow | allow | **refuse** | **refuse** |
| `expired_media_sessions` scan | allow | allow | **refuse** | **refuse** |
| Membership change (propose) | allow | allow | **refuse** | **refuse** |
| `/readyz` | ready | ready | **not ready** | ready |
| Own lease renewal | allow | allow | allow | allow |
| Serving an admitted body | allow | allow | allow | allow |
| Self-fence on lost lease | allow | allow | allow | allow |

Reasons, each row:

- **Takeover and the expiry scan refuse on `Incomplete`.** These are the two
  decisions where one node's clock spends *another* node's lease. Failing
  closed costs a delayed recovery; failing open costs a fleet-wide playback
  restart, which is the F-sc-10 scenario. Refusal here means `continue` on
  the existing takeover tick — the loop already tolerates a skipped tick
  ([media_sessions.rs:4384-4390](../../crates/plurxd/src/media_sessions.rs)).
- **Membership change refuses** because OPERATIONS.md already says a
  membership change under unsynchronized clocks is a no-go; this makes the
  daemon enforce what the runbook asks of the operator. The shared core handle
  is checked at the start of `redeem`, `finalize`, `promote_learner`,
  `remove_node` and `leave_node`, before an intent, durable removal fence, or
  Raft proposal is written. Token issuance alone is not a membership change.
  There is no clock-guard bypass for removing an `Unknown` peer: first restore
  the signed probe or clock discipline. The existing removal protocol already
  requires that target to apply a durable route fence, so an unreachable node
  is not made safely removable by ignoring the clock guard.
- **`/readyz` stays ready on `Incomplete`.** A rolling deploy makes every
  peer temporarily `Unknown`; taking the whole fleet out of rotation because
  a probe route is new would be the upgrade turning itself off. Only a
  *positive* measurement above the bound, sustained across two consecutive
  probe rounds, reports `ReadinessFailure::ClockUnbounded`. Readiness is a
  single local read of the cached state — no Store call, honouring the
  comment at [mod.rs:1034-1037](../../crates/plurxd/src/http/mod.rs).
- **Renewal, serving and self-fencing are untouched.** A node with a wrong
  clock still owns what it owns; dropping it would convert a clock fault into
  the outage the guard exists to prevent. Its renewals fail or succeed
  against the authoritative row exactly as today.
- **The process never exits.** CockroachDB's `--max-offset` self-terminates;
  that is right for a database whose clients retry elsewhere and wrong for a
  media server whose every in-flight playback dies with the process. The
  review cites CockroachDB's behaviour; this plan takes its *measurement* and
  refuses its *remedy*, and says so here so the divergence is deliberate.

### 3.6 Metrics

Hand-written into the existing Prometheus text in
[system.rs:4645-4660](../../crates/plurxd/src/http/system.rs), scraped
without taking the Store or the Activity gate (the contract
`cluster.page-reads` already requires that of the neighbouring families).

```
# HELP plurx_cluster_clock_offset_seconds Measured clock offset to a committed peer.
# TYPE plurx_cluster_clock_offset_seconds gauge
plurx_cluster_clock_offset_seconds{peer="lab2"} 0.031

# HELP plurx_cluster_clock_offset_uncertainty_seconds Half the filtered round trip for that sample.
# TYPE plurx_cluster_clock_offset_uncertainty_seconds gauge
plurx_cluster_clock_offset_uncertainty_seconds{peer="lab2"} 0.0004

# HELP plurx_cluster_clock_observation_state Per-peer offset observation state.
# TYPE plurx_cluster_clock_observation_state gauge
plurx_cluster_clock_observation_state{peer="lab2",state="bounded"} 1
plurx_cluster_clock_observation_state{peer="lab2",state="unknown"} 0

# HELP plurx_cluster_clock_refusals_total Decisions refused because the clock could not be bounded.
# TYPE plurx_cluster_clock_refusals_total counter
plurx_cluster_clock_refusals_total{decision="takeover",cause="offset"} 0
plurx_cluster_clock_refusals_total{decision="takeover",cause="unknown"} 0
plurx_cluster_clock_refusals_total{decision="membership_change",cause="offset"} 0
plurx_cluster_clock_refusals_total{decision="membership_change",cause="unknown"} 0
plurx_cluster_clock_refusals_total{decision="expiry_scan",cause="offset"} 0
plurx_cluster_clock_refusals_total{decision="expiry_scan",cause="unknown"} 0
```

Label bounds, stated because an unbounded label set is a cardinality bug:
`peer` takes values from the committed roster, capped at 64
([membership.rs:969](../../crates/plurx-core/src/cluster/membership.rs)),
and a node that leaves the roster loses its series on the next scrape;
`state` ∈ {`bounded`, `unknown`}; `decision` ∈ {`takeover`,
`membership_change`, `expiry_scan`}; `cause` ∈ {`offset`, `unknown`}. The
offset gauge is a signed value in seconds so `abs()` and a symmetric alert
work in PromQL; it is absent for a peer in `Unknown`, which is why the
separate state gauge exists — a missing series and a zero offset must not
look alike.

### 3.7 Leader-assigned times

This plan does **not** move lease stamping to the leader. The client binds
`now` today ([media_sessions.rs:3895](../../crates/plurxd/src/media_sessions.rs),
[membership.rs:4307](../../crates/plurx-core/src/cluster/membership.rs)) and
continues to; the guard makes that clock trustworthy instead of relocating
it. Moving it would need a leader round trip per lease tick, which is exactly
the cost S3 is removing elsewhere.

What this plan fixes is the *rule* for any value that is later made
leader-assigned, because the review's withdrawn remedy shows how it goes
wrong:

> A leader-assigned time is read once, on the leader, into a Rust value, and
> bound as a parameter of the replicated statement. It never appears as SQL.
> `ReplicatedSql::new` already refuses `unixepoch`, `now`, `datetime`,
> `strftime`, `julianday` and `random`
> ([replicated.rs:879-897](../../crates/plurx-core/src/store/replicated.rs));
> M3 adds the corresponding *positive* test — a statement that binds a
> caller-computed `now` and a fixture that proves two voters replaying the
> same entry write the same value.

The existing test named in the appendix,
`replicated_auth_schema_and_writes_bind_every_clock_value`, is that rule for
the auth schema; M3 extends the same assertion shape to the session and
membership statements.

### 3.8 Developer readiness is advisory, not an enablement switch

K-06 adds no setting, cargo feature, environment variable, hidden query
parameter or per-node enable button. M0 is a measurement-only binary by
construction. The later enforcement change is a separate reviewed change
that lands only after M0 evidence confirms the fixed 2-second contract; once
that change lands, every clustered node enforces it. A mixed deployment is
handled by `Unknown`, not by an operator coordinating a switch across voters.

Settings → Developer may display these read-only facts:

| Field | Meaning |
|---|---|
| Clock contract | `measurement-only` or `enforcing`, reported by the binary |
| Observation coverage | bounded peers / committed remote peers |
| Worst upper bound | `max(|offset| + uncertainty)` in milliseconds |
| Refusal bound | the compiled `CLOCK_OFFSET_REFUSAL_MS` (`2 000`) |
| Readiness consequence | advisory text naming `Incomplete` or the decision that would refuse |

The panel never mutates these values and never blocks saving unrelated
Developer settings. `/readyz` remains the machine-facing signal described in
§3.5; the page only explains it. Rollback of enforcement is a reviewed binary
rollback or corrective release, not a setting flip that can leave voters on
different safety policies.

No recipe identity, cache digest or manifest changes: nothing here enters a
transcode recipe or a cache key.

## 4. Guardrails (non-goals)

Each is an assessment disposition; each says how this plan honours it.

- **No clock evaluated inside replicated SQL** (assessment correction 12,
  F-sc-10 row). Honoured by §3.7: no SQL is added at all, and M3 pins the
  rule with a test. `validate_sql` already refuses the identifiers.
- **No offset derived from `last_seen_at`** (assessment correction 3).
  Honoured by §2.2 and §3.1: the measurement is a four-timestamp exchange on
  its own route. `cluster_nodes` is not read by the guard.
- **Unknown state is handled separately from zero** (correction 3, "separate
  handling of missing observations"). Honoured by §3.3's three-valued state
  and the per-decision table in §3.5, where `Incomplete` and `Bounded ≤ 2 s`
  have different answers for takeover and the same answer for readiness.
- **Explicit delay/uncertainty bounds** (correction 3). Honoured by §3.1:
  every sample carries `uncertainty = round_trip/2 + quantization`, retained
  in microseconds so half-millisecond results are exact; decisions
  compare `|offset| + uncertainty`, and a sample whose round trip exceeds the
  transport deadline becomes `Unknown`.
- **Do not weaken the peer authentication fence.** The nonce, target
  binding, body digest, signature length, replay window and redirect refusal
  are unchanged. The route uses the existing 30-second exact-request
  authorizer; it neither borrows nor changes the five-second internal-read
  authorizer.
- **Do not make readiness a Store round trip** ([mod.rs:1034-1037](../../crates/plurxd/src/http/mod.rs)).
  Honoured: readiness reads an `ArcSwap` of the cached state.
- **Do not fence a sole voter or a standalone SQLite node.** `NoPeers` allows
  everything; M2's test covers a one-node cluster and the SQLite backend
  explicitly, because a guard that bricks the single-node install is worse
  than no guard.
- **Do not terminate the process.** §3.5, last bullet, with the reason.
- **No feature or enablement gate.** No cargo feature, setting, environment
  variable or UI control switches enforcement. Measurement and enforcement
  are separate reviewed releases, and Developer readiness is read-only.
- **Do not change `LEASE_TTL_MS`, `LEASE_INTERVAL` or
  `RELAY_DEADLINE_CLOCK_SKEW_ALLOWANCE`** in this work. The guard measures
  the assumption those constants already make; changing them is a separate
  decision with its own evidence.

## 5. Design milestones — this PR stops before runtime behaviour

K-06 is one design plan and one draft PR. These milestones complete the
decision artifact; they do not authorize Rust changes. After adversarial
review, the work board must create separately owned implementation plans for
the measurement release and the enforcement release. That split is required
because fleet evidence from the merged measurement release is an input to the
enforcement release, not evidence a single unmerged branch can manufacture.

### 5.1 M0 — inventory the actual wire and clock assumptions

The inventory in §2 records the existing signed request timestamp, response
proof, 30-second exact-request window, five-second internal-read window,
two-second relay allowance, and every ownership decision that uses wall time.

Acceptance:

```bash
rg -n "TIMESTAMP_HEADER|signed_response_payload|PeerAuthMode" \
  crates/plurxd/src/http/peer_transport.rs
rg -n "ACTIVITY_AUTH_WINDOW_MS|INTERNAL_READ_AUTH_WINDOW_MS" \
  crates/plurx-core/src/cluster/membership.rs
rg -n "RELAY_DEADLINE_CLOCK_SKEW_ALLOWANCE|expired_media_sessions" \
  crates/plurxd/src/media_sessions.rs
```

### 5.2 M1 — make the measurement arithmetic executable

The later `clock_offset` unit test uses these exact fixtures. Arithmetic keeps
half-millisecond results exactly (store signed microseconds or doubled
milliseconds; do not truncate the point estimate before constructing the
interval).

| Case | `t1,t2,t3,t4` (ms) | Expected offset | Expected uncertainty | Result |
|---|---:|---:|---:|---|
| symmetric | `1000,1011,1013,1024` | `0 ms` | `12 ms` | bounded, contains 0 |
| asymmetric | `1000,1050,1052,1053` | `+24.5 ms` | `26.5 ms` | bounded, contains 0 |
| above bound | `1000,3510,3512,1022` | `+2500 ms` | `11 ms` | upper `2511 ms`, refuse |
| crosses bound | `1000,3100,3102,2202` | `+1500 ms` | `601 ms` | upper `2101 ms`, refuse |
| responder step | any `t3 < t2` | n/a | n/a | `Unknown` |
| excessive RTT | network RTT `2001 ms` | n/a | n/a | `Unknown` |

Acceptance for the later measurement PR:

```bash
cargo test -p plurxd clock_offset::tests::four_timestamp_contract
cargo test -p plurxd clock_offset::tests::step_resets_minimum_delay_window
```

The second test seeds a low-delay sample, advances the peer by 15 seconds,
then supplies another low-delay sample. The conservative intervals are
disjoint, so the old eight-sample window is cleared and the new sample is
selected in that round.

### 5.3 M2 — fix the refusal and rollout decisions

The decisions are final for follow-on planning:

1. **Compare the upper bound.** Allow only when
   `abs(offset) + uncertainty <= 2_000 ms`; an interval crossing the bound is
   not evidence of safety.
2. **Fail dangerous acquisition closed on `Unknown`.** Takeover, expiry scan,
   and membership mutation refuse; renewal, serving, and self-fencing remain
   unchanged. `/readyz` remains ready on `Incomplete` so a mixed deployment
   does not drain the fleet.
3. **Keep the refusal node-local.** The Store primitive accepts a caller-bound
   `now_ms` as today; it does not receive or reinterpret clock evidence.
   `ClusterClockGuard` lives in `plurx-core`, so membership entry points can
   refuse before writing lifecycle intent while the daemon remains the only
   component that performs probes.
4. **Do not widen authentication.** The probe uses the existing 30-second
   exact-request path. The five-second internal-read path is evidence that the
   guard must act earlier, not a window to change.
5. **Use no enablement gate.** Measurement and enforcement are separate
   reviewed releases. Developer readiness explains state but cannot change it.

Acceptance for the later enforcement plan:

```bash
cargo test -p plurxd media_sessions::tests::takeover_clock_guard
cargo test -p plurxd http::tests::readiness_clock_guard
cargo test -p plurx-core --features hiqlite-store \
  cluster::membership::tests::change_refuses_unbounded_clock
```

The takeover test covers `NoPeers`, a safe upper bound, an interval crossing
2 seconds, a certain violation, and `Incomplete`. The readiness test requires
two consecutive violating rounds for `503` but stays `200` through
`Incomplete`. The membership test asserts one stable typed refusal code.

### 5.4 M3 — pin the replicated-clock rule

Later runtime work extends the existing SQL guard; it does not move time into
SQLite. A source census covers session and membership statements, and a
two-voter fixture proves that one caller-bound timestamp replays identically.

Acceptance for that implementation plan:

```bash
cargo test -p plurx-core --features hiqlite-store store::replicated
cargo test -p plurx-core --features hiqlite-contract-tests \
  replicated_auth_schema_and_writes_bind_every_clock_value
```

Replacing one bound timestamp with `unixepoch('subsec')` must make the first
command fail.

### 5.5 M4 — record the evidence gate

The measurement release contains the route, prober, filter, metrics, advisory
Developer fields and no refusal call site. Its fleet evidence must exist before
an enforcement plan is claimed.

```text
GPT prompt (fleet): With <measurement-sha> on lab1-lab3, leave the fleet idle
for one hour. From each node provide `curl -s localhost:32400/metrics | grep
plurx_cluster_clock_`, `chronyc tracking`, and `chronyc sources -v`. Report
the distribution of abs(offset), uncertainty, abs(offset)+uncertainty,
Unknown rounds, and authority reads attributable to the clock route. Then
load lab3's network path for 60 seconds with an ordinary media encode and
repeat the readout. Do not step any host clock in this measurement phase.
The enforcement plan may proceed only if every healthy upper bound remains
below 2,000 ms and the probe's authority-read rate matches the design.
```

After enforcement exists in a disposable lab deployment, its acceptance is
24 hours with healthy refusals flat, followed by an approved 15-second clock
step proving that no session changes owner, takeover/expiry/membership refuse,
readiness drops after two violating rounds, and recovery occurs after clock
discipline returns. That destructive drill is not authorized by this design
PR and must name its operator and recovery procedure before execution.

## 6. Verification and rollout

This design PR changes Markdown only, so it does not establish a Rust compile
loop or run a broad unit suite. Its local acceptance is the docs index, link
contracts, formatting, and the static source checks in §5.1.

The eventual rollout has two release boundaries:

1. **Measurement release.** Older peers return `404`; the prober records
   `Unknown`. No production decision consults the observation, so mixed
   versions do not change behaviour.
2. **Enforcement release.** It is proposed only after the M4 fleet record is
   attached to its own plan. Mixed versions make observation `Incomplete`,
   which refuses new ownership/membership actions but does not make readiness
   fail. Rollback is a reviewed binary rollback, not a per-node switch.

Silence is not evidence of synchrony: a missing, expired, rejected or invalid
probe is always represented as `Unknown`.

## 7. Decisions made and evidence still missing

There are no unresolved protocol decisions in this design boundary.

1. **Host type is evidence, not a branch in the protocol.** Record whether
   lab1-lab6 are bare metal or can receive a hypervisor resume step in the M4
   fleet artifact. The four-timestamp exchange and interval-reset rule are the
   same either way.
2. **The filter is eight samples at ten seconds, with discontinuity reset.**
   A step does not wait 80 seconds because disjoint intervals clear the old
   window. M0 may show this cadence is too expensive, but it does not silently
   change the safety rule.
3. **Expiry refusal is node-local.** The Store does not accept clock evidence
   and does not evaluate a second clock. Passing evidence into replicated SQL
   would widen the state-machine contract without improving the caller's
   proof.
4. **Authentication windows stay 30 seconds and five seconds.** The probe
   uses the former; it does not create a 120-second exception or trust an
   unsigned rejection timestamp.
5. **Chrony is corroborating fleet evidence only.** The daemon does not spawn
   `chronyc`, parse its output, or enforce the runbook's absolute 250 ms UTC
   rule. Relative peer offset is the ownership protocol's input.

The remaining blockers are observations: M4's per-voter offset, uncertainty,
Unknown-rate and authorization-cost readout; confirmation of the lab hosts'
clock-discipline environment; and an explicitly approved disposable-lab clock
step after enforcement exists. None has been performed or implied by this
design PR.

---

## Execution log

Executing sessions append one row per logical milestone in the one K-06 plan
PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit trailers
`Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M0-M4 design | [#430](http://192.168.4.7:3000/noirr/plurx/pulls/430) · `1ddfe0c26` | Reconciled the existing signed request timestamp, distinct 30 s/5 s auth windows, authenticated response body, conservative upper-bound decision, discontinuity reset, no-gate rollout split and executable follow-on evidence. No runtime behaviour or fleet result is claimed. |
