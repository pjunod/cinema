# Cluster page latency fix plan — restore quorum truth, then unblock first paint

**Status:** review reconciled · **Executes:** findings from
[CLUSTER_PAGE_LATENCY_REVIEW.md](CLUSTER_PAGE_LATENCY_REVIEW.md) and decisions
from
[CLUSTER_PAGE_LATENCY_FIX_PLAN_REVIEW.md](CLUSTER_PAGE_LATENCY_FIX_PLAN_REVIEW.md)
· **Written:** 2026-08-22 · **Re-pinned:** 2026-08-22 · **Code:**
`origin/main` at `64e96aa7`

Read the review brief and adversarial review first, then
[OPERATIONS.md](OPERATIONS.md) sections on readiness and voter removal. Execute
this plan milestone by milestone. The reconciliation in §2.2 is controlling:
extend the cluster-performance primitives already on `main`; do not build a
second observer, watermark, Store timer, or cluster lifecycle harness. Do not
combine the production recovery, WAL repair, health projection, and web
hydration changes into one pull request because each has a different rollback
boundary. If any step appears to require editing Raft files, restoring an
activated node from `plurx.db`, forcing a minority to reconfigure, or weakening
authorization consistency, stop and return it for operator review.

This plan deliberately contains two kinds of work:

- an operator-approved recovery of the live damaged voter; and
- code changes that prevent recurrence, report degradation truthfully, and
  keep one slow dependency from blanking an entire page.

The first removes the immediate three-second retry tax. The second makes the
result durable.

## 1. Objective — fix the incident and the design that exposed it

The work is complete only when all four outcomes below are true.

| Outcome | Required result |
|---|---|
| Cluster correctness | The production-shaped cluster has no missing-log, closed-Raft-core, or leadership-confirmation failures, and every retained voter converges after restart. |
| Health truth | The UI never equates a recent application heartbeat with healthy Raft replication; `/readyz` and the Cluster panel agree about a degraded serving node. |
| Bounded page work | Home performs a constant number of first-load HTTP requests independent of library count; Settings fetches only its active tab; Activity remains one request. |
| User-visible latency | Home, Activity, and Settings show useful route content without waiting for unrelated or optional dependencies, and meet the reviewed LAN percentile budgets in §1.1. |

The primary target topology is three voters. Four voters require three
acknowledgements, tolerate the same one failure as three voters, and add work
without adding ordinary-HA resilience. Removing the damaged fourth voter is
an operator action, not an optimization hidden inside a deploy.

```text
live incident
     │
     ├── damaged `nuc4` WAL / Raft state
     │        │
     │        └── leader confirmation retries ──▶ request reaches 3 s timeout
     │
     └── full-body loading contract
              │
              └── slowest request ──────────────▶ blank page for the same 3 s

target state
     │
     ├── three healthy voters + crash-consistent purge/restart proof
     ├── topology, heartbeat, and replication shown as separate facts
     ├── constant-cardinality Home API + active-tab Settings loading
     └── shell ──▶ first content ──▶ optional sections, each generation-fenced
```

### 1.1 Proposed latency budgets require reviewer ratification

These are go/no-go targets, not claims about the current product. Keep or
replace them during review, but do not remove the percentile gate.

| Surface | Healthy three-voter LAN target | Degraded-but-quorate target |
|---|---:|---:|
| Home first meaningful content | p95 ≤ 500 ms | p95 ≤ 1,000 ms |
| Home fully settled, excluding a deliberately delayed optional integration | p95 ≤ 1,000 ms | p95 ≤ 1,500 ms |
| Activity first meaningful content | p95 ≤ 500 ms | p95 ≤ 1,000 ms |
| Settings tab bar | same browser turn | same browser turn |
| Settings active panel | p95 ≤ 500 ms | p95 ≤ 1,000 ms |

Each hardware result uses 30 warm samples per route per target role, reports
p50 · p95 · max · failures, and retains every raw sample. Means are not an
acceptance metric because they hide the pauses the user notices.

### 1.2 Non-goals keep the fix inside its evidence

- **Do not add a time-only authorization cache.** Token deletion, password
  reset, user deletion, and admin demotion must remain authoritative on the
  next request.
- **Do not change native mobile UI code.** The affected views are the HTML app
  embedded in `plurxd`. Apple and Android remain API compatibility consumers,
  not implementation targets for this incident.
- **Do not automate voter removal.** The system may explain why three voters
  are preferable, but only an operator selects and removes a machine.
- **Do not lower the three-second Store timeout to make graphs look faster.**
  That converts a slow page into an error without repairing the replica.
- **Do not serve catalogue or watch state from an unboundedly stale replica.**
  Bounded-replica reads remain the separate contract in
  [CLUSTER-PERFORMANCE-PLAN.md](CLUSTER-PERFORMANCE-PLAN.md).
- **Do not call a skeleton a latency fix.** A placeholder is useful only when
  independent content can commit behind it.
- **Do not add a database migration for this work.** The health projection is
  process state, the Home endpoint is additive, and the web hydration model is
  client state.

## 2. Starting contract — preserve these boundaries while changing the path

Line numbers refer to `origin/main` at `64e96aa7`; re-verify every boundary at
implementation time.

