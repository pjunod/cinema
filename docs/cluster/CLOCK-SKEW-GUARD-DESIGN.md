# Clock-skew guard — measure the offset, bound it, and refuse the dangerous side

**Status:** ready for review · **Executes:** S9 / F-sc-10 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `0f02b7ea`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Read §2 first: every wall-clock comparison that decides ownership is listed
there with its current line. Then §3, which is a design, not a diff — it
fixes the measurement, the uncertainty, the three states an observation can
be in, and exactly which decisions consult it. Execute §5 in order; M0 is
the measurement everything after it is judged against. If a step seems to
require putting a clock inside a replicated statement, deriving an offset
from `cluster_nodes.last_seen_at`, terminating the process on a bad clock,
or making `/readyz` do a Store round trip, stop and flag it — each of those
is explicitly refused in §4 and each has a reason.

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
   ([membership.rs:10297-10305](../../crates/plurx-core/src/cluster/membership.rs)).
   That is `t1`, authenticated, today. What is missing is the *response*
   side: `signed_response_payload`
   ([peer_transport.rs:303-308](../../crates/plurxd/src/http/peer_transport.rs))
   covers `(status, body)` and no timestamp. So this plan extends an
   existing exchange rather than inventing one.
3. **The code already assumes a two-second bound and never checks it.**
   `RELAY_DEADLINE_CLOCK_SKEW_ALLOWANCE = 2 s`
   ([media_sessions.rs:112-116](../../crates/plurxd/src/media_sessions.rs))
   is a one-sided allowance added to a relayed deadline. The number S9 asks
   us to enforce is therefore already load-bearing in one place and
   unverified in all of them.
4. **Large skew is already fatal, just illegibly.** Activity RPC refuses a
   request whose timestamp differs from local now by more than
   `ACTIVITY_AUTH_WINDOW_MS = 30_000`
   ([membership.rs:961, 6284](../../crates/plurx-core/src/cluster/membership.rs)),
   and the exact internal read window is `INTERNAL_READ_AUTH_WINDOW_MS =
   5_000` ([membership.rs:1009](../../crates/plurx-core/src/cluster/membership.rs)).
   A node five seconds out already loses exact peer reads — reported as an
   authentication refusal, which names the wrong cause. §3.3 turns that
   refusal into evidence instead of hiding it.

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
([membership.rs:6256-6270](../../crates/plurx-core/src/cluster/membership.rs));
response payloads carry none.

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
 t4  |                                              |
     v                                              v

 offset      = ((t2 - t1) + (t3 - t4)) / 2       // B's clock minus A's
 round_trip  =  (t4 - t1) - (t3 - t2)            // network time only
 uncertainty =  round_trip / 2                   // one-way asymmetry bound
