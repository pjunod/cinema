# Bounded replica reads rollout — a consistency-policy change, one route at a time

**Status:** in progress — M0-M1 merged (#428); M2-M3 server side in draft PR #504, review pending; the web `X-Plurx-Read-After` echo and M4 not started · **Executes:** S1, F-sc-1, F-core-4 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

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

### 2.6 Re-verified for M2-M4 (2026-09-24, `main` @ `0e2c3fd47`)

What the M2-M4 continuation found when it re-read the tree:

- **The M2 blocker was real, and in-repo.** `Client::execute` returned rows
  affected only ([vendor/hiqlite/src/client/execute.rs:21](../../vendor/hiqlite/src/client/execute.rs)
  at `0e2c3fd47`), and a follower's write crosses the API stream as
  `ApiStreamResponsePayload::Execute(Result<usize, Error>)`
  ([network/api.rs:508-510](../../vendor/hiqlite/src/network/api.rs)). The
  leader already had the index in hand — OpenRaft's `ClientWriteResponse`
  carries `log_id` — and simply dropped it. The vendor is Plurx's own fork
  (19 ledgered patches), so the fix is a twentieth patch, not a new client
  library: see §3.3 *As built*.
- **Watch reads on the browse path are wider than §2.2's table.** Besides
  `list_items` `:128,261`, `home_previews` `:578,581` and `hubs` `:631-632`,
  `item_detail` read `watch_map` (via `watch_lookup`, `:472`) and
  `watch_rollup` (`:479`), `hubs` read a third `watch_map` for its
  recently-added rail (`:656`), and `search` read `watch_map` (`:723`). All
  of them were `state.store` Authority reads; all of them now go through
  the reader and its fence.
- **The consistent-read census is 230 production sites, not 225.**
  Counting `query_consistent` in each `hiqlite*.rs` slice with its
  `#[cfg(test)] mod tests` excluded gives 230 at `0e2c3fd47` (231 counting
  one test-module site in `hiqlite.rs`). *Corrected by the review of #504:*
  an earlier revision said M3's consolidation brought this to 226. It did
  not remove a single consistent read from the source; it routed five
  existing watch reads through one `watch_query` dispatch site, which the
  text count could no longer see. The census now counts each call that
  passes `WatchRead::Authority` to that helper as a site (§3.4 *As built*),
  and stands at 241 after merging `main` at `448e803da`.
- **The item page pays three consistent settings reads before any watch
  read.** `item_detail` calls `TranscodeManager::lang_prefs`
  ([browse.rs:389](../../crates/plurxd/src/http/browse.rs)), which reads
  `AUDIO_LANG`, `SUB_LANG` and `SUB_MODE` one `get_setting` at a time
  ([transcode.rs:21070-21080](../../crates/plurxd/src/transcode.rs)). §3.4's
  settings bullet names exactly this class; it is not changed yet (see the
  Execution log).
- **Everything else in §2 still holds**: `bounded_replica_reads` defaults to
  `false` and the lag budget to 64 (`config.rs`), `run_bounded_replica`
  issues and revalidates one permit per local read
  ([migration.rs:8068-8085](../../crates/plurx-core/src/cluster/migration.rs)),
  and authentication is one consistent read per request.

## 3. Change

### 3.1 M0 — per-route, per-role attribution

A request-scoped counter set on `AppState` incremented by the store wrapper
(`TimedClient`) and flushed by the HTTP layer into
`plurx_http_store_reads_total{route_group,class,role}`:

- `route_group` ∈ {`auth`, `home`, `library`, `item`, `search`, `playback`,
  `settings`, `cluster`, `other`} — mapped from axum's `MatchedPath` through
  a fixed table (bounded; unknown → `other`);
- `class` ∈ {`local_read`, `authority_read`, `write`};
- `role` ∈ {`standalone`, `voter`, `learner`, `remote_authority`, `fenced`,
  `unknown`} from the process's selected backend and committed role. A failed
  role lookup and a fail-closed fenced node never contaminate the voter series.

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
  non-writing nodes — correct, merely slower. *(As built after the review of
  #504: on every node, the writing one included — see below.)*

Only after M2 do `watch_map`, `watch_rollups`, `continue_watching`, `next_up`
move behind the reader. `set_watched_tree`'s pre-read `get_item` stays
Authority: it precedes a mutation.

**As built (2026-09-24, PR #504).**

- *Where the index comes from.* Hiqlite patch 21
  ([PLURX-PATCH.md](../../vendor/hiqlite/PLURX-PATCH.md)) adds
  `Client::execute_acked` and `execute_returning_map_acked`, which return a
  `WriteAck { result, log_index: Option<u64> }`. On the leader the index is
  `ClientWriteResponse::log_id.index`. On any other node it crosses the API
  stream, negotiated per connection by an upgrade-request header
  (`x-hiqlite-write-ack: raft-log-index-v1`) so that no request encoding
  changes and neither direction of a rolling upgrade can receive a variant
  it cannot decode: an older leader ignores the header and answers with the
  original variant, which the new client reports as `log_index: None`.
- *The per-process map* is `store/watch_fence.rs`: user → (fence, expiry),
  60 s monotonic TTL, at most 4,096 users. Every hiqlite watch mutation
  (`put_progress_at`, `put_progress_if_current`, `set_watched`,
  `set_watched_tree`, `apply_remote_watch`) records its index; a write whose
  index is unknown, or that failed or timed out (it may still commit), marks
  the user **unprovable** for the window, which means Authority even with a
  header. A full map does not evict a live fence — forgetting one would let
  a read skip that write — it closes local watch reads for everyone for one
  window instead.
- *The rule a local watch read obeys*: the bounded permit **and** a
  client `X-Plurx-Read-After` **and** `applied_index ≥ max(X-Plurx-Read-After,
  local fence)`. Without the header the read goes to Authority on every
  node, including one that holds a write record for the user. *(Changed by
  the review of #504, finding 1. The first build let a node's own record
  stand in for the header, so this sequence served stale state: the TV
  writes progress through node B at index 100, the phone marks the next
  episode watched through node A at 140, and the TV's next Home on B, applied
  to 110, passed a fence of 100 and showed the episode unwatched. A node
  cannot tell whether the user wrote through a peer after its own record,
  any more than it can tell a user who wrote nothing from one who wrote
  through a peer a second ago.)* The local record now only **raises** a
  client's floor, when this node acknowledged a newer write for the user
  than the one the client echoes. The header gives **per-client session
  consistency, not per-user consistency**: a second device's write through
  another node is covered by neither source, and a read that must see it is
  an Authority read. A watch read is therefore local only for a client that
  echoes an index from the last 60 s — see §7 question 4.
- *The permit* gains one check, `run_bounded_replica_after`: it is issued
  only when the sampled applied index already reaches the fence, and the
  existing revalidation keeps the applied index from moving below that.
- *HTTP*: `ReadAfter` accepts exactly one positive decimal
  `X-Plurx-Read-After`; anything else is no fence (Authority). The route
  attribution middleware scopes each request. Background writers — the
  progress coalescer's trailing flush, Trakt sync — have no request, so they
  raise the per-process fence but offer no header.
- *The `X-Plurx-Commit-Index` contract* (the web echo is built against
  this):
  - a response to a request that made **no** watch write carries no
    header, and the client keeps what it holds;
  - a response to a request whose watch writes **all** reported a Raft log
    index carries the highest, as a positive decimal; the client echoes the
    larger of that and what it holds, as `X-Plurx-Read-After`, for 60 s from
    when it saw it;
  - a response to a request with **any** watch write whose index is unknown
    — an older leader or a proxy answered it, or it failed or timed out and
    may still commit — carries `X-Plurx-Commit-Index: unknown`, and the
    client **drops** its echo, so its next watch reads go to Authority.
    *(Review of #504, finding 2: the first build sent no header here, which
    left the client echoing its previous, older index past a write it had
    just been told succeeded.)* A client that echoes `unknown` verbatim
    sends a malformed fence, which is also Authority.
- *Not built here*: the web client's echo (`node tests/web/read-after.test.js`
  in §5.3). It is client code and is left to the session that owns web
  client work; until it lands no client sends the header, so **no node
  serves watch state locally**, even with `bounded_replica_reads=true`: M2's
  local path is exercised by the contracts and is dormant in production
  until a client echoes. Apple and Android are §7 question 1.
- *Not fenced*: `publish_identity_repair`'s watch-row copies
  (`hiqlite_publication.rs`) are a catalogue repair under a job lease, not
  a user's write; the rows move with the items they describe and are
  covered by the catalogue's bounded lag like the items themselves.

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

**As built (2026-09-24, PR #504).** `WatchStore::watch_summary(user,
item_ids, container_ids)` and `WatchStore::progress_rails(user, limit)` are
new required Store methods on both backends. On hiqlite each is one
statement (`UNION ALL` of the two halves with a discriminator column; the
rails keep their own subquery order and limit and the outer order restores
them); on SQLite `watch_summary` runs both statements in one closure on the
one connection and `progress_rails` runs the same shared statement
(`sql_source::progress_rails`, which embeds `next_up`'s `FROM` verbatim).
`list_items`, `item_detail` and `home_previews` read watch state once each;
`hubs` reads its two progress rails once and keeps a separate lookup for the
recently-added rail's cards, which depend on the catalogue rows it
annotates. The settings bullet is **not** done: the only handler-path case
found is `lang_prefs`' three reads (§2.6), and `get_setting_pair` would take
it to two, not one; which shape to use is left to the M0 readout.

Census: `crates/plurx-core/src/store/consistent_read_census.rs` counts
`query_consistent` per `hiqlite*.rs` against a checked-in table, fails when
a file's count rises unless the same PR raises the table entry, and refuses
a total above the ceiling without a `// authority: <reason>` comment on the
new site. It also counts, as a site of its own, every call that passes a
consistent read kind to a shared dispatch helper (`CONSISTENT_DISPATCH`,
today `WatchRead::Authority` into `watch_query`), because the helper's one
`query_consistent_map` would otherwise hide every new Authority watch read
written through it (review of #504, finding 4). The ceiling is 241 at the
merge with `main` `448e803da`: 230 at `0e2c3fd47`, eight sites `main` added
since, M3's two new watch reads and the dispatch site itself. M3 lowers
consistent reads *per request*; it did not lower the number of sites. The
doc comment states plainly that this is a source census, not a latency or
request-rate measurement (a helper call that issues two statements counts
once, like a site in a loop); §3.1 is that.

Watch reads per request are counted by the Store itself:
`HttpStoreOperationCounts::watch_reads` rises once per watch statement on
the replicated store (at `watch_query`, which every watch read goes
through) and once per connection checkout on SQLite. The plurxd tests
`http::browse::tests::home_previews_reads_watch_state_exactly_once` and
`home_hubs_grid_and_item_pages_read_watch_state_once_per_dependency` drive
the real handlers inside a request scope and assert the count, replacing a
source-text check (review of #504, finding 3); the store contract ties the
counter to one consistent statement each for `watch_summary` and
`progress_rails` on three voters.

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
- **Watch state is never served locally without the M2 fence** — the
  client's `X-Plurx-Read-After`, which a node's own write record may raise
  but never replace.
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
4. *(Raised by the M2 build, for Paul; narrowed by the review of #504.)* A
   watch read is local only for a client that echoes an
   `X-Plurx-Read-After` from the last 60 s. A node's own write record no
   longer counts on its own (§3.3 *As built*), so until the web echo lands
   **no watch read is local anywhere**, and afterwards Apple and Android
   (question 1) and an idle web session stay on Authority for watch state.
   That is what makes the fence sound without a replicated per-user
   revision, and M3 already takes that Authority cost from two reads to one.
   The header also gives per-client, not per-user, consistency: a second
   device's write through another node is visible locally only once the
   node's bounded lag has passed it, and a read that must see it is an
   Authority read. If more locality is wanted, the options are a longer
   window (costs nothing in correctness), a client that always sends its
   last seen index even when older than 60 s, or a replicated per-user
   watch revision that a node can check locally (a per-user consistency
   guarantee, and a schema change) — all decisions, not fixes.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | M0 | [#428](http://192.168.4.7:3000/noirr/plurx/pulls/428) · `3976b8c51` | Implemented fixed-cardinality request-local Store attribution and route latency. `cargo test -p plurxd metrics_route_attribution_renders_fixed_labels_and_scoped_store_counts` passed; the required lab1–lab3 readout remains pending. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | M1 | [#428](http://192.168.4.7:3000/noirr/plurx/pulls/428) · `06c87ff92` | Named search explicitly as `NodeLocal` through `CatalogueReader` and expanded the handler inventory across photos, images, reading, DVR, and library channels while retaining Authority pre-mutation/ownership reads. `cargo test -p plurxd bounded_catalogue_handler_inventory_keeps_reads_and_mutations_separate` passed. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | M2 | [#428](http://192.168.4.7:3000/noirr/plurx/pulls/428) · pending | Blocked: M0 fleet evidence is required before widening the rollout. The vendored `Client::execute` also exposes rows affected, not the acknowledged Raft log index; deriving a fence from a later quorum sample would add a second round trip and would not be the write response contract this milestone specifies. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | M3 | [#428](http://192.168.4.7:3000/noirr/plurx/pulls/428) · pending | Blocked behind M2: watch-state reads stay Authority until the revision fence is real. No combined-query claim or authority-read reduction is recorded. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | M4 | [#428](http://192.168.4.7:3000/noirr/plurx/pulls/428) · pending | Blocked on M0–M3 and three-voter paused/partition evidence. `bounded_replica_reads` remains false; the lag budget remains 64; no enablement gate or auth cache was added. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | Review disposition | [#428 comment #3344](http://192.168.4.7:3000/noirr/plurx/pulls/428#issuecomment-3344) | Replaced substring attribution with an exact registered-`MatchedPath` table, exercised every non-`other` family through the production middleware and the `TimedClient` operation-timer path, added explicit `fenced`/`unknown` roles, and made the M1 five-surface inventory an exact per-function call multiset. Focused tests named in the PR disposition passed. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | claim | [#504](http://192.168.4.7:3000/noirr/plurx/pulls/504) · `b497c86ba` | Claimed the server-side remainder (M2-M4) on `plan/K-04-2` from `0e2c3fd47`; re-verified §2 (§2.6). |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M2 (server) | [#504](http://192.168.4.7:3000/noirr/plurx/pulls/504) · `7fcf4853b`, `afc92eafe` | Unblocked in-repo: Hiqlite patch 20 reports the committed Raft log index (§3.3 *As built*). Per-process fence, fenced reader methods, `ReadAfter`, `X-Plurx-Commit-Index`. Evidence: `watch_fence_serves_watch_state_locally_only_behind_the_acknowledged_write` (three voters, streamed writes) passed; `store::watch_fence::tests` (5), `bounded_replica_after_refuses_before_reading_until_the_fence_is_applied`, vendor `write_ack_*` (2), plurxd `read_after_accepts_exactly_one_positive_decimal_index` and `commit_index_header_offers_only_a_fully_acknowledged_watch_write_position` passed. Reverting each production hunk fails its test: the permit's applied-index check (the core unit test and the contract), the leader's negotiation (the contract gets no index), the no-record-no-header rule (three fence unit tests and the contract), the response header (the plurxd test). **Not built:** the web echo and its `tests/web/read-after.test.js` (client code). |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M3 | [#504](http://192.168.4.7:3000/noirr/plurx/pulls/504) · `afc92eafe` | `watch_summary` and `progress_rails` on both backends; browse pages read watch state once per dependency; source census at 226 sites *(restated as 241 by the review disposition below: 226 was the refactor hiding five reads)*. Evidence: `watch_summary_and_progress_rails_match_the_separate_reads_on_every_backend` (SQLite and three voters), `watch_summary_and_progress_rails_are_one_read_matching_the_separate_reads` (one consistent read each, local path parity), `consistent_read_census_*` (3), plurxd `home_previews_reads_watch_state_exactly_once` passed *(then a source-text check; a Store counter since the review disposition below)*. **Not done:** the settings bullet (§3.4 *As built*). |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M4 | not started | Not buildable in this PR by the plan's own guardrail: the default flip is the last PR, after M0-M3 are merged (§4). Its precondition (c) also needs the §5.1 readout on lab voters with M2-M3 deployed; (b) needs `plurx-cluster-check`'s apply-pause/partition case extended to the watch methods, which is buildable and belongs to that PR. `bounded_replica_reads` stays `false`, the lag budget stays 64, no auth cache was added. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | needs: fleet | post-merge | After #504 deploys to lab1-lab3, run the §5.1 GPT prompt with one addition: *"Also, with a browser session on a follower, mark an episode watched and give me the response's `X-Plurx-Commit-Index` header (present, a positive integer), then load Home on that same follower within 60 s and on another voter, and report `plurx_http_store_reads_total{route_group="home"}` deltas for each load with `PLURX_CLUSTER_BOUNDED_REPLICA_READS=true`. Then deploy it to one voter only (rolling upgrade) and confirm a watch write through that voter still succeeds while the leader runs the previous build."* |
| 2026-09-25 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | Review disposition | [#504 comment 4631](http://192.168.4.7:3000/noirr/plurx/pulls/504#issuecomment-4631) · `d17cf2381`, `b11dc6b69`, `d5fd2ff81`, `ac43956c1` | Merged `main` at `448e803da` first (`2b75ce66c`; two conflicts, both additive: the store contract's tail and this plan's work-board row). (1) **P1, fixed:** a node's own older write record no longer proves freshness; without `X-Plurx-Read-After` every watch read is Authority, on every node (§3.3 *As built*, §7 question 4). Pinned by `watch_fence_a_peers_older_record_does_not_prove_a_write_through_another_node` (three voters: write via B, write via A, read on B) and `store::watch_fence::tests::a_local_write_record_never_stands_in_for_the_clients_floor`. (2) **P2, fixed:** `X-Plurx-Commit-Index: unknown` on a request with an unindexed watch write, and the client drops its echo; pinned by `http::tests::commit_index_header_offers_the_acknowledged_position_or_unknown`. (3) **P2, fixed:** a Store counter of watch reads per request (`HttpStoreOperationCounts::watch_reads`) replaces the source-text check; `http::browse::tests::home_previews_reads_watch_state_exactly_once` and `home_hubs_grid_and_item_pages_read_watch_state_once_per_dependency` drive the handlers. (4) **P2, fixed:** the census counts `WatchRead::Authority` call sites of `watch_query` and is restated at 241 (§2.6, §3.4); `consistent_read_census_refuses_a_new_authority_read_through_a_dispatch_helper`. Each test was run with its production hunk reverted on nuc3 and failed (exit 101); the counter test also fails (2 reads) when an extra `state.store.watch_state` call is added to `home_previews`. Consequence recorded for Paul: until the web echo lands, no watch read is served locally (§7 question 4). |
| 2026-09-25 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | Promotion | [#504](http://192.168.4.7:3000/noirr/plurx/pulls/504) | On merging `origin/main` (`37baf6e0b`, P-03 phase B in force past `448e803da`) the branch's two `validation/regressions.d` rows (`b11dc6b6`, `d17cf238`) were refused by the freeze, since both commits are past the boundary. They are removed; the pull request body's five `Regression-Test:` lines name the review-fix tests and are carried into the landing commit's message. |
| 2026-09-25 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | Promotion | [#504](http://192.168.4.7:3000/noirr/plurx/pulls/504) | K-08 (#503) landed first and took Hiqlite ledger row 20 (`ring` as the only rustls provider), so on merging `origin/main` `196d2a43e` the `WriteAck` patch this plan adds is **patch 21**: `vendor/hiqlite/PLURX-PATCH.md` now carries twenty-one patches, thirteen of them Plurx policies (rows 20 and 21 in the removal paragraph), and `tests/operations/test_hiqlite_patch_ledger.py` expects 21 rows. The 2026-09-24 rows above that say patch 20 predate the renumbering. |

Implementation decisions recorded for this run: retain the proposed nine route
groups and classify every registered route pattern explicitly (`other` is only
the unknown fallback), retain the proposed 60-second fence/header lifetime when
M2 becomes executable, and refuse both an ordinary-auth cache and a fabricated
write revision. Route attribution uses a Tokio request scope rather than a field
copied through every `AppState` constructor; the scope is installed once around
the matched request, and `TimedClient` remains the only production recorder of
physical replicated Store calls.