| Boundary | Current contract | Required preservation |
|---|---|---|
| Request authorization | [`AuthUser`](../crates/plurxd/src/http/extract.rs) calls `Store::user_for_token`; Hiqlite uses a consistent query. | No stale-authority window and no route bypass. |
| Store deadline | [`TimedClient`](../crates/plurx-core/src/store/hiqlite.rs) bounds every Hiqlite operation at three seconds. | A wedged leader cannot hold an HTTP task forever. |
| Readiness | `GET /readyz` performs `Store::ping`, including an authority `SELECT 1`. | Liveness stays Store-free; readiness remains an active serving proof. |
| Roster reachability | [`MembershipManager::status`](../crates/plurx-core/src/cluster/membership.rs) sets `reachable` from a 30-second heartbeat window. | Keep the heartbeat fact, but label it as heartbeat rather than Raft health. |
| Replication projection | [`ReplicationMonitor`](../crates/plurx-core/src/cluster/migration.rs) classifies local running state, leader visibility, apply lag, and leader-visible peer lag; its merged passive observer and quorum watermark publish validity-gated local/apply facts. | Reuse the merged proof sources, do not invent peer certainty on a follower, and preserve privacy-safe output. |
| Voter removal | `DELETE /api/v1/cluster/nodes/{node_id}` fences work, resolves offline ownership, commits membership, and tombstones the identity. | Never bypass quorum, offline-work, leader, or job-owner fences. |
| Home | `loadHome` waits for `/libraries`, `/hubs`, and `/coming-soon` together, then starts one preview request per library and waits for all of them before `viewHome` paints. | Preserve grouping, ordering, failure isolation, 401 behavior, and generation fences. |
| Activity | `viewActivity` installs `Loading…`, awaits `/activity/detail`, then owns one non-overlapping poll. | Preserve one-request cardinality, poll ownership, and stale-response fences. |
| Settings | `viewSettings` waits for seven endpoints before rendering any tab. | Preserve admin gating, join-token lifetime, mutation behavior, and tab-specific polling. |
| Native clients | Existing Apple and Android code consumes existing APIs. | Add endpoints; do not change or remove existing response contracts. |

### 2.1 Facts the implementation must not turn back into hypotheses

- `nuc4` repeatedly reported `LogIndexNotFound` for index `520000`.
- The leader repeatedly failed leadership confirmation against Raft id `3`.
- The observed route delays cluster around the Store timeout, not ordinary LAN
  consensus cost.
- The vendored WAL already carries restart-recovery patches for missing purge
  metadata and interrupted metadata replacement. The new failure is therefore
  adjacent to a known sharp edge and must be reproduced, not dismissed as a
  generic bad disk.
- Current WAL purge code mutates physical WAL state and then publishes
  `last_purged_log_id`. A crash between those operations is a candidate path
  to retained metadata naming a log that no longer exists. This is a design
  hypothesis until the captured voter or a deterministic failpoint proves it.

### 2.2 Merged cluster-performance work is an input, not duplicate scope

The re-pin includes PRs #517, #518, and #519 from
[CLUSTER-PERFORMANCE-PLAN.md](CLUSTER-PERFORMANCE-PLAN.md). Those commits change
how PR-A, PR-C, and PR-D are built:

| Milestone | Merged source of truth | This plan adds |
|---|---|---|
| PR-A | P0b's `cluster-topology` artifact supplies the versioned provenance, `semantic_ci` versus `named_runner` evidence split, Type-7 percentile rule, raw-sample retention, and resource-disclosure convention. P2f remains unlanded. | A browser page-outcome artifact that follows those conventions. It does not start a cluster, replace the topology artifact, or create a second cluster lifecycle harness. |
| PR-C | P2c's `PassiveRaftMetricsView` supplies validity-gated local term, leader, and applied-index facts. P2d's quorum watermark supplies a validity-gated quorum-confirmed commit index and apply lag. | An additive public projection and truthful UI wording. The existing leader metrics map remains the only source for every-peer confirmation because the watermark proves quorum, not an all-peer count. No request performs a new Store read or peer fan-out. |
| PR-D | P2b's fixed-label `plurx_store_operation_seconds` histogram exposes local-read, authority-read, and write outcomes. | Backoff and bounded diagnostics for the existing probe loop. Correlate it with P2b rather than adding a duplicate Store timer; add a probe-specific metric only if a reviewed operational question cannot be answered from the bounded logs and existing histogram. |

The page-read work remains separate from bounded-replica reads. Authentication,
settings, offline ownership, and removal fences continue to use authority
semantics.

## 3. Target contracts — the interfaces the milestones converge on

### 3.1 WAL inspection is read-only and emits no application data

Add a diagnostic command under the existing cluster-check binary:

```text
plurx-cluster-check inspect-wal \
  --hiqlite-dir <stopped-forensic-copy>/hiqlite \
  --output target/validation/wal-inspection.json
```

The command must refuse a live-locked directory, open files read-only, and
never start Hiqlite or contact cluster peers. Its versioned JSON contains:

| Field | Meaning |
|---|---|
| metadata CRC and format version | Whether `meta.hql` is structurally valid. |
| `last_purged_log_id_present` | Whether the purge boundary is absent or stale-non-`None`; this distinguishes the already-covered first-purge reconstruction case from the uncovered repeat-purge crash window. |
| decoded `last_purged_log_id` | The boundary OpenRaft is told no longer exists. |
| WAL number · first index · last index · byte size · file hash | Whether retained files have gaps, overlap, or unexpected replacement. |
| first and last decodable retained log id | Whether headers agree with actual entries. |
| snapshot last-log id | The durable state the purge was allowed to cover. |
| local applied index | Whether the state machine is ahead of, equal to, or behind the claimed purge. |
| invariant verdicts | Stable codes such as `metadata_behind_wal`, `retained_gap`, `snapshot_gap`, or `clean`. |

It must not emit SQL rows, media paths, token hashes, cluster secrets, API
addresses, or raw log payloads. A file hash proves identity without publishing
its contents.

### 3.2 WAL purge must make every crash boundary restartable

The candidate repair is metadata-first, physical-cleanup-second:

