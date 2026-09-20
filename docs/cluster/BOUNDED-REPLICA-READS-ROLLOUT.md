# Bounded replica reads rollout — a consistency-policy change, one route at a time

**Status:** ready for review · **Executes:** S1, F-sc-1, F-core-4 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

Read [CLUSTER-PERFORMANCE-PLAN.md](CLUSTER-PERFORMANCE-PLAN.md) §3.2–§3.4
(the four consistency classes and the `BoundedReplica` proof) and §6.4 (what
P3 built) before this. Then §2 here for what the tree does today, and §3 for
the order: measure, widen coverage, fence write-followed-by-read, and only
then flip the default. If a step seems to require caching an ordinary
authentication result, serving a watch-state read locally without a fence,
or turning `bounded_replica_reads` on before a route's fallback has a test,
stop and flag it.

**Correction to the review:** none on the facts. Two clarifications the
plan depends on. First, `search_items` is already a local read on hiqlite
(`query_map`, [hiqlite_media.rs:2160-2166](../../crates/plurx-core/src/store/hiqlite_media.rs));
the handler-inventory test deliberately keeps it on `state.store` rather
than `CatalogueReader` ([http/mod.rs:1586-1589](../../crates/plurxd/src/http/mod.rs)),
so search is not in the count of leader round trips. Second, the 225
`query_consistent` sites are per implementation file, not per request; the
per-request number is what §3.5 measures and the census in §3.4 only keeps
the file count from growing silently.

## 1. Objective

On the hiqlite backend, a Home, library or item request costs one leader
round trip — the authentication read — plus whatever the route genuinely
needs to be linearizable, with every other catalogue read served from the
local replica under the existing lag/term/watermark proof and falling back to
Authority whenever that proof cannot be shown; and `bounded_replica_reads`
defaults to `true` only after each eligible route has that coverage proven
by a test and the per-route measurement shows the saving.

## 2. Contract today

Re-verify at build time.

### 2.1 The switch and the reader

```rust
// crates/plurx-core/src/config.rs:139-145, 170-171
/// Opt-in/kill switch for lag-gated local catalogue reads. Keep identical
/// on every voter during rollout and rollback.
pub bounded_replica_reads: bool,                     // default false
pub bounded_replica_max_lag_entries: u64,            // DEFAULT_BOUNDED_REPLICA_MAX_LAG_ENTRIES = 64 (:23)
// env: PLURX_CLUSTER_BOUNDED_REPLICA_READS (:337), PLURX_CLUSTER_BOUNDED_REPLICA_MAX_LAG_ENTRIES (:343)
```

`CatalogueReader` ([store/mod.rs:4949-5066](../../crates/plurx-core/src/store/mod.rs))
is "the only application-facing boundary for catalogue consistency
choices". With `bounded: None` (SQLite, recovery boots, tests) or
`enabled == false` every method is the Authority call. With it enabled,
`bounded()` runs the local closure under
`PassiveRaftMetrics::run_bounded_replica`
([migration.rs:6468-6485](../../crates/plurx-core/src/cluster/migration.rs)):
a permit is granted only from a valid quorum watermark (term, leader,
committed index, one-second monotonic lease) with `applied ≥ committed −
max_lag`, the local read runs, and the result is kept only if
`permit.remains_valid()` afterwards — otherwise it is discarded and the
Authority path runs. A local SQL error is treated the same way.

Methods on the reader today: `get_library`, `list_libraries`, `get_item`,
`get_item_children`, `list_top_items_in_genre`, `home_preview_pages`,
`recently_added`, `get_file`, `files_for_item`, `child_counts`,
`item_max_heights`, `item_media_facts`, `media_shape`,
`get_file_probe_json` (`:5067-5300`).

### 2.2 Which handlers are on it, and which reads are not

`bounded_catalogue_handler_inventory_keeps_reads_and_mutations_separate`
([http/mod.rs:1542-1670](../../crates/plurxd/src/http/mod.rs)) pins, per
handler, which methods go through `state.catalogue` and which must stay on
`state.store`. Remaining Authority reads on the browse path:

