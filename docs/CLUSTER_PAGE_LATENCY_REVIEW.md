# Cluster page latency — evidence and adversarial review brief

**Status:** ready for adversarial review · **Scope:** Home, Activity, and
Settings in the clustered web app · **Observed:** 2026-08-22 · **Code:**
`origin/main` at `a0f9fc14`

Companion to [OPERATIONS.md](OPERATIONS.md) (the supported cluster runbook) and
[CLUSTER-PERFORMANCE-PLAN.md](CLUSTER-PERFORMANCE-PLAN.md) (the measurement and
optimization contract). This document records a live production diagnosis and
asks a reviewer to challenge it. It is not authorization to remove a voter,
delete a data directory, restore a snapshot, or alter production membership.

## Reviewer instruction — challenge the causal chain before proposing work

Review this as an incident analysis, performance design, and recovery-safety
problem. Start with the facts in §2 and the code paths in §3. Separate facts,
strong inferences, and open hypotheses. In particular:

1. Confirm or dispute that `nuc4`'s Raft storage inconsistency is the dominant
   cause of the multi-second read latency.
2. Identify the safest supported way to recover or replace that voter without
   risking split brain, silent state loss, or an unsupported activated-node
   restore.
3. Review why PR #510 reduced Store work but did not improve the observed first
   paint, and whether its validation contract was too narrow for the stated
   user outcome.
4. Review the health model: the UI says every voter is reachable while Raft on
   one voter is continuously rejecting `AppendEntries` and the leader cannot
   confirm reads promptly.
5. Review the page-loading model independently of the Raft fault. A repaired
   cluster should be fast, but one slow dependency should still not replace a
   populated page with a full-page `Loading…` sentinel for seconds.

Return a prioritized `P0` / `P1` / `P2` finding list. For each finding, name
the evidence, the failure mode, the smallest safe correction, the regression
test, and any destructive operator step that requires explicit approval.

## 1. Executive finding — a broken replica meets all-or-nothing rendering

The slowdown is real and reproducible. It is not explained by normal LAN
consensus overhead and it is not an undeployed PR.

Two problems compound:

- **Operational fault:** Raft voter `3` (`nuc4`) repeatedly rejects Raft work
  because log index `520000` is missing from its local storage. The leader logs
  failed leadership confirmations for consistent reads. A two-minute sample
  contained 1,014 leader read-confirmation failures and 2,071 `nuc4`
  `LogIndexNotFound` errors.
- **Presentation fault:** Home and Settings replace the current body with
  `Loading…`, await every dependency needed by their page model, and paint only
  after the slowest dependency completes. Cluster retry latency therefore
  becomes blank-screen latency.

PR #510 correctly removed several forms of read amplification. Its retained
validation contract explicitly measures selected Store primitives, not full
HTTP pages, network round trips, or time to first meaningful paint. That
contract passed while the user-visible objective remained unmet.

> Fewer calls are useful evidence, but they are not latency evidence.

## 2. Reproduction — the live four-voter cluster is slow and internally ill

### The observed topology requires three votes but one voter cannot apply Raft work

The live Cluster panel reported this roster during the diagnosis:

| Host | Raft id | Role | UI reachability | Relevant observation |
|---|---:|---|---|---|
| `nynuc` | 1 | voter · inspected web node | reachable | Advertised host is shown as `localhost`; verify that the actual Raft address is remotely usable. |
| `nuc4` | 3 | voter | reachable | Continuously rejects Raft work with `LogIndexNotFound` for index `520000`. |
| `nuc3` | 4 | voter · leader | reachable | Logs repeated failures while confirming leadership for consistent reads against target `3`. |
| `m6` | 5 | voter | reachable | No specific fault established in this investigation. |

Four voters require three acknowledgements and still tolerate only one voter
failure. The existing runbook already warns that four voters add quorum work
without improving failure tolerance over three. That topology choice is not
itself the incident: a healthy four-voter cluster must still answer reads in a
small number of LAN round trips. It does mean one broken voter leaves less
margin and makes every false health signal more consequential.

The `localhost` display on `nynuc` is a review item, not a proven cause. Confirm
the committed Raft and API addresses before attributing any quorum failure to
it. Do not infer routing health from the hostname label alone.

### End-to-end navigation reproduced the delay