```text
snapshot is already durable
          │
          ▼
flush + sync the active WAL header when its durable end advances
          │
          ├── preserves the existing append-side hole guard
          ▼
stage + fsync new purge boundary
          │
atomic rename + directory fsync
          │
          ├── crash here ──▶ extra old WAL bytes remain, metadata is safe
          ▼
remove/truncate covered WAL bytes
          │
sync affected headers + directory
          │
          └── crash here ──▶ restart sees a complete purge or safe extra bytes
```

The reviewer must confirm this ordering against OpenRaft 0.9's storage
contract. Advancing purge metadata before deleting already-snapshotted bytes
is expected to be safe because extra bytes below the authoritative purge
boundary are disposable; the reverse ordering can expose a missing required
entry. If the WAL reader cannot tolerate those extra bytes, startup must prune
them idempotently after it has accepted the metadata boundary.

The active-header flush currently preceding physical removal in
`Action::Remove` remains in place and remains ordered before removal. It guards
the independent append-side hole between a snapshot boundary and the latest
entry. The metadata-first experiment may move `Metadata::write`; it must not
delete, weaken, or move that header flush behind physical cleanup.

Do not add an automatic production repair that guesses a new purge id from a
hole. Reconstruction is allowed only when the snapshot, metadata, and retained
entry identities prove one unique boundary. Ambiguity must remain a startup
refusal with a diagnostic code.

### 3.3 Replication status says what scope it actually proves

Keep the existing fields and add a bounded, privacy-safe proof scope. Proposed
Rust shapes:

```rust
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplicationScope {
    Local,
    Cluster,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplicationReason {
    RaftNotRunning,
    LeaderUnknown,
    TwoVoterReconfiguration,
    LocalApplyLag,
    PeerLag,
    PeerUnknown,
    MetricsUnavailable,
}
```

Add these fields to `ReplicationStatus`:

```rust
pub scope: ReplicationScope,
#[serde(skip_serializing_if = "Option::is_none")]
pub reason: Option<ReplicationReason>,
pub voter_count: usize,
#[serde(skip_serializing_if = "Option::is_none")]
pub confirmed_peer_count: Option<usize>,
```

`scope = cluster` only when the validity-gated quorum watermark agrees with the
local passive term/leader observation and the existing leader metrics sample
contains replication entries for the configured peers. A follower that has
applied through a valid quorum watermark reports `scope = local`; it proves its
own apply lag, not every peer's health. `confirmed_peer_count` is absent for
local scope and is counted from the existing leader replication map for
cluster scope. It cannot be derived from `DbQuorumWatermark`, which proves a
quorum-confirmed index but carries no every-peer roster.

The existing `ClusterAvailability` remains topology arithmetic. The web banner
uses topology plus replication proof:

| Topology | Replication proof | Banner |
|---|---|---|
| Three or more voters | cluster scope · in sync · every peer confirmed | `Redundant and in sync — N voters`, green. |
| Three or more voters | degraded | `Redundancy configured — replication degraded`, warning. |
| Three or more voters | local scope only | `Redundancy configured — verify cluster health on the leader`, neutral. |
| Two voters | any | Existing reconfiguration warning; never redundancy. |
| One voter | in sync | Existing supported one-node wording. |

The node table keeps the existing JSON `reachable` field for compatibility,
but labels the column **Heartbeat** and renders **seen recently** or **stale**.
It must not color a recent heartbeat as proof of Raft storage health.

### 3.4 Home request cardinality is constant in library count

Add an authenticated, web-oriented endpoint without changing `/hubs`,
`/libraries`, or `/libraries/{id}/items`:

```text
GET /api/v1/home/previews?limit=24
```

Proposed response:

```rust
#[derive(Serialize)]
pub struct HomePreviews {
    pub libraries: Vec<HomeLibraryPreview>,
}

#[derive(Serialize)]
pub struct HomeLibraryPreview {
    pub library: LibraryDto,
    pub items: Vec<ItemDto>,
    pub total: i64,
}
```

Add one Store primitive whose call count does not grow with the library
roster:

```rust
async fn home_preview_pages(
    &self,
    limit_per_library: i64,
) -> Result<Vec<HomePreviewPage>, StoreError>;
```

`HomePreviewPage` is a domain/store result keyed by `library_id`; it does not
contain HTTP DTOs. Both SQLite and Hiqlite use one windowed item query:

```sql
WITH ranked AS (
    SELECT items.*,
           COUNT(*) OVER (PARTITION BY library_id) AS library_total,
           ROW_NUMBER() OVER (
               PARTITION BY library_id
               ORDER BY added_at DESC, id DESC
           ) AS preview_rank
    FROM items
    WHERE kind IN ('movie', 'show', 'book', 'audiobook')
       OR (kind IN ('folder', 'video', 'photo') AND parent_id IS NULL)
)
SELECT <item columns>, library_total, preview_rank
FROM ranked
WHERE preview_rank <= $1
ORDER BY library_id, preview_rank;
```

Re-verify the exact item-column list and top-level predicate against
[`Store::list_top_items_in_genre`](../crates/plurx-core/src/store/mod.rs) at
build time. The implementation must share the predicate or test byte-for-byte
semantic parity so the Home preview cannot drift from a library sorted by
`added`.

The handler obtains libraries once, then annotates all preview items with the
existing page-wide `watch_map` · `item_max_heights` · `child_counts` ·
`watch_rollups` primitives. The number of authority calls is therefore bounded
by a small constant independent of `L`, the number of libraries.

The browser's Home load becomes exactly three top-level requests:

```text
/hubs ───────────────▶ first content
/home/previews ──────▶ category/library sections
/coming-soon ────────▶ optional rail, never blocks either result
```