| Handler | `state.store.` read | Class it belongs to |
|---|---|---|
| `browse::list_items` `:128` | `watch_map` | ReadYourWrite (per-user watch state) |
| `browse::list_items` `:261` | `watch_rollups` | ReadYourWrite |
| `browse::home_previews` `:578,581` | `watch_map`, `watch_rollups` | ReadYourWrite |
| `browse::hubs` `:631-632` | `continue_watching`, `next_up` | ReadYourWrite |
| `browse::item_detail` `:432-435` | `fragment_index`, `fragment_index_outcome` | node-local cache state (C14) |
| `items.rs:162,165,212,218` | `get_item`, `files_for_item` | Authority by design — mutation handlers |
| `watch.rs:33,107,130` | `get_item` before `set_watched_tree` | Authority by design |

Every one of the watch reads is `query_consistent_map` on hiqlite
([hiqlite_media.rs:3579-3910](../../crates/plurx-core/src/store/hiqlite_media.rs)).

### 2.3 Authentication

```rust
// crates/plurx-core/src/store/hiqlite.rs:3980-3997
async fn user_for_token(&self, token_hash: &str) -> Result<Option<User>, StoreError> {
    let sql = "SELECT u.id, u.username, u.password_hash, u.is_admin, u.created_at, t.last_seen_at \
             FROM users u JOIN tokens t ON t.user_id = u.id WHERE t.token_hash = $1";
    let mut rows = self.client().query_consistent_map::<TokenUserRow, _>(sql, params!(token_hash)).await?;
    ...refresh_token_activity_if_due(...)   // suppressed inside the 60 s window (:2963-2999)
}
```

One consistent read per authenticated request
([extract.rs:619-641](../../crates/plurxd/src/http/extract.rs)), and the
vendored client routes it through the WebSocket stream even on the leader
([client/query.rs:11-33, 286](../../vendor/hiqlite/src/client/query.rs)).
That is the floor this plan does not lower, for the reason in §4.

### 2.4 Why the admin proof is not a template

`CacheOnlyAdminUser` ([extract.rs:24-57](../../crates/plurxd/src/http/extract.rs))
exists for two recovery reads only. Its proof holds `user_id` and a fixed
five-minute expiry (`CACHE_ONLY_ADMIN_PROOF_TTL`, `:56`), is never renewed
by a cache hit, and is safe **only because of the revocation fence around
it**: every token deletion brackets the Store mutation with two cache
generations (`CacheOnlyAdminRevocation`, `:124-131`), an ambiguous write
closes the whole cache for one proof lifetime (`local_ambiguity_expires_at`,
`:75-79`), and replicated nodes start closed until a committed roster proves
every member runs the peer revocation protocol
(`cluster_revocation_capability_ready`, `:80-83`,
`CACHE_ADMIN_REVOCATION_CAPABILITY = "cache_admin_revocation_v4"` in
[membership.rs:131](../../crates/plurx-core/src/cluster/membership.rs)).
Copying the proof onto `AuthUser` would carry a stale `is_admin`, a stale
username, and — without that fence on every route — a revoked credential
honoured for up to five minutes on a peer. The assessment (correction 1,
F-core-4) and CLUSTER-PERFORMANCE-PLAN.md §3.3 both refuse it; so does this
plan.

### 2.5 What is measured today

`plurx_store_operation_seconds{class="local_read"|"authority_read"|"write",outcome}`
and `plurx_store_operations_total` per process
([hiqlite.rs:554-578](../../crates/plurx-core/src/store/hiqlite.rs),
[system.rs:690-730](../../crates/plurxd/src/http/system.rs)). Nothing
attributes a Store class to an HTTP route or a node role.

## 3. Change

### 3.1 M0 — per-route, per-role attribution

A request-scoped counter set on `AppState` incremented by the store wrapper
(`TimedClient`) and flushed by the HTTP layer into
`plurx_http_store_reads_total{route_group,class,role}`:

- `route_group` ∈ {`auth`, `home`, `library`, `item`, `search`, `playback`,
  `settings`, `cluster`, `other`} — mapped from axum's `MatchedPath` through
  a fixed table (bounded; unknown → `other`);
- `class` ∈ {`local_read`, `authority_read`, `write`};
- `role` ∈ {`standalone`, `voter`, `learner`, `remote_authority`} from the
  process's selected backend and committed role.

Plus `plurx_http_route_seconds{route_group,role}` (histogram, same buckets
as `plurx_store_operation_seconds`) so store time and page time are read
side by side — the assessment's "store-call duration is not page latency".