The timings below were collected from the already signed-in Chrome tabs. Each
sample starts immediately before clicking the navigation link and ends when a
route-specific content heading becomes visible. These are warm navigations,
not cold process starts. Browser automation overhead is included, so the
values are evidence of the user-visible order of magnitude rather than a
microbenchmark.

| HTTP host | Route | First route-specific content | Observed time |
|---|---|---|---:|
| `nynuc` | Activity | `Now playing` | 761 ms |
| `nynuc` | Settings | Settings tab bar | 3,238 ms |
| `nynuc` | Home | `Continue watching` | 3,236 ms |
| `nuc3` | Activity | `Now playing` | 3,062 ms |
| `nuc3` | Settings | Settings tab bar | 3,063 ms |
| `nuc3` | Home | `Continue watching` | 3,069 ms |

One sample per route is enough to reproduce the complaint, not enough to set a
performance budget. The remediation must add repeated measurements with
percentiles as described in §8.

### The live fleet was not uniformly attributable to one build

`GET /api/v1/server` returned the following during the same investigation:

| Host | Reported build | Built at |
|---|---|---|
| `nynuc` | `v0.2.7-954-g9df935b0` | `2026-08-22T20:44:27Z` |
| `nuc4` | `unknown` | `2026-08-22T20:44:32Z` |
| `nuc3` | `unknown` | `2026-08-22T19:31:40Z` |
| `m6` | `v0.2.7-956-gb1da45f3` | `2026-08-22T20:57:51Z` |

`9df935b0` contains PR #510, so `nynuc` proves the optimization was deployed.
The two `unknown` stamps do not prove an incompatible binary, but they prevent
the fleet from proving that every voter runs the same revision. Finish a
uniform, stamped rollout before treating a version-sensitive conclusion as
closed. Current `main` at the time of writing is `a0f9fc14`, after PR #516.

### Raft errors are continuous, not a transient coincident with one click

The following excerpts are shortened and contain no token or media path.

On `nuc4`:

```text
AppendEntries: vote=T1131-N4:committed,
leader_commit=Some(T1131-N4-534071)
StorageError(Defensive {
  subject: LogIndex(520000),
  violation: LogIndexNotFound { want: 520000, got: None }
})
```

On the leader, `nuc3`:

```text
timeout while confirming leadership for read request
target=3
error=Unreachable node: ... LogIndex(520000) ... got None
```

On `nynuc`:

```text
offline source probe pass failed code="membership_internal"
```

A bounded two-minute sample recorded:

| Signal | Count | Approximate rate |
|---|---:|---:|
| Leader read-confirmation failures | 1,014 | 8.5/s |
| `nuc4` missing-log defensive errors | 2,071 | 17.3/s |
| `nynuc` offline-source-probe failures | 218 | 1.8/s |

The Cluster log also showed a high rate of accepted Hiqlite SQLite WebSocket
streams during page load. Treat connection churn as a plausible amplifier,
not yet a proven primary cause. The missing-log defensive failure and leader
read-confirmation failure are direct evidence; the cost of connection setup
still needs measurement.

### What is fact, inference, and hypothesis

| Classification | Statement | Confidence |
|---|---|---|
| Fact | `nuc4` repeatedly reports `LogIndexNotFound` for Raft log index `520000`. | Direct live logs. |
| Fact | The leader repeatedly fails to confirm consistent reads against Raft target `3`. | Direct live logs; target `3` maps to `nuc4` in the roster. |
| Fact | Home and Settings paint only after their complete awaited request sets resolve. | Current web source. |
| Fact | The UI labels `nuc4` reachable while its Raft core is rejecting work. | Same-time UI and log observations. |
| Strong inference | Raft confirmation retries dominate the multi-second route delay. | Error type, rate, timing shape, and consistent-read code path agree. Confirm with per-operation timings before declaring closure. |
| Hypothesis | Repeated WebSocket setup magnifies CPU/network contention. | Connection log volume is high; no connection-lifetime or CPU correlation was captured. |
| Hypothesis | Mixed or unstamped builds contributed to the invalid log state. | Fleet attribution is incomplete; no causal version boundary is established. |
| Hypothesis | `nynuc`'s displayed `localhost` advertisement reduces usable quorum paths. | The display is suspicious; the actual committed Raft address has not been verified. |