Each request performs its own authoritative authentication. That is three
bounded decisions, not `3 + L`, and it preserves immediate revocation.

### 3.5 Settings loads one tab contract at a time

Replace the seven-request `Promise.all` with an explicit dependency manifest:

| Tab | Required before panel paint | Secondary after paint |
|---|---|---|
| Libraries | `/settings` · `/libraries` · `/scan/status` | none |
| Metadata | `/settings` · `/trakt/status` | `/libraries` only for the backfill notice/action |
| Playback | `/settings` | none |
| Users | `/users` | none |
| System | `/system` | `/system/playback-events` · log refresh |
| Cluster | `/cluster/nodes` | cluster log refresh |

Add process-local browser state, not durable state:

```javascript
let SETTINGS_DATA = {};
let SETTINGS_LOADED = new Set();
let SETTINGS_LOADS = new Map();

async function loadSettingsTab(tab, generation) { /* coalesced per tab */ }
```

`SETTINGS_LOADS` coalesces repeated clicks on the same tab. Every response
checks the current route, page generation, and selected tab before committing.
Changing tabs still calls `forgetJoinToken()` before any load begins. A tab
renders its bar and a panel-local loading state immediately; it never replaces
the entire Settings route with `Loading…`.

### 3.6 Activity and Home retain useful content during refresh

Use one shared page-phase contract in the web app:

```javascript
function setPagePhase(route, generation, phase) {
  // Commit only when route and PAGE_RENDER_GENERATION still match.
  // Set #main.dataset.page and #main.dataset.phase.
}
```

Allowed phases are `shell` · `content` · `settled`. The named browser harness
observes those attributes instead of guessing from text or screenshots.

Home commits the existing shell immediately, `/hubs` as first content, preview
sections when the batched response arrives, and Coming Soon independently.
Activity keeps the last successful detail body during polls, marks it stale
after a refresh failure, and replaces the whole body only on a first visit
with no snapshot. A 401 still follows the existing global logout path rather
than displaying cached protected data as current.

## 4. Delivery order — one reviewable boundary per pull request

| Order | Deliverable | Depends on | Rollback |
|---:|---|---|---|
| OP-0 | Preserve evidence and remove the damaged fourth voter through the supported API. | Explicit operator approval. | Before membership commit, restart the unchanged voter; after commit, the old identity is permanently tombstoned. |
| PR-A | Page-latency measurement artifact and deterministic phase tests, extending P0b/P2f evidence conventions. | Merged topology artifact contract; no runtime dependency. | Remove the browser runner; no runtime behavior changes. |
| PR-B1 | Read-only WAL forensic inspector and synthetic invariant fixtures. | Preserved copy or synthetic layouts. | Remove the command; it never changes a WAL. |
| EV-B | Run PR-B1 against the preserved copy and record only its privacy-safe verdict. | Operator access to the stopped forensic copy. | Evidence-only; the copy remains byte-for-byte preserved. |
| PR-B2 | Deterministic crash reproducer and the smallest repair matching EV-B. | EV-B identifies the crash window, or an equivalent deterministic synthetic reproducer does. | Revert code only if the on-disk format remains unchanged; never restore the damaged live directory into membership. |
| PR-C | Proof-scoped replication status and truthful Cluster UI wording. | PR-A for outcome evidence; merged P2c passive observer and P2d quorum watermark. | Extra JSON fields are additive; old web code ignores them. |
| PR-D | Probe-loop failure backoff and deduplicated diagnostics. | Merged P2b Store metrics; PR-C reason vocabulary where useful. | Restore fixed cadence; no durable format change. |
| PR-E | Constant-cardinality `/home/previews` endpoint and Store primitive. | Existing Store-call gate plus the PR-A request artifact. | Web does not use it yet; revert safely. |
| PR-F | Incremental Home/Activity hydration and active-tab Settings loading. | PR-E. | Revert the embedded web app while leaving the additive endpoint in place. |
| REL-1 | Named-runner evidence, rolling deploy, and production observation. | All accepted PRs. | Roll back runtime code one voter at a time; do not undo committed OP-0 membership with the old identity. |

Do not stack PR-B1 through PR-F as one review unit. Stacking branches is fine;
merging them without their individual gates is not.

## 5. OP-0 — recover the live cluster without destroying the evidence

This milestone changes production availability and membership. The plan is
not authorization to execute it.

### 5.1 Preflight proves the remaining majority before touching `nuc4`

From direct, node-specific HTTP addresses:

1. Confirm the roster still maps `nuc4` to Raft id `3` and another voter is
   leader.
2. Confirm the other three voters run attributable builds and answer
   `/healthz` plus `/readyz` twice, ten seconds apart.
3. On each survivor, require valid local and watermark samples, zero apply lag,
   converged applied indexes, and no local WAL or state-machine error. Use the
   merged P2c/P2d metrics rather than inferring convergence from heartbeat.
4. Audit offline work before removal. `settle_offline_work` performs
   authority-backed reads and writes, and the survivor probe loops use that
   same replicated Store path. If any probe pass still reports
   `membership_internal`, or the Store histogram records failures during the
   preflight window, stop: the removal path is not healthy enough to commit.
5. Stop new offline-package admissions to `nuc4` at the routing layer and let
   active transfers finish. The removal API still owns the final resolution;
   routing quiets the race rather than bypassing it.
6. Open a maintenance window. Three surviving voters are exactly the quorum of
   the current four-voter configuration; another failure during removal stops
   progress.

**Stop condition:** if fewer than three non-target voters are ready, if either
Raft sample is invalid, if the probe/removal Store path is failing, if the
target is the leader, or if two voters show independent storage faults, do not
remove anything. Preserve all directories and recover the original majority
as [OPERATIONS.md](OPERATIONS.md) requires.