```

The uncertainty term is the whole point and is the part the review's first
remedy lacked. One-way delays are non-negative, so the true offset lies in
`[offset − round_trip/2, offset + round_trip/2]` whatever the asymmetry;
that interval is exact, not statistical. A sample with
`round_trip < 0` or `round_trip > CLOCK_PROBE_MAX_RTT_MS` (2 000, the
existing peer deadline in
[internal_activity.rs:29](../../crates/plurxd/src/http/internal_activity.rs))
is discarded as unusable rather than believed.

NTP's clock filter is adopted for the same reason NTP has it: the minimum-
delay sample in a window is the least contaminated by queueing. Keep the
last `CLOCK_FILTER_DEPTH = 8` samples per peer (80 s at a 10 s cadence) and
report the one with the smallest `round_trip`, together with its
`uncertainty`. Discard a sample whose `round_trip` exceeds four times the
window minimum before it enters the window.

`t2` and `t3` are distinct on purpose: `t3 − t2` is the responder's own
service time and must be excluded from the network term, or a busy peer
looks like a skewed one.

### 3.2 The route

A new peer route, `GET /_internal/v1/clock`, answered by
`crates/plurxd/src/http/internal_clock.rs`. It touches no Store, holds no
lock, and returns

```json
{ "node_id": "lab2", "received_unix_ms": 1789..., "sent_unix_ms": 1789... }
```

It reuses `PeerAuthMode::Exact` (nonce + body digest,
[peer_transport.rs:380-405](../../crates/plurxd/src/http/peer_transport.rs))
with one deliberate difference, which is a security decision and is written
here so it is reviewed as one:

**`CLOCK_PROBE_AUTH_WINDOW_MS = 120_000`, not the 5 000 of
`INTERNAL_READ_AUTH_WINDOW_MS`.** The narrow window exists so a captured
request cannot be replayed later to obtain a *stale read of state*. This
route returns no state: its body is the responder's clock at the moment it
answers, so a replayed request yields a fresh, correct answer and teaches an
attacker nothing a `Date` header would not. Meanwhile the narrow window is
exactly the failure this guard exists to detect — at five seconds of skew
the probe would be refused and the offset would stay invisible. Sender,
target, path, nonce and body digest remain signed and unchanged; only the
freshness window widens, and only on this route.

Refusal is still evidence. When the responder refuses for any reason it
answers `409` with a signed body carrying its own `sent_unix_ms`; the prober
records that as a one-sided sample with `uncertainty = round_trip` (no `t2`,
so the interval is wider) rather than as "unknown". A peer that refuses
without a timestamp — an older binary — yields `Unknown`, not zero (§3.4).

Cadence: `CLOCK_PROBE_INTERVAL = 10 s`, aligned with `HEARTBEAT_INTERVAL`,
fan-out bounded by the existing activity-peer budget
(`MAX_ACTIVITY_PEERS = MAX_COMMITTED_ROSTER_MEMBERS = 64`,
[membership.rs:969-970](../../crates/plurx-core/src/cluster/membership.rs))
with `PEER_CONCURRENCY = 8` as
[internal_activity.rs:45](../../crates/plurxd/src/http/internal_activity.rs)
already uses. Cost per node per day: 8 640 requests × peers, no Store, no
consensus entry.

### 3.3 The state, which has three values and not two

```rust
// crates/plurxd/src/clock_offset.rs (new)
pub(crate) enum PeerClockOffset {
    /// A filtered sample exists and is fresh.
    Bounded { offset_ms: i64, uncertainty_ms: i64, observed_at: Instant },
    /// A peer exists but no usable sample does — never probed, every probe
    /// failed, or the newest sample is older than CLOCK_OBSERVATION_MAX_AGE.
    Unknown,
}