## 3. Critical paths — one delayed authority read holds the whole body

### Every authenticated request still performs an authority-consistent token lookup

The extractor in
[`http/extract.rs`](../crates/plurxd/src/http/extract.rs) lines 175–188 hashes
the bearer token and calls `store.user_for_token`. The Hiqlite implementation
in [`store/hiqlite.rs`](../crates/plurx-core/src/store/hiqlite.rs) lines
1314–1330 executes that lookup with `query_consistent_map`.

PR #514 coalesces replicated *activity timestamp writes*. It deliberately does
not cache the authorization result: revocation, password reset, user deletion,
and admin demotion must take effect authoritatively. Consequently, every HTTP
request on these pages still pays at least one consistent read before its
handler begins.

That security boundary should not be weakened casually. A safe optimization
must either retain authority-consistent authorization or carry a replicated
credential generation/invalidation proof. A time-only user cache can preserve
revoked access and is out of bounds.

```text
navigation click
      │
      ▼
N parallel HTTP requests
      │
      ├──▶ token lookup ──▶ consistent Hiqlite read ──▶ leader confirmation
      │
      └──▶ handler Store reads ───────────────────────▶ leader confirmation
                                                            │
                                                     broken voter retry
                                                            │
                                                            ▼
                                                   slowest request wins
                                                            │
                                                            ▼
                                                  replace `Loading…`
```

### Home waits for both request waves before its first content paint

[`web/index.html`](../crates/plurxd/src/web/index.html) lines 3620–3667 owns
the Home critical path:

1. It awaits `/libraries`, `/hubs`, and `/coming-soon` together.
2. After that entire first request group resolves, it starts one preview
   request per library, with at most six active at once.
3. It awaits every preview worker.
4. Only then does `viewHome` replace the `Loading…` body with rendered content.

The concurrency cap prevents an unbounded burst, but it does not provide an
early paint. With `L` libraries the route still makes `O(L)` preview requests,
and each request performs its own authoritative token lookup before its
handler reads media state.

`/coming-soon` is allowed to fail without failing Home, but it still sits in
the first `Promise.all`; a slow successful integration response can therefore
delay Home. This was not isolated as the live incident's dominant dependency,
but it should not own the Home first-paint deadline.

### Settings waits for seven endpoints regardless of the selected tab

[`web/index.html`](../crates/plurxd/src/web/index.html) lines 9108–9123
installs `Loading…` and awaits these seven endpoints in one `Promise.all`:

| Endpoint | Needed immediately by every tab? | Cluster-relevant cost |
|---|---|---|
| `/libraries` | Only the Libraries tab | Auth read plus library Store read. |
| `/settings` | Most settings tabs | Auth read plus settings snapshot and cache aggregate. |
| `/scan/status` | Only the Libraries tab | Auth read; status body is process memory. |
| `/system` | Only the System tab | Auth read plus several Store reads and process state. |
| `/users` | Only the Users tab | Auth read plus user-list Store read. |
| `/trakt/status` | Metadata/Trakt surfaces | Auth read plus Trakt/config state. |
| `/system/playback-events` | Only the System tab | Auth read; event rows are node-local telemetry. |

This means opening the Libraries tab waits for System diagnostics, users,
Trakt, and up to 2,000 playback events. Parallelism makes the critical path the
slowest endpoint; it does not make unrelated work free.

### Activity is one HTTP request but still uses an all-or-nothing body

[`web/index.html`](../crates/plurxd/src/web/index.html) lines 8927–8953
installs `Loading…`, awaits `/activity/detail`, and paints the full body. Its
request fan-out is lower than Settings and Home, which is consistent with the
761 ms `nynuc` sample, but the route still has no stale-content or sectional
fallback. On `nuc3` it reproduced the same three-second wait as the other two
pages.

The non-overlapping three-second poll and generation fences added by PR #510
are correctness improvements. They prevent detached timers and stale paints;
they do not shorten the initial request.

## 4. PR #510 — correct reductions, incomplete outcome evidence