**Acceptance:** a timestamped preflight record names the four node ids, Raft
ids, leader, builds, applied indexes, sample validity, apply lag, readiness
results, probe health, Store failure delta, and active offline-work count
without including tokens or media paths.

### 5.2 Preserve a stopped forensic copy before membership changes

Stop only `nuc4`, confirm its process and data-directory lock are gone, then
copy its complete authoritative set described in
[OPERATIONS.md](OPERATIONS.md): `hiqlite/` · `node.id` · `membership.json` ·
cluster secrets · activation/readdress markers · migration state · credential
key. Preserve owner, mode, timestamps, links, filesystem metadata, and a file
manifest with hashes.

The copy remains offline. Do not start `plurxd` against it, because its
membership and secrets still name the live cluster. Run only the read-only
inspector from PR-B1, or inspect a second disposable copy while preserving the
first byte-for-byte.

**Acceptance:** the source is stopped, the forensic copy hash manifest
verifies, and the original directory has not been edited or partially cleared.

### 5.3 Commit removal through the supported follower path

Target a known surviving node directly and use the node id, not Raft id:

```bash
curl -fsS -X DELETE \
  "$PLURX_SURVIVOR/api/v1/cluster/nodes/$NUC4_NODE_ID" \
  -H "Authorization: Bearer $PLURX_ADMIN_TOKEN" | jq .
```

The environment variables above are placeholders; never put their values in a
shared plan or shell history. The API must be allowed to refuse leader removal,
quorum loss, or unresolved offline work. Handle those stable refusals through
the existing runbook. Do not delete Raft files to make the request pass.

If the HTTP result is ambiguous, read the current roster and pending-removal
diagnostics before retrying. The membership implementation is designed to
reconcile an interrupted outcome; an operator must not infer failure from one
lost response.

**Irreversible boundary:** once the membership change commits, `nuc4`'s old
identity and directory are tombstoned. The rollback is a fresh join with a new
token and fresh directory, not restarting the old copy.

**Acceptance:** the committed roster contains exactly three voters, no pending
removal remains, offline work has moved or failed with its documented code,
and the stopped `nuc4` process cannot re-enter membership.

### 5.4 Hold at three voters and measure before adding anything

Do not immediately rejoin the fourth machine. Three is the ordinary HA target
and still tolerates one loss. Run ten minutes of mixed authenticated reads and
writes, cross at least one restart of a non-leader, and collect the PR-A page
baseline.

**Acceptance:** zero missing-log · closed-core · leadership-confirmation
errors; all three applied indexes converge; `/readyz` stays healthy; Home,
Activity, and Settings no longer cluster at the three-second Store deadline.

## 6. PR-A and PR-B — measure first, then prove the recurrence path

### 6.1 PR-A adds a page outcome artifact, not a noisy CI stopwatch

Add:

- `scripts/cluster-page-latency` — Playwright driver using the repository's
  pinned browser runtime;
- `benchmarks/cluster-page-latency.schema.json` — Draft 2020-12 artifact shape;
- deterministic unit tests for percentile math, redaction, route/phase order,
  and malformed samples; and
- a validation point mapping the harness, schema, embedded web markers, and
  tests.

This is a page-outcome extension of the existing cluster evidence contract,
not another topology harness. Reuse P0b's `schema_version` · `build_sha` ·
`evidence_scope` · raw-sample · Type-7 percentile · resource-disclosure
vocabulary. The script targets an already-running deployment and never starts,
joins, stops, or reconfigures voters. Map its audited files to the existing
`cluster.page-reads` functionality point rather than creating a competing
cluster-performance point.

The runner accepts a base URL, an owner-only browser storage-state file,
sample count, and output path. It never accepts a bearer token on the command
line and never writes cookies, authorization headers, response bodies, media
paths, node UUIDs, or raw IPs into the artifact.

Each raw sample records:

| Field | Unit or vocabulary |
|---|---|
| route | `home` · `activity` · `settings:<tab>` |
| target role | `leader` · `follower` |
| voter count | positive integer |
| shell · content · settled | integer microseconds from click |
| endpoint timings | normalized route template · start/end microseconds · status |
| failure | bounded code, never dynamic response text |
| build | exact server build stamp |

GitHub CI validates the artifact shape and browser state machine. Only the
named LAN runner enforces the wall-clock budgets in §1.1.

**Acceptance:**

```bash
make check
make web-check
```

The deterministic harness tests pass, a synthetic delayed endpoint produces
the expected phase ordering, and an artifact containing a token-like value is
rejected by its redaction test.

### 6.2 PR-B1 turns the stopped voter into a privacy-safe verdict

Implement the read-only inspector from §3.1 before changing purge ordering.
Synthetic fixtures cover absent metadata, stale-non-`None` metadata, retained
gaps, snapshot mismatch, and a clean directory. EV-B then compares the
forensic copy's snapshot id, purge-metadata presence and value, actual retained
ranges, and applied index. Preserve before/after hashes proving the inspector
did not modify any file.

**Acceptance:**

```bash
make cluster-check
make check
```

Every synthetic fixture receives its stable verdict, secret-shaped fixture
content never appears in JSON, and the forensic-copy manifest is byte-for-byte
unchanged after inspection. EV-B records whether `last_purged_log_id` is absent
or stale-non-`None`; it does not publish raw WAL contents.

### 6.3 PR-B2 repairs only a reproduced failure

Build the smallest reproducer that creates EV-B's invariant violation. If the
forensic copy is not available, a synthetic reproducer may establish the same
window, but the production cause remains unconfirmed and the PR stays draft
until that limitation is explicit in its evidence.