pub(crate) enum ClusterClockState {
    /// No committed peer to measure against: standalone SQLite, a sole
    /// voter, or a node whose roster is only itself.
    NoPeers,
    /// Every peer has a fresh bounded sample.
    Bounded { worst_abs_lower_ms: i64 },
    /// At least one peer is Unknown.
    Incomplete { unknown_peers: usize, worst_abs_lower_ms: i64 },
}
```

`worst_abs_lower_ms` is `max over peers of max(0, |offset| − uncertainty)` —
the largest offset magnitude the evidence *cannot rule out being below*. A
decision refuses on the lower bound, never on the point estimate, so a
momentarily congested link widens the interval and produces indecision, not
a false fence.

`Unknown` is not zero. A node that has just started, or whose peers all run
an older binary, has no evidence of synchrony and must be treated as such by
each decision in §3.5 — but "treated as such" differs per decision, which is
the point of separating them.

`CLOCK_OBSERVATION_MAX_AGE = 60 s` (six probe intervals, the same ratio
`NODE_REACHABLE_WINDOW_MS` has to `HEARTBEAT_INTERVAL`).

### 3.4 The bound, and where it comes from

`CLOCK_OFFSET_REFUSAL_MS = 2_000`. Not chosen: derived.
`RELAY_DEADLINE_CLOCK_SKEW_ALLOWANCE` is already 2 s
([media_sessions.rs:116](../../crates/plurxd/src/media_sessions.rs)), and
`LEASE_INTERVAL` is 3 s — an offset that consumes a whole renewal interval
is past the point where an owner can defend its lease. Two seconds is below
that and equal to the allowance the relay path already grants. If the fleet's
measured offsets make 2 s noisy, the number moves by measurement (M0), not
by argument.

This is the *relative* bound the protocols actually need. OPERATIONS.md's
250 ms is an *absolute* bound to UTC and stays where it is: a
go/no-go rule for benchmark and drill runs. M4 adds one sentence there
pointing at the metric, and does not change the number.

### 3.5 Where the guard fires — and where it does not

| Decision | `NoPeers` | `Bounded` ≤ 2 s | `Bounded` > 2 s | `Incomplete` |
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
  daemon enforce what the runbook asks of the operator.
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

### 3.8 Settings

Two replicated settings in
[store/mod.rs:1467+ `keys`](../../crates/plurx-core/src/store/mod.rs),
surfaced in Settings → Developer with an advisory readiness list (never a
blocking one), per the repo's rule that a switch is a setting and not a
compile flag:

- `cluster.clock_guard_enabled` — default absent/off for the first deploy,
  flipped on after M0's fleet readout. Off means measure and export only:
  every refusal becomes a counter increment and a `tracing::warn!` with no
  behaviour change, so the bound can be validated against real data before it
  fences anything.
- `cluster.clock_offset_refusal_ms` — the bound, default `2000`, so the
  number can be corrected from the fleet without a deploy. Parsed with a
  floor of 250 and a ceiling of 30 000 (`ACTIVITY_AUTH_WINDOW_MS`, above
  which the peer RPC fails anyway); an out-of-range value logs and uses the
  default rather than being honoured.

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
  every sample carries `uncertainty = round_trip/2`, decisions compare
  `|offset| − uncertainty`, and a sample whose round trip exceeds the
  transport deadline is discarded.
- **Do not weaken the peer authentication fence.** The nonce, target
  binding, body digest, signature length and redirect refusal are unchanged;
  only this one route's freshness window widens, with the reason in §3.2.
  Everything else keeps `INTERNAL_READ_AUTH_WINDOW_MS`.
- **Do not make readiness a Store round trip** ([mod.rs:1034-1037](../../crates/plurxd/src/http/mod.rs)).
  Honoured: readiness reads an `ArcSwap` of the cached state.
- **Do not fence a sole voter or a standalone SQLite node.** `NoPeers` allows
  everything; M2's test covers a one-node cluster and the SQLite backend
  explicitly, because a guard that bricks the single-node install is worse
  than no guard.
- **Do not terminate the process.** §3.5, last bullet, with the reason.
- **No in-code feature gate.** The switch is `cluster.clock_guard_enabled`, a
  replicated setting in Settings → Developer, not a cargo feature — the same
  rule the `live-hls-recovery` removal note records in
  [plurxd/Cargo.toml:11-17](../../crates/plurxd/Cargo.toml).
- **Do not change `LEASE_TTL_MS`, `LEASE_INTERVAL` or
  `RELAY_DEADLINE_CLOCK_SKEW_ALLOWANCE`** in this work. The guard measures
  the assumption those constants already make; changing them is a separate
  decision with its own evidence.

## 5. Milestones

One draft PR per milestone into `main` under the fast lane. Every milestone
has a focused test command; the fleet-only ones carry a GPT prompt.

### 5.1 M0 — measure before designing the bound

Route, prober loop, filter and metrics, with `cluster.clock_guard_enabled`
absent so nothing refuses anything. No decision reads the state yet.

Acceptance: `cargo test -p plurxd clock_offset` covers the offset and
round-trip arithmetic against fixed `t1..t4` quadruples, including a
deliberately asymmetric pair (`t2−t1 = 50 ms`, `t4−t3 = 1 ms`) where the
point estimate is wrong by 24.5 ms and the reported interval still contains
the truth; `curl -s localhost:32400/metrics | grep plurx_cluster_clock_`
on a lab voter prints one series per peer.

```text
GPT prompt (fleet): With <sha> on lab1-lab3, leave the new setting unset and
let the fleet idle for one hour. Then give me, from each of the three nodes,
the full output of `curl -s localhost:32400/metrics | grep
plurx_cluster_clock_`, plus `chronyc tracking` and `chronyc sources -v` from
each node. I want the observed |offset| distribution and the observed
uncertainty (round_trip/2) so the 2 000 ms bound can be confirmed or
corrected before anything refuses on it. Also run it once with lab3's
network path loaded (`ping -f` from lab1 for 60 s, or an ffmpeg job
saturating its NIC) so I can see the uncertainty widen rather than the
offset move.
```

### 5.2 M1 — takeover and the expiry scan consult the state

The two refusals in §3.5, gated on `cluster.clock_guard_enabled`, with the
`plurx_cluster_clock_refusals_total` counter incrementing in both the gated
and ungated cases so the shadow period shows what would have been refused.

Acceptance: `cargo test -p plurxd media_sessions::tests::takeover_refuses`
covers three cases — a `Bounded` 5 s offset refuses, a `Bounded` 5 s offset
with 4 s uncertainty (lower bound 1 s) allows, and `Incomplete` refuses —
and `cargo test -p plurxd media_sessions::tests::takeover_allows_without_peers`
proves a one-peer-less node still takes over. `make unit` green.

### 5.3 M2 — readiness and membership change

`ReadinessFailure::ClockUnbounded` in
[http/mod.rs:1022-1084](../../crates/plurxd/src/http/mod.rs), requiring two
consecutive rounds above the bound; membership-change proposal refusal in
[membership.rs](../../crates/plurx-core/src/cluster/membership.rs) with a
stable refusal code so the admin surface can name the cause.

Acceptance: `cargo test -p plurxd http::readiness_clock` shows `/readyz`
returning `503 clock unbounded` only after the second consecutive round, and
returning `200` throughout an `Incomplete` window; `cargo test -p plurx-core
cluster::membership::tests::change_refuses_unbounded_clock` green;
`make unit` green.

### 5.4 M3 — pin the replicated-clock rule

Extend the binding assertion beyond the auth schema: a test that walks the
session and membership statements and fails if any of them contains a
`FORBIDDEN_IDENTIFIERS` entry, plus a two-voter replay fixture proving both
voters store the same bound timestamp for one entry.

Acceptance: `cargo test -p plurx-core store::replicated` and
`cargo test -p plurx-core --features hiqlite-contract-tests
replicated_auth_schema_and_writes_bind_every_clock_value` green; reverting
one statement to `unixepoch('subsec')` makes the first fail.

### 5.5 M4 — steps, asymmetry, and the runbook

Three behaviour tests driven by an injectable clock (a `ClockSource` trait
with a test implementation; production keeps `SystemTime::now`):

1. **Step.** A peer's clock jumps +15 s between two probe rounds. The window
   holds eight samples, so the minimum-delay sample is stale for up to 80 s —
   the test asserts the guard refuses within two rounds, which requires the
   filter to discard the window on a discontinuity rather than average
   through it. This is the case a naive minimum-delay filter gets wrong and
   is why it is a milestone rather than a line.
2. **Asymmetric delay.** A 200 ms one-way delay in one direction only, clocks
   identical. The point estimate reads 100 ms; the interval must contain 0
   and the guard must not refuse.
3. **Unknown.** A peer that never answers, and a peer that answers `409`
   without a timestamp. Both yield `Unknown`; takeover refuses, readiness
   does not.

Also in this PR: one paragraph in [OPERATIONS.md](../OPERATIONS.md) §"Synchronize
clocks before cluster work" naming `plurx_cluster_clock_offset_seconds` and
the two settings keys, leaving the 250 ms absolute rule unchanged, and one
row in [ARCHITECTURE.md](../ARCHITECTURE.md) §9 replacing "membership
reachability and artwork repair proofs currently compare Unix timestamps
from different nodes" with the measured guard.

Acceptance: `cargo test -p plurxd clock_offset::tests` covers all three;
`python3 -m unittest discover -s tests/validation -p 'test_*.py'` green (the
docs index test); `make unit` green.

### 5.6 M5 — enable on the fleet

Flip `cluster.clock_guard_enabled` on all voters, keeping
`cluster.clock_offset_refusal_ms` at whatever M0 justified.

Acceptance: 24 h with `plurx_cluster_clock_refusals_total` flat at zero on a
healthy fleet, and a deliberate step reproducing the F-sc-10 scenario.

```text
GPT prompt (fleet): With <sha> deployed to lab1-lab3 and
cluster.clock_guard_enabled set on all three, start one playback session on
each node. Then on lab3 only, step the clock forward 15 seconds
(`sudo chronyc makestep` will not do this — use `sudo systemctl stop
chrony && sudo date -s '+15 seconds'`), wait 60 seconds, and tell me:
(a) whether any session moved owner, (b) lab3's
plurx_cluster_clock_refusals_total and plurx_cluster_clock_offset_seconds,
(c) lab1's and lab2's offset series for peer="lab3", (d) whether lab3's
/readyz went 503 and after how long. Then restore chrony
(`sudo systemctl start chrony`) and confirm all three return to ready and
the offset series return under 0.05. Repeat the whole thing with
cluster.clock_guard_enabled unset on lab1 and lab2 only, so I can see the
unguarded takeover happen once and compare.
```

## 6. Verification and rollout

Fast lane `make unit` per PR plus the named filters. M0-M4 are additive: an
older binary refuses `/_internal/v1/clock` with 404, which the prober records
as `Unknown`, so a mixed fleet degrades to "no guard" rather than to a wrong
guard — which is why M1's refusal on `Incomplete` must not reach `/readyz`
(§3.5). M5 is a settings flip and rolls back by unsetting the key; no
binary change is needed to disable the guard, which is the reason it is a
replicated setting.

Order matters: M0's fleet readout gates M1's bound. Do not merge M1 with a
bound that has not been compared against a measured distribution.

## 7. Open questions

1. Does anything besides chrony discipline these hosts? If a hypervisor
   injects time on VM resume (the F-sc-10 scenario names it), the step can
   land between two probes with no NTP event to correlate against. Paul to
   confirm whether lab1-lab6 are bare metal.
2. `CLOCK_FILTER_DEPTH = 8` and `CLOCK_PROBE_INTERVAL = 10 s` give an 80 s
   window; M4.1 shows that is also the worst-case staleness after a step.
   A shorter window reacts faster and filters worse. Paul confirms, or M0's
   data picks it.
3. Should `expired_media_sessions` refusal be node-local (skip the tick) or
   should the *store primitive* refuse a `now_ms` that the caller cannot
   bound? Node-local is planned here because the store has no view of the
   caller's clock evidence; a store-side rule would need the evidence passed
   in, which is a wider contract change.
4. Widening one route's auth window (§3.2) is the only security-relevant
   change in this plan. If it is unacceptable, the fallback is the `409`
   path alone — measurable, but only above five seconds of skew, and with a
   wider interval. Paul decides.
5. Whether a chrony reading (`chronyc tracking` root delay + root
   dispersion) should be a second, absolute input exported alongside the
   relative offset. It would let the daemon enforce OPERATIONS.md's 250 ms
   rule directly, at the cost of a process spawn and a parser for a format
   plurx does not control. Not planned here.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| | | | | | |