PR [#510](https://github.com/pjunod/plurx/pull/510) merged as `9f9f4a6b`.
It made these valid improvements:

- Settings reads one settings snapshot instead of approximately 31 individual
  setting keys, with a separate authoritative cache aggregate.
- Activity joins offline package display fields in one Store query instead of
  resolving user, file, and item per package.
- Home overlaps independent recently-added work with progress-derived work and
  overlaps watch lookup with child counts where ordering permits.
- The web client bounds Home preview concurrency, prevents overlapping polls,
  serializes log refreshes, and generation-fences late responses.

PR #510 did not modify `clients/apple` or `clients/android`. The affected Home,
Activity, and Settings views are the HTML application served by `plurxd`, not
the native mobile clients. Treat native-client work as out of scope for this
incident unless a proposed server change alters an existing API contract.
Server-composed page endpoints should be additive, and the existing Apple and
Android contracts must continue to pass unchanged.

The retained validation point documents its own narrower boundary. In
[`validation/points.toml`](../validation/points.toml) lines 389–391,
`cluster.page-reads` says the three-voter gate measures Settings and Activity
**Store primitives**, not complete HTTP pages or network round trips. The web
test in
[`tests/web/page-read-budget.test.js`](../tests/web/page-read-budget.test.js)
checks request concurrency and stale-update behavior, not wall-clock latency.

The gap is therefore not that the tests lied. The gap is that the user-facing
claim was broader than the enforced contract:

| Intended outcome | Existing evidence | Missing evidence |
|---|---|---|
| Home feels faster | Independent Store work overlaps; preview concurrency is capped. | First meaningful paint and fully-settled latency on leader and follower. |
| Activity feels faster | Offline rows use one joined Store query. | Whole `/activity/detail` latency under healthy and degraded quorum. |
| Settings feels faster | Settings snapshot plus cache use two Store calls. | Whole Settings route latency and selected-tab dependency isolation. |
| Clustering does not make navigation blank for seconds | No stale paint or overlapping timer regressions. | Fault-injected quorum/read-confirmation behavior and a nonblank loading contract. |

PR [#513](https://github.com/pjunod/plurx/pull/513) added a topology evidence
contract, PR [#514](https://github.com/pjunod/plurx/pull/514) coalesced auth
activity writes, and PR [#516](https://github.com/pjunod/plurx/pull/516)
removed Store work from metrics scrapes. Those are adjacent foundations. None
repairs an already inconsistent voter or changes the three routes' initial
paint contract.

## 5. Root cause and contributing defects — fix them in dependency order

### P0: `nuc4` has an invalid Raft storage state

`LogIndexNotFound { want: 520000, got: None }` is an OpenRaft defensive storage
failure, not a slow query. `nuc4`'s Raft core repeatedly closes its channel
after rejecting `AppendEntries`; the leader then reports that target as
unreachable while confirming reads.

The review must determine how the state became invalid before recommending a
repair. Candidate classes include snapshot/log-purge ordering, WAL or snapshot
install failure, restart during compaction, filesystem loss, and a vendored
Hiqlite/OpenRaft defect. The logs establish the bad state; they do not establish
which class created it.

Do not start by deleting `nuc4`'s Raft files. Preserve a forensic copy and use
the supported membership contract. Directly editing or partially clearing an
activated data directory can turn one damaged voter into an unprovable
cluster.

### P0: the health surface confuses application heartbeat with Raft health

[`cluster/membership.rs`](../crates/plurx-core/src/cluster/membership.rs) uses a
30-second `last_seen_at` window for the roster's `reachable` boolean. That
answers whether a node recently sent its application heartbeat. It does not
prove that the node can append a Raft entry, install a snapshot, serve a
consistent read, or keep its Raft core alive.

The current UI therefore presents `nuc4` as reachable at the same time its
Raft storage rejects every append. The word is locally correct but
operationally misleading. The Cluster panel needs separate signals such as:

- process heartbeat reachable;
- Raft transport reachable;
- replication/apply healthy;
- snapshot/log storage healthy; and
- last successful quorum confirmation.

A single green `reachable` label must not summarize all five.

### P1: the probe loop retries a failing authority path every 500 ms

[`cluster/membership.rs`](../crates/plurx-core/src/cluster/membership.rs) lines
1916–1932 sleeps 500 ms, calls `answer_offline_source_probes`, and logs a
warning on every failure. On the live node this produced about 1.8 warnings per
second indefinitely.

The loop needs failure-aware backoff and a deduplicated diagnostic that retains
the useful internal cause. The 500 ms cadence is justified while a real node
removal waits for probes; it is not justified for an idle loop that cannot
read its replicated queue. Preserve quick response to actual pending work, but
do not hammer a known-failing authority path forever.

### P1: page models couple unrelated data to first paint

Even after Raft repair, the current rendering contract is fragile. Home and
Settings should render useful stable content before optional or off-tab data
arrives. A slow Trakt integration, playback-event query, System diagnostic, or
one library preview should not blank the entire page.

This is a web architecture defect, not merely a spinner choice. Replacing the
word `Loading…` with a skeleton leaves the same critical path. The fix must
move dependency boundaries so sections or active panels can commit
independently while retaining the generation fences already added.

### P2: connection churn may amplify the broken-quorum load

The Cluster log showed many accepted Hiqlite SQLite WebSocket streams while the
pages loaded. Review the vendored client connection lifecycle and establish
whether requests reuse a bounded connection, reconnect per query, or reconnect
because the Raft core is failing. Instrument before changing pooling; a large
log volume alone does not prove connection setup dominates latency.

## 6. Remediation plan — restore correctness before tuning latency

### 6.1 Preserve evidence and make the fleet attributable

Before changing membership or local Raft state:

1. Record the roster, leader term, commit/applied indexes, snapshot metadata,
   and exact build identity from all four nodes.
2. Preserve forensic copies of every relevant activated-node data directory
   using a procedure the reviewer confirms is consistent with the existing
   cluster runbook. A copy is evidence; it is not automatically a supported
   restore point.
3. Capture the first occurrence of `LogIndex(520000)` and the deploy/restart
   boundary around it, rather than only the current error storm.
4. Complete a uniformly stamped rollout if the cluster remains safe to roll.
   If rolling a damaged voter could worsen quorum, stop and establish the
   recovery sequence first.

**Acceptance:** every node's running revision is attributable, and the review
can explain which evidence survives each proposed recovery step.

### 6.2 Recover or replace `nuc4` only through a reviewed quorum-safe path

The existing supported model removes a follower through the membership API,
stops it, discards the tombstoned data directory, and rejoins from a fresh
directory with a new single-use token. Whether that path is safe *in this
specific state* is a review question: removal itself requires a functioning
quorum and the offline-work resolution path is currently failing.

The reviewer must answer:

- Can the remaining three voters prove a healthy quorum without `nuc4`?
- Can the membership removal transaction complete while the probe loop's
  replicated read fails?
- Does `nuc4` own active offline work or singleton job leases that must be
  resolved or fenced?
- Is a controlled process stop before removal safe, or would it remove an
  acknowledgement the four-voter quorum still needs?
- Is there a supported snapshot-install recovery that preserves identity, or
  is remove-and-fresh-rejoin the only safe path?

**Acceptance:** the chosen procedure has an explicit rollback boundary, names
which directories are retained, and never asks a minority to force itself into
a new cluster.

### 6.3 Make cluster health fail loudly and specifically

Add a health projection that distinguishes heartbeat reachability from Raft
read/write/apply health. Surface the exact stable category without dumping
paths or opaque library rows into the UI. Deduplicate repeated identical
errors, but retain first seen · last seen · count · affected Raft id.

The readiness contract should become false when the node cannot safely serve
the authoritative operations assigned to it. A process answering HTTP while
its Raft core rejects every append is alive, not ready.

**Acceptance:** a fault-injected missing-log or closed-Raft-core condition
cannot render `Redundant` plus four green reachability labels without an
adjacent degraded verdict and actionable cause.

### 6.4 Decouple first paint from complete page hydration

Preserve the existing route generation guards and timer ownership. Change only
which model boundaries are awaited before a section commits.

**Home:** render cached or newly returned hubs first · render each category or
library preview as its request resolves · make Coming Soon independently
optional · retain the previous Home body during refresh where safe.

**Settings:** render the tab bar immediately · fetch the active tab's required
data first · lazy-load other tabs on selection · do not fetch System playback
events before painting Libraries · retain the last safe snapshot while a
refresh runs.

**Activity:** retain the last body during its poll · show a small stale or
reconnecting state when refresh is slow · replace the whole body only on the
first visit when no prior snapshot exists.

**Acceptance:** delaying any one optional endpoint by three seconds does not
delay unrelated first content by three seconds, and navigating away still
prevents late responses from repainting the new route.

### 6.5 Reduce authority round trips without weakening authorization

Review server-composed page endpoints or request-scoped batching before adding
client-side caches. One authenticated request that returns a coherent Home or
active Settings panel can pay one token authorization decision and batch its
Store reads, while the browser still hydrates optional sections separately.

Do not introduce a TTL-only authorization cache. If an authorization cache is
proposed, require a replicated generation or revocation fence and demonstrate
immediate token deletion, password reset, user deletion, and admin demotion.

**Acceptance:** the number of authority reads per first-paint model is bounded
independently of library count, and all existing revocation contracts remain
authoritative.

## 7. Guardrails — changes that would make the incident easier to hide

- **Do not serve stale authorization.** Fast revoked access is a security
  regression, not a performance improvement.
- **Do not call heartbeat reachability cluster health.** Keep the narrower
  signal, but label it narrowly and add Raft health beside it.
- **Do not automate removal of the fourth voter.** Three voters are the usual
  topology, but selecting and removing a live voter is an operator decision
  with offline-work and quorum consequences.
- **Do not delete or edit `nuc4`'s Raft state before preserving evidence and
  approving the recovery sequence.** The missing index may be the only clue to
  a snapshot/purge defect that can recur on every replacement node.
- **Do not treat an activated-node SQLite copy as a supported restore point.**
  [OPERATIONS.md](OPERATIONS.md) explicitly distinguishes legacy pre-activation
  snapshots from authoritative clustered state.
- **Do not replace `Loading…` with a prettier blocking placeholder and declare
  the UX fixed.** First meaningful content must move earlier.
- **Do not use Store-call counts as the sole acceptance criterion.** Keep those
  deterministic gates, then add HTTP and browser outcome evidence.
- **Do not tune timeouts first.** A shorter timeout can turn a three-second page
  into an error page while leaving the broken voter and false health signal
  intact.

## 8. Acceptance matrix — prove recovery, health truth, and user outcome

The exact budgets below are proposed review targets, not existing guarantees.
Opus should tighten or replace them using baseline evidence rather than delete
the timing gate.

| Surface | Scenario | Proposed acceptance |
|---|---|---|
| Raft correctness | Healthy three- or four-voter cluster | Zero `LogIndexNotFound`, closed-core, or leadership-confirmation errors during a 10-minute mixed read/write run. |
| Replica recovery | Rejoined replacement voter | Snapshot/install completes, applied index catches the leader, restart remains caught up, and old tombstoned identity is refused. |
| Health UI | Broken follower storage | Degraded verdict within two health samples; heartbeat reachability remains separately visible. |
| Probe loop | Authority read failure for 10 minutes | Bounded exponential backoff, one summarized diagnostic, no 500 ms warning storm; an actual pending removal still receives a prompt probe after recovery. |
| Home first paint | Healthy LAN cluster, warm navigation | 30 samples per leader/follower host; p95 first meaningful content at or below 500 ms. |
| Home settled | Same | p95 optional-section completion at or below 1,000 ms; a delayed optional integration does not move first paint. |
| Activity first paint | Same | 30 samples per host; p95 at or below 500 ms, with previous content retained during polls. |
| Settings first paint | Same | Tab bar immediately; active panel p95 at or below 500 ms; inactive-tab endpoints absent from the first-paint request log. |
| One voter unavailable | Supported odd-voter topology | Page p95 remains below 1,000 ms while quorum is healthy; health reports the missing voter. |
| Navigation races | All three routes | Late responses and old timers cannot repaint or poll after navigation. |
| Authorization | Cache/batching proposal, if any | Token deletion, password reset, user deletion, and admin demotion take effect without a stale-authority window. |

Keep deterministic CI and named-runner evidence separate:

- `make check` proves formatting, lint, unit, contract, and documentation
  integrity.
- `make web-check` proves shipped browser behavior and static request
  contracts.
- `make cluster-check` proves replicated semantics and fresh-cluster topology
  behavior.
- A named four-machine run proves LAN percentiles, failover behavior, resource
  use, and the repaired production-shaped topology. GitHub-hosted CI cannot
  honestly prove those physical latency claims.

## 9. Evidence collection — safe, repeatable, and free of credentials

Run these against a controlled environment or through the existing deployment
tooling. Do not paste bearer tokens into a shared review document.

```bash
# Public build identity; repeat for every host.
curl -fsS http://HOST:32400/api/v1/server | jq '{build,built_at,uptime_seconds}'

# Quantify the established failure signatures over the same bounded window.
docker logs --since 2m plurxd 2>&1 \
  | rg -c 'LogIndexNotFound|timeout while confirming leadership for read request'

# Preserve the first and last matching lines without dumping unrelated rows.
docker logs --since 30m plurxd 2>&1 \
  | rg 'LogIndexNotFound|leadership for read request|offline source probe pass failed'

# Code-level gates after a fix.
make check
make web-check
make cluster-check
```

For browser evidence, record per sample:

| Field | Reason |
|---|---|
| build and host | Separates client revision and leader/follower placement. |
| route and selected Settings tab | Defines the dependency set. |
| click-to-shell, click-to-first-content, click-to-settled | Prevents one aggregate duration from hiding a blocked first paint. |
| endpoint start/end/status | Identifies the slowest dependency. |
| authority/local/write Store operation durations | Connects HTTP latency to the replicated boundary. |
| Raft term, leader id, and health verdict | Detects elections and unhealthy samples rather than averaging them into the baseline. |

**How to read it:** healthy samples establish product latency; unhealthy
samples establish degradation behavior. Do not merge them into one percentile
and call the result normal clustering overhead.

## 10. Requested Opus output — a review that can become work

Return these sections in order:

1. **Verdict:** confirm, partially confirm, or reject the stated primary root
   cause. Name the strongest contradicting evidence if rejecting it.
2. **P0 recovery findings:** the exact safe sequence for `nuc4`, required
   backups/evidence, rollback boundary, and any reason not to use the normal
   remove-and-rejoin path.
3. **P0 correctness findings:** defects in Raft persistence, snapshot/log
   purge, version compatibility, or health projection that can recur after
   replacement.
4. **P1 latency findings:** page-model, endpoint, authentication, connection,
   and Store-call changes, ordered by expected user-visible effect.
5. **P1 validation findings:** tests that would have failed before PR #510 was
   presented as a page-latency fix.
6. **P2 follow-ups:** observability, topology, and cleanup that should not block
   restoring correctness.
7. **Decision table:** for each proposed destructive or availability-affecting
   operation, state prerequisites · blast radius · rollback · operator
   approval point.

## 11. Source map — re-verify these boundaries before implementation

Line numbers refer to `origin/main` at `a0f9fc14` and will move.

| Boundary | Source |
|---|---|
| Home request waves and full-body loading | [`crates/plurxd/src/web/index.html`](../crates/plurxd/src/web/index.html), lines 3596–3667 |
| Activity loading and poll | [`crates/plurxd/src/web/index.html`](../crates/plurxd/src/web/index.html), lines 8927–8953 |
| Settings seven-request gate | [`crates/plurxd/src/web/index.html`](../crates/plurxd/src/web/index.html), lines 9108–9123 |
| Per-request token authorization | [`crates/plurxd/src/http/extract.rs`](../crates/plurxd/src/http/extract.rs), lines 175–188 |
| Timed consistent-query boundary | [`crates/plurx-core/src/store/hiqlite.rs`](../crates/plurx-core/src/store/hiqlite.rs), `query_consistent_map` |
| Hiqlite token lookup | [`crates/plurx-core/src/store/hiqlite.rs`](../crates/plurx-core/src/store/hiqlite.rs), `user_for_token` |
| Heartbeat-derived reachability | [`crates/plurx-core/src/cluster/membership.rs`](../crates/plurx-core/src/cluster/membership.rs), `NODE_REACHABLE_WINDOW_MS` and membership projection |
| Failing 500 ms source-probe loop | [`crates/plurx-core/src/cluster/membership.rs`](../crates/plurx-core/src/cluster/membership.rs), lines 1916–1932 |
| Existing narrow validation claim | [`validation/points.toml`](../validation/points.toml), `cluster.page-reads` |
| Store-call regression evidence | [`validation/regressions.d/712c44cf-cluster-page-reads.toml`](../validation/regressions.d/712c44cf-cluster-page-reads.toml) |
| Web request/race regression evidence | [`validation/regressions.d/9a0ebf1e-web-experience.toml`](../validation/regressions.d/9a0ebf1e-web-experience.toml) |