Exercise at least these failpoints:

1. after new purge metadata is staged but before rename;
2. after metadata rename but before old WAL removal;
3. during removal of a complete front WAL file;
4. during trimming of a retained front WAL file;
5. after physical removal but before final directory/header sync; and
6. restart after at least 60 reduced-threshold snapshot/purge cycles, because
   the live missing index is near the 52nd production threshold.

The exact failpoint that reproduces the observed shape lands in the regression
name. Preserve the existing pre-removal active-header flush in every path. If
none reproduces the observed shape, do not merge a speculative reorder: keep
PR-B1 and expand the evidence to filesystem errors, storage replacement, and
concurrent snapshot installation.

Candidate test names:

```text
purge_restart_never_exposes_a_missing_required_boundary
multi_rollover_purge_restart_keeps_log_state_contiguous
snapshot_install_and_purge_survive_every_metadata_publish_boundary
follower_rejoins_after_sixty_snapshot_cycles_without_log_index_not_found
```

Update the affected vendored `PLURX-PATCH.md` when the repair changes behavior
or provenance. Update `vendor/hiqlite/PLURX-PATCH.md` as well only if the
Hiqlite crate itself changes. Keep the on-disk format unchanged unless the
reviewer approves an explicit compatibility migration.

**Acceptance:**

```bash
make cluster-check
make check
```

The new failpoint fails on the pre-fix vendor code, EV-B's verdict matches that
pre-fix invariant, and the repaired code passes the test, reopens repeatedly,
catches up to a live leader, and answers a consistent read.

## 7. PR-C and PR-D — make degradation visible without creating new load

### 7.1 PR-C extends the existing replication classifier

Implement §3.3 inside the current `ReplicationMonitor`; do not add a Store read
to the `/metrics` scrape path or a new per-request peer fan-out. Read the merged
`PassiveRaftMetricsView` for validity-gated local and quorum/apply facts. Keep
the existing `metrics_db` sample as the leader-only source of every-peer match
indexes; do not pretend the quorum watermark contains an all-peer count.

Required classifier cases:

- local core not running → degraded · `raft_not_running`;
- no leader → degraded · `leader_unknown`;
- two voters → degraded · `two_voter_reconfiguration`;
- local log ahead of apply → degraded · `local_apply_lag`;
- leader sees lagging peer → cluster scope · degraded · `peer_lag`;
- leader has fewer peer entries than configured → cluster scope · degraded ·
  `peer_unknown`;
- follower caught up locally → local scope · in sync, without a cluster-wide
  claim; and
- metrics read failure → degraded · `metrics_unavailable`, preserving the last
  safe applied/convergence facts.

Update the System and Cluster renderers, their prose in
[OPERATIONS.md](OPERATIONS.md), the exact JSON key test, and
[`cluster-membership.test.js`](../tests/web/cluster-membership.test.js).

**Acceptance:**

```bash
make check
node tests/web/cluster-membership.test.js
make web-check
```

A synthetic stopped core with fresh heartbeat renders a warning, the row says
`Heartbeat: seen recently`, no green `Redundant and in sync` claim appears,
and a fully confirmed leader sample retains the green path.

### 7.2 PR-D backs off only repeated failures

Replace the fixed failure cadence in `offline_source_probe_loop` with:

```text
success or empty pass ──▶ next poll in 500 ms
first failure          ──▶ warn once; retry in 1 s
continued failures     ──▶ 2 s · 4 s · capped 5 s
first recovery         ──▶ one recovery info; reset to 500 ms
```

The five-second cap stays below the existing 15-second removal probe wait, so
a recovered worker can still answer an in-flight operation. Add bounded per-
node jitter only if synchronized voters are measured to collide; do not make
the retry deadline nondeterministic in unit tests.

Log the first failure, then one summary per 60 seconds containing first seen ·
last seen · count · stable error code. Dynamic internal error text remains in
the first diagnostic, not a metric label.

Use the merged P2b Store histogram to correlate authority-read/write failures
with probe backoff. Do not add another Store timer. A probe-specific counter or
gauge requires a separate bounded-label justification and is not required for
this PR's acceptance.

**Acceptance:**

```bash
make check
make cluster-check
```

A paused-time test proves the exact delay sequence, log deduplication, reset on
recovery, and a pending removal receives a source-probe answer within its
15-second contract after Store recovery.

## 8. PR-E and PR-F — bound server work and commit sections independently

### 8.1 PR-E adds the Home primitive with backend parity

Implement §3.4 for both SQLite and Hiqlite. Test these invariants through the
backend-neutral Store contract:

- zero libraries returns an empty list;
- empty libraries remain present with `total = 0`;
- each nonempty library returns at most 24 top-level items;
- ordering and totals match `list_top_items(..., Added, 0, 24)`;
- home-video folders/photos and book/audiobook roots preserve current rules;
- watch, rollup, resolution, and child-count annotations match the existing
  per-library endpoint; and
- attempted Hiqlite call count stays fixed for one, ten, and fifty libraries.

The HTTP test proves one `AuthUser` extraction, a clamped limit, admin-neutral
authenticated access, and unchanged existing endpoints. No native client must
adopt the new route.

**Acceptance:**

```bash
make check
make cluster-check
make web-check
```

The Store-call gate is cardinality-independent, response parity holds on both
backends, and the Apple/Android API surface has no removed or changed field.

### 8.2 PR-F changes the page state machine, not just the markup

Implement §3.5 and §3.6 in the embedded web app. Keep the existing generation
counter as the single cancellation authority.

**Home commit order:** shell now · `/hubs` first content · batched previews ·
Coming Soon independently · settled when all nonfailed current-generation
requests finish.