### 3.2 M1 — coverage: the remaining catalogue reads

Add to `CatalogueReader` the reads that are catalogue facts, not per-user
state. `item_detail`'s fragment-index summary is C14's item and is not
moved here. What is left on `state.store` that is a pure catalogue read:
`plex.rs:350,382,395` (`get_item(..).is_some()` existence checks before
Plex timeline/scrobble handlers — these precede a mutation and stay
Authority; the inventory test already pins two of them) and
`libraries.rs:174,188,202` (`get_library` existence checks before
`update_library`/`set_library_schedule`/`delete_library` — mutation
pre-reads, stay Authority). So M1 is small: the inventory test grows to
cover `photos.rs`, `images.rs`, `reading.rs`, `dvr.rs` and
`library_channels.rs` handlers explicitly (today it covers browse, libraries,
plex and system only, `mod.rs:1544-1670`), and `search_items` joins the
reader as an explicit `NodeLocal` method (it already is local; naming the
class at the call site is CLUSTER-PERFORMANCE-PLAN.md §3.2's rule).

### 3.3 M2 — a fence for watch state (ReadYourWrite)

The Home rails read what the same user just wrote. A follower one entry
behind would show an episode as unwatched a second after the tick. The fence:

- every watch mutation on hiqlite already returns from a committed write;
  capture the commit index from the client's write response and keep, per
  process, `last_write_index[user_id]` (bounded map, TTL 60 s);
- `CatalogueReader::watch_*` local variants require `applied_index ≥
  last_write_index[user]` **in addition to** the bounded permit; otherwise
  Authority;
- a write on node A followed by a read on node B is not fenced by this map.
  Sticky sessions make it rare, not impossible. So the write response also
  carries `X-Plurx-Commit-Index`, the web client echoes the last value it saw
  as `X-Plurx-Read-After` on the next requests for 60 s, and the reader
  honours the larger of the two. A client that does not send the header
  (Apple, Android until updated) gets Authority for watch reads on
  non-writing nodes — correct, merely slower.

Only after M2 do `watch_map`, `watch_rollups`, `continue_watching`, `next_up`
move behind the reader. `set_watched_tree`'s pre-read `get_item` stays
Authority: it precedes a mutation.

### 3.4 M3 — fewer consistent reads per request, and a growth census

- `home_previews` and `list_items` issue `watch_map` and `watch_rollups`
  as two consistent statements for the same id set. One
  `watch_summary(user, ids)` returning both halves is one round trip; on
  SQLite it is one closure on one connection.
- `hubs` issues `continue_watching` and `next_up` separately for the same
  user; the second's four `NOT IN`/`IN` subqueries over `watch_state` and the
  first's scan of the same table can share one statement with two CTEs.
- Settings reads inside handlers (`get_setting` per request for playback
  language preferences and similar) are consistent reads that gate nothing
  latency-sensitive; where a handler reads two, use `get_setting_pair`.

Census: `crates/plurx-core/src/store/consistent_read_census.rs` counts
`query_consistent` per `hiqlite*.rs` against a checked-in table (225 today,
per file), fails when a file's count rises unless the same PR raises the
table entry, and refuses a total above the current one without a
`// authority: <reason>` comment on the new site. The doc comment states
plainly that this is a source census, not a latency or request-rate
measurement; §3.1 is that.

### 3.5 M4 — flip the default, per route, with proof

Preconditions per `route_group` before its reads are allowed to go local by
default: (a) every catalogue read on the route is on `CatalogueReader`
(inventory test); (b) a paused-follower test proves fallback to Authority
and a partitioned-follower test proves the one-second watermark expiry
(CLUSTER-PERFORMANCE-PLAN.md §6.4 acceptance) — these exist in
`cluster-read-cost-validation` for the current methods and are extended to
each added method; (c) M0 shows the route's `authority_read` count per
request on a voter and the expected count after; (d) readiness already drops
a node that lost its serving proof (`serving_fence.rs`); the lag budget
`bounded_replica_max_lag_entries = 64` is kept.

Then `bounded_replica_reads` default becomes `true` in `config.rs:170`, with
the env/TOML key retained as the kill switch and OPERATIONS.md's rollout
note ("identical on every voter during rollout and rollback") kept. The
setting is node-local config by necessity (it is read before the store
exists); the Developer readiness page lists, advisory only, whether every
committed voter reports the same value in its heartbeat capability set.

## 4. Guardrails (non-goals)

- **No ordinary-auth cache.** Not the admin proof, not a variant of it. A
  future cache needs the full user/role state and the same revocation
  protocol on every route (extract.rs, CLUSTER-PERFORMANCE-PLAN.md §3.3); the
  safer step here is fewer consistent reads per request. The floor stays at
  one authority read per authenticated request.
- **Authority stays Authority:** token and key validation, settings that
  gate work, leases, membership, admission, offline ownership, migration
  guards, and every pre-mutation read.
- **Watch state is never served locally without the M2 fence.**
- **No wall-clock freshness.** The bounded permit's lease is monotonic; M2's
  fence is an index comparison. Neither reads `SystemTime`.
- **The default does not flip before M0–M3.** Flipping it is the
  consistency-policy change the assessment names; it is the last PR.
- **The census is not evidence of latency**, and the plan never presents it
  as such.
- **No change to `bounded_replica_max_lag_entries`** without a lab
  measurement that shows 64 is the limit.

## 5. Milestones

### 5.1 M0 — attribution metrics

Acceptance: `cargo test -p plurxd http::metrics_route_attribution` renders
the two families with the fixed label sets; a GET to `/api/v1/home` on a
lab voter shows `authority_read ≥ 3` (auth + two watch reads) and
`local_read = 0` with the switch off, and the readout below is attached to
the M1 PR.

```text
GPT prompt (fleet): With <sha> on lab1–lab3 and one voter behind a browser
session, load Home, one library grid, one item page and one search ten
times each, then give me plurx_http_store_reads_total by route_group and
class for the node you hit, and plurx_http_route_seconds p50/p99 per
route_group. Repeat with PLURX_CLUSTER_BOUNDED_REPLICA_READS=true on all
three voters (restart each), and once more with lab3's apply path paused
(SIGSTOP the process for 20 s while loading) to show the fallback count.
```

### 5.2 M1 — coverage

Acceptance: the handler-inventory test lists every browse/plex/reader
handler's catalogue reads; `make cluster-store-check` green.

### 5.3 M2 — watch fence

Acceptance: `cargo test -p plurx-core --features cluster-read-cost-validation
--test store_contract watch_fence_` : a write on node A then a read on A
with `applied < commit` falls back to Authority; a read on B without the
header falls back; with `X-Plurx-Read-After` set to A's commit index and B
applied past it, B serves locally; a new Node test under `tests/web/`
(`node tests/web/read-after.test.js`) proves the client echoes the header
for 60 s and drops it afterwards.

### 5.4 M3 — fewer reads and the census

Acceptance: `home_previews` performs exactly 1 authority read for watch
state (counter assertion in `cargo test -p plurxd http::browse::home_`);
`cargo test -p plurx-core consistent_read_census` fails on a synthetic
extra `query_consistent` in a fixture copy of `hiqlite_reading.rs`.

### 5.5 M4 — default flip

Acceptance: `config.rs` test asserts the new default; `make cluster-check`
green; the M0 readout repeated on the lab shows `home`'s authority reads per
request ≤ 2 and `library`/`item` ≤ 1 with the switch at its new default;
OPERATIONS.md and CLUSTER-PERFORMANCE-PLAN.md §6.4 updated in the same PR.

## 6. Verification and rollout

Fast lane per PR: `make unit`; M2/M3/M4 also `make cluster-store-check`
(the P3 contracts live there). Node gate for M2's client change:
`node tests/web/read-after.test.js`. Rollout: M0–M3 are behaviour-neutral
with the switch off. M4 rolls voter by voter with the env kill switch set
`false` on each node first, then removed; rollback is setting it back, and
the mixed state is safe because the reader falls back per request.

## 7. Open questions

1. Whether Apple and Android should adopt the `X-Plurx-Read-After` echo in
   the same quarter; until then their Home rails are Authority on
   non-writing nodes, which is today's behaviour.
2. The 60 s TTL on the per-user write index and the header window — a
   proposal; a longer window only costs Authority reads.
3. Should `route_group` include `reader` (ebooks) as its own value? Nine
   groups are proposed; the table is the place to add one.