**Activity commit order:** prior snapshot or shell now · current detail first
content · poll refresh in place · stale indicator on non-401 error · settled
after the current request.

**Settings commit order:** tab bar now · active-tab loader · active panel ·
secondary data inside that panel. A tab switch starts only the newly selected
tab's missing dependencies.

Extend [`page-read-budget.test.js`](../tests/web/page-read-budget.test.js) with
deferred promises that prove behavior, not elapsed milliseconds:

1. a three-second Coming Soon promise cannot delay Home hubs;
2. a delayed Home preview cannot erase or delay rendered hubs;
3. Home makes exactly three top-level requests for one or fifty libraries;
4. a stale Home response cannot commit any phase after navigation;
5. Settings Libraries starts only three required endpoints;
6. Settings Playback starts only `/settings`;
7. Settings System paints `/system` before playback events resolve;
8. Settings Cluster does not fetch `/system`, users, Trakt, or playback events;
9. a tab switched while loading cannot paint the old tab;
10. Activity retains the last body during a slow or failed poll; and
11. 401 clears the protected route through the existing auth path.

Update the structural UI golden deliberately and review the diff. The expected
change is panel-local loading/stale state and phase attributes, not a layout
redesign.

**Acceptance:**

```bash
make check
make web-check
make ui-check
```

All deterministic request/phase tests pass. No whole-route `Loading…` sentinel
remains after a route shell or prior snapshot exists.

## 9. Validation matrix — every claim has one owner

| Claim | Deterministic evidence | Physical or operational evidence |
|---|---|---|
| WAL purge survives crashes | Vendor failpoint tests · multi-cycle follower restart scenario. | Preserved `nuc4` inspection matches or contradicts the reproduced invariant. |
| Three retained voters are healthy | Cluster convergence and restart tests. | Ten-minute mixed load with zero established failure signatures. |
| Health wording is truthful | Rust classifier table tests · shipped JS text tests. | Stop/fault one follower; panel warns while heartbeat may remain recent. |
| Probe loop does not storm | Paused-time cadence and log-summary tests. | Ten-minute injected Store failure has bounded warnings and recovers promptly. |
| Home work is bounded | Store operation counters at 1/10/50 libraries · exactly three browser requests. | Named runner records endpoint and phase distributions. |
| Settings work follows tab | Deferred-promise request matrix tests. | Network trace contains no inactive-tab endpoints before first panel. |
| Activity remains stable | Busy/generation/stale-snapshot tests. | Slow response retains prior content and poll count remains one. |
| Authorization remains immediate | Existing token/key revocation matrix plus new endpoint tests. | Delete a disposable test token and require the next request to return 401. |
| Native apps are unchanged | No `clients/apple` or `clients/android` implementation diff; existing API tests. | Final candidate passes `make apple-test` and `make android-test`. |

### 9.1 Named-runner procedure separates healthy and faulted samples

For each leader and one follower:

1. warm the authenticated session without navigating to a measured route;
2. collect 30 Home · Activity · Settings Libraries · Settings System samples;
3. record raw phase and endpoint timings;
4. repeat with one non-leader stopped while the remaining three-voter quorum is
   healthy; and
5. repeat one explicit slow-optional-dependency injection for Home and System.

Do not mix healthy, voter-unavailable, and deliberately delayed-integration
samples into one percentile. Each answers a different product question.

**Acceptance:** every healthy surface meets §1.1, the stopped-voter case meets
the degraded budget without blanking, and the delayed optional dependency
moves settled time but not first-content time.

### 9.2 Full release gate

Run the repository-wide checks after all PRs are integrated:

```bash
make check
make web-check
make ui-check
make cluster-check
make apple-test
make android-test
```

Hardware-only or Docker-only failures remain reported as such; they are not
silently skipped or converted into unit-test claims.

## 10. Rollout and rollback — protect quorum while changing local code

### 10.1 Merge and deploy order follows the rollback table

Merge PR-A first so every behavior PR has outcome evidence. Merge PR-B1 before
EV-B, and merge PR-B2 only after the inspector verdict and reproducer agree.
Do not allow another damaged voter to cycle through production snapshots while
that repair remains open. PR-C and PR-D may follow once their classifier and
cadence tests are independent. Merge PR-E before PR-F so old web code ignores
the new endpoint and the new web code never deploys without its server route.

None of these PRs changes the replicated schema. Roll one non-leader voter at a
time, require `/readyz`, local applied-index catch-up, and an attributable build
before moving to the next. On a three-voter cluster, never have two voters down
at once.

### 10.2 Canary the web behavior on one follower

Use one follower as the first HTTP target while all three voters keep the same
code-compatible Raft membership. Verify:

- old native clients still browse and play;
- Home emits three top-level requests;
- Settings tab request sets match §3.5;
- Activity owns one poll;
- no new 401, 5xx, Store timeout, or render-generation error appears; and
- the page artifact meets the healthy budget.

Then complete the rolling deploy and measure the leader plus another follower.

### 10.3 Roll back code without undoing membership

- PR-F web failure: deploy the preceding binary; the unused additive Home
  endpoint is harmless.
- PR-E endpoint failure before PR-F: revert it directly because no client uses
  it.
- PR-C status failure: old clients ignore additive fields; revert the
  classifier/UI together so prose and JSON do not disagree.
- PR-D cadence failure: restore the 500 ms loop while preserving the first
  diagnostic evidence.
- PR-B1 inspector failure: remove the diagnostic binary; no WAL was changed.
- PR-B2 WAL failure: stop the rollout. Because the format is intended to
  remain unchanged, the preceding binary can reopen a healthy voter, but the
  reviewer must confirm this with the restart test before deployment.
- OP-0 removal: never restart the tombstoned directory. A capacity rollback is
  a fresh join under the supported runbook, with a new token and identity.

## 11. Review decisions — Fable's findings are incorporated

[CLUSTER_PAGE_LATENCY_FIX_PLAN_REVIEW.md](CLUSTER_PAGE_LATENCY_FIX_PLAN_REVIEW.md)
is the completed adversarial review. These decisions now control execution:

1. **Live recovery:** code review approves the supported 4→3 follower path,
   subject to §5.1's live quorum, valid-watermark, Store-path, offline-work, and
   explicit operator gates. Source review cannot supply those live facts.
2. **WAL candidate:** metadata-first ordering is endorsed only after PR-B1 and
   EV-B identify the matching stale-boundary shape. The existing active-header
   flush remains mandatory.
3. **Forensic scope:** §3.1 is sufficient with the added
   `last_purged_log_id_present` fact distinguishing absent from stale-non-`None`
   metadata.
4. **Health contract:** the additive fields are accepted. P2c/P2d supply
   validity-gated local and quorum facts; the existing leader replication map,
   not the quorum watermark, supplies every-peer confirmation.
5. **Home endpoint:** the windowed primitive matches both current backends. The
   public page limit is fixed at 24 rather than caller-selectable.
6. **Settings dependencies:** the read manifest is accepted. Every mutation is
   re-verified when PR-F lands because a future mutation-side dependency must
   not be hidden by a stale manifest.
7. **Latency budgets:** the percentile shape is accepted, while the numeric
   500 ms healthy and 1,000 ms degraded targets remain provisional until PR-A
   produces named-runner variance. Implementation may proceed; release cannot
   claim those budgets before that evidence exists.
8. **PR boundaries:** PR-A, PR-C, and PR-D are reconciled with the sibling
   performance plan in §2.2. The WAL inspector and WAL mutation are split into
   PR-B1 and PR-B2 because their rollback boundaries differ.
9. **Mobile boundary:** accepted. The additive endpoint requires native
   regression gates but no Apple or Android feature change.

Any later reviewer change is recorded as an edit to this section with its
source PR. Production and availability-affecting actions still require the
explicit approvals in the adversarial review's §8 table.

## 12. Source map — re-verify before each milestone

| Boundary | Source |
|---|---|
| Incident evidence and causal chain | [CLUSTER_PAGE_LATENCY_REVIEW.md](CLUSTER_PAGE_LATENCY_REVIEW.md) |
| Completed adversarial review and approval table | [CLUSTER_PAGE_LATENCY_FIX_PLAN_REVIEW.md](CLUSTER_PAGE_LATENCY_FIX_PLAN_REVIEW.md) |
| Supported readiness and removal runbook | [OPERATIONS.md](OPERATIONS.md), readiness and voter-removal sections |
| Existing cluster measurement contract | [CLUSTER-PERFORMANCE-PLAN.md](CLUSTER-PERFORMANCE-PLAN.md) |
| Hiqlite Store timeout and operation wrappers | [`crates/plurx-core/src/store/hiqlite.rs`](../crates/plurx-core/src/store/hiqlite.rs), `TimedClient` |
| Store browse interface | [`crates/plurx-core/src/store/mod.rs`](../crates/plurx-core/src/store/mod.rs), `CatalogStore` |
| SQLite browse semantics | [`crates/plurx-core/src/store/sqlite/media.rs`](../crates/plurx-core/src/store/sqlite/media.rs), `list_top_items_in_genre` |
| Hiqlite browse semantics | [`crates/plurx-core/src/store/hiqlite_media.rs`](../crates/plurx-core/src/store/hiqlite_media.rs), `list_top_items_in_genre` |
| Home hubs handler | [`crates/plurxd/src/http/browse.rs`](../crates/plurxd/src/http/browse.rs), `hubs` |
| Native API route table | [`crates/plurxd/src/http/mod.rs`](../crates/plurxd/src/http/mod.rs), `router` |
| Authorization extractor | [`crates/plurxd/src/http/extract.rs`](../crates/plurxd/src/http/extract.rs), `AuthUser` |
| Replication classifier | [`crates/plurx-core/src/cluster/migration.rs`](../crates/plurx-core/src/cluster/migration.rs), `status` module |
| Membership roster and removal | [`crates/plurx-core/src/cluster/membership.rs`](../crates/plurx-core/src/cluster/membership.rs), `status` and `remove_voter` |
| Membership HTTP endpoints | [`crates/plurxd/src/http/cluster.rs`](../crates/plurxd/src/http/cluster.rs) |
| WAL metadata publication | [`vendor/hiqlite-wal/src/metadata.rs`](../vendor/hiqlite-wal/src/metadata.rs) |
| WAL purge implementation | [`vendor/hiqlite-wal/src/writer.rs`](../vendor/hiqlite-wal/src/writer.rs), `Action::Remove` |
| Existing WAL patch contract | [`vendor/hiqlite-wal/PLURX-PATCH.md`](../vendor/hiqlite-wal/PLURX-PATCH.md) |
| Embedded Home/Activity/Settings state machines | [`crates/plurxd/src/web/index.html`](../crates/plurxd/src/web/index.html), `viewHome` · `viewActivity` · `viewSettings` |
| Current request/race gate | [`tests/web/page-read-budget.test.js`](../tests/web/page-read-budget.test.js) |
| Current Cluster wording gate | [`tests/web/cluster-membership.test.js`](../tests/web/cluster-membership.test.js) |
| Validation catalog | [`validation/points.toml`](../validation/points.toml), `cluster.page-reads` and cluster status points |
