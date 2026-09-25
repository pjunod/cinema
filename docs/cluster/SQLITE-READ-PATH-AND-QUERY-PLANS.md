# SQLite read path and query plans — readers off the writer, indexes from plans, and the search predicate that keeps renamed titles

**Status:** M0-M4, M6, M7 and one found fix in draft [PR #502](http://192.168.4.7:3000/noirr/plurx/pulls/502) (M4 partial, M5 not built; see the execution log) · **Executes:** S6, S7, S11, F-sc-6, F-sc-7,
F-sc-8 (as corrected in §0 of the review), F-sc-14 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Read §2 first: it carries the actual SQL and the connection helpers as they
stand, because every decision below is "run this statement through `EXPLAIN
QUERY PLAN` on a representative database and act on what it says", and a
plan is only evidence for the statement it was taken on. Then §3.1's
migration rule before touching any `with_conn`, and §3.6 before touching
search. If a step seems to require a `NOT EXISTS` on `media_classifications`,
an `ANALYZE` inside a replicated statement without the §3.7 note, a
read-pool size chosen without the §3.3 bench, or `PoisonError::into_inner`
without a validation, stop and flag it.

**Correction to the review:** none on S6/S7/S11 beyond what revision 2
already corrected (the `NOT EXISTS` proposal is withdrawn there). One
addition: the tree already contains the correct membership predicate.
`classification::inventory_sql()`
([classification.rs:83-85](../../crates/plurx-core/src/store/classification.rs))
computes `indexed` as `EXISTS(SELECT 1 FROM classification_fts WHERE
rowid=i.id)` — a rowid point lookup on current FTS membership, exactly the
shape §3.6 adopts for search. And the "~70 read-only methods on the writer
mutex" is an undercount: a census of `with_conn` closures with no textual
DML finds 97 candidates across 341 `with_conn` methods (§2.4), of which the
audit rule in §3.1 removes at least twelve.

## 1. Objective

On the standalone SQLite backend, every genuinely read-only Store method
runs on a read connection with snapshot consistency where it needs it;
authentication reads never take the write lock inside the sixty-second
activity window; the Home, library-page, Next-Up and search statements have
plans read from `EXPLAIN QUERY PLAN` on representative data, indexes added
only where a plan justified one, and pagination that is provably the same
order as today; the optimizer has statistics; and a poisoned or corrupt
connection is validated or reopened instead of poisoning every later request.

## 2. Contract today

Re-verify each line at build time.

### 2.1 Connections

```rust
// crates/plurx-core/src/store/sqlite/mod.rs:1298-1300
/// Two: enough that a settings read and a metadata read can overlap, few
/// enough to be nothing on any box this runs on.
const READ_CONNS: usize = 2;
// :1350-1355 (init)
conn.pragma_update(None, "journal_mode", "WAL")?;
conn.pragma_update(None, "foreign_keys", "ON")?;
conn.pragma_update(None, "busy_timeout", 5000)?;
conn.pragma_update(None, "synchronous", "NORMAL")?;
// :1577-1590
async fn with_conn<T, F>(&self, f: F) -> Result<T, StoreError> {
    let conn = Arc::clone(&self.conn);
    tokio::task::spawn_blocking(move || {
        let guard = conn.lock()
            .map_err(|_| StoreError::Task("sqlite connection mutex poisoned".to_owned()))?;
        f(&guard)
    }).await.map_err(|e| StoreError::Task(e.to_string()))?
}
// :1666-1683
pub(crate) async fn with_read<T, F>(&self, f: F) -> Result<T, StoreError> {
    let Some(pool) = self.reads.clone() else { return self.with_conn(f).await; };
    tokio::task::spawn_blocking(move || {
        let idx = pool.next.fetch_add(1, Ordering::Relaxed) % pool.conns.len();
        let guard = pool.conns[idx].lock()
            .map_err(|_| StoreError::Task("sqlite read mutex poisoned".to_owned()))?;
        f(&guard)
    }).await.map_err(|e| StoreError::Task(e.to_string()))?
}
```

Read connections are opened `SQLITE_OPEN_READ_ONLY` after migration
(`:1304-1312`), so a write routed through them fails loudly. No `PRAGMA
optimize`, `ANALYZE`, `quick_check`, `cache_size`, `mmap_size` or
`journal_size_limit` anywhere in this module; `prepare_cached` appears once
in `media.rs` and once in `publication.rs`.

Two facts the migration rule rests on. Under the writer mutex, a closure's
statements cannot interleave with any other writer, so a multi-statement
closure sees one state without asking for it. A read connection in WAL mode
sees one state per *statement* (each implicit transaction starts at the
current end mark) unless the closure opens an explicit read transaction —
then it sees one snapshot for the transaction.

### 2.2 Authentication on the writer

```rust
// crates/plurx-core/src/store/sqlite/users.rs:273-299
async fn user_for_token(&self, token_hash: &str) -> Result<Option<User>, StoreError> {
    self.with_conn(move |conn| {
        let user = conn.query_row(
            "SELECT u.id, u.username, u.password_hash, u.is_admin, u.created_at FROM users u
             JOIN tokens t ON t.user_id = u.id WHERE t.token_hash = ?1",
            params![token_hash], user_from_row).optional()?;
        if user.is_some() {
            conn.execute(
                "UPDATE tokens SET last_seen_at = unixepoch()
                 WHERE token_hash = ?1 AND last_seen_at < unixepoch() - 60",
                params![token_hash])?;
        }
        Ok(user)
    }).await
}
```

The `UPDATE` executes on every authenticated request; when its predicate
matches nothing it still begins a write transaction and takes the WAL write
lock, and it holds the single writer mutex for the whole round trip. The
hiqlite twin already splits read from a rate-gated write
([hiqlite.rs:2963-2999](../../crates/plurx-core/src/store/hiqlite.rs)) using
`crate::auth::activity_refresh_due` and a process-local singleflight.

### 2.3 The hot statements, as executed

Interpolations resolved: `ITEM_COLS`/`item_cols("i")` is the item column
list; `TOP_LEVEL_ITEM_PREDICATE`
([store/mod.rs:2898-2900](../../crates/plurx-core/src/store/mod.rs)) is
`(kind IN ('movie','show','book','audiobook') OR (kind IN
('folder','video','photo') AND parent_id IS NULL))`.

`list_top_items_in_genre` ([media.rs:846-905](../../crates/plurx-core/src/store/sqlite/media.rs)),
`ItemSort::Title`:

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND <TOP_LEVEL> AND
  (?4 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE));
SELECT <ITEM_COLS> FROM items WHERE library_id = ?1 AND <TOP_LEVEL> AND <GENRE>
 ORDER BY sort_title ASC LIMIT ?3 OFFSET ?2;
-- other orders: "added_at DESC, id DESC" · "year IS NULL, year DESC, sort_title ASC"
-- · "COALESCE((SELECT MAX(f.height) FROM files f WHERE f.item_id = items.id), -1) DESC, sort_title ASC"
-- · "(recorded_at IS NULL), recorded_at DESC, sort_title ASC"
```

`recently_added` (`:956-1013`):

```sql
WITH ranked AS (
  SELECT <i cols>, show.title AS rail_show_title, season.poster_path AS rail_season_poster,
         ROW_NUMBER() OVER (
           PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL
                             THEN 'show:' || show.id ELSE 'item:' || i.id END
           ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC,
                    COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank
  FROM items i
  LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode'
  LEFT JOIN items show ON show.id = season.parent_id
  WHERE i.kind IN ('movie','episode','video','folder','book','audiobook')
    AND (?1 IS NULL OR i.library_id = ?1)
    AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')))
SELECT <r cols>, r.rail_show_title, r.rail_season_poster FROM ranked r
 WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT ?2;
```

`search_items` (`:1015-1043`; hiqlite twin at `hiqlite_media.rs:2165` with
`$N`):

```sql
WITH hits AS MATERIALIZED (
  SELECT rowid, rank AS score FROM items_fts WHERE items_fts MATCH ?1
     AND rowid NOT IN (SELECT rowid FROM classification_fts)
  UNION ALL
  SELECT rowid, rank AS score FROM classification_fts WHERE classification_fts MATCH ?1)
SELECT <i cols>, show.title, season.poster_path
  FROM (SELECT rowid, min(score) AS score FROM hits GROUP BY rowid) f
  JOIN items i ON i.id = f.rowid
  LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode'
  LEFT JOIN items show ON show.id = season.parent_id
 WHERE i.kind IN ('movie','show','episode','folder','video','photo','book','audiobook')
 ORDER BY f.score, i.id LIMIT ?2;
```

`continue_watching` ([watch.rs:445-480](../../crates/plurx-core/src/store/sqlite/watch.rs))
filters `w.user_id = ?1 AND w.watched = 0 AND w.position_ms > 0` and orders
by `w.updated_at DESC LIMIT ?2`. `next_up` (`:482-535`) joins
episode→season→show and evaluates four correlated subqueries over
`watch_state` per candidate row (`e.id NOT IN (…)`, `> (SELECT
COALESCE(MAX(…)))`, `show.id IN (…)`, `show.id NOT IN (…)`), `GROUP BY
show.id ORDER BY show.sort_title LIMIT ?2`.

Indexes on these tables ([sqlite/mod.rs:108-143](../../crates/plurx-core/src/store/sqlite/mod.rs)):
`idx_items_library_kind(library_id, kind)`, `idx_items_parent(parent_id)`,
`idx_items_added(added_at DESC)`, `idx_files_item(item_id)`,
`idx_watch_updated(user_id, updated_at DESC)`, `tokens(token_hash)` primary
key, plus partial `idx_items_missing_artwork` and `idx_items_book_work`.
Nothing on `tmdb_id`, `sort_title`, `(kind, parent_id)` or
`watch_state(user_id, watched)`.

### 2.4 Read-only candidates (census, 2026-09-20)

A scan of `sqlite/*.rs` for `async fn` bodies that call `with_conn` and
contain no DML keyword, `.execute(`, `execute_batch` or transaction call
finds 341 `with_conn` methods, 78 `with_read` methods, and 97 candidates.
The census is textual and therefore not proof — `coordination.rs:27
acquire_lease` passes it because its `INSERT … RETURNING` lives in the
constant `ACQUIRE_SQL`, and `fragindex.rs:12,33,38,56,121,156,162`,
`dvr.rs:833`, `telemetry.rs:16,33,62` pass for the same reason. Candidates
that survive a first reading, by file and line (re-run the census at build
time; the script is four lines of Python over `async fn` blocks):

- `users.rs:22 count_users · :51 get_user · :64 get_user_by_username ·
  :78 list_users · :90 list_users_page · :133 count_admins`
- `apikeys.rs:53 list_api_keys · :68 api_key_for_hash` (the touch is
  separate, `extract.rs:526`)
- `library.rs:131 get_library · :144 list_libraries`
- `media.rs:415 item_by_external_id · :451 find_movie · :472
  find_movies_by_directory · :504 find_book · :535 find_show · :560
  find_shows_by_directory · :612 find_season · :630 find_episode · :648
  find_child_item · :734 get_item_children · :752 items_with_artwork · :773
  items_with_artwork_page · :846 list_top_items_in_genre · :1015
  search_items · :1360 book_items · :1465 items_needing_artwork · :1493
  items_needing_metadata · :1524 items_missing_artwork · :1580
  items_missing_genres · :1614 episodes_for_show · :1631 get_file_by_path ·
  :1726 media_shape · :1842 files_missing_dolby_vision · :1869
  files_missing_video_codec_tag · :1950 get_file_probe_json · :1964
  get_file_probe_chapters_json · :1985 files_for_item · :2003
  files_missing_probe · :2027 child_counts · :2046 item_max_heights · :2067
  item_media_facts · :2142 library_file_paths`
- `watch.rs:53 watch_state · :71 watch_map · :364 watch_rollup · :388
  watch_rollups · :445 continue_watching · :482 next_up`
- `reading.rs:30 reading_state · :51 current_reading_state`
- `trakt.rs:46 sealed_trakt_row_census · :188 get_trakt_auth · :201
  list_trakt_auth · :328 trakt_sync_candidates`
- `outbox.rs:88 watched_outbox_counts`
- `offline.rs:259 offline_package_for_user · :315 offline_activity_packages
  · :369 offline_package_stats`
- `cache.rs:38 cache_hit · :162 cache_by_age · :198
  cache_manifest_candidates · :304 all_cache_rows · :321
  cache_ownership_inventory · :346 cache_candidate_owners · :483 cache_bytes`
- `shared_cache.rs:78 cache_storage_member · :123 shared_cache_hit`
- `pretranscode.rs:107 pretranscode_job · :367 pretranscode_staging_jobs ·
  :386 active_pretranscode_job_ids`
- `dv_conversion.rs:159 dv_conversion · sessions.rs:1801 desired_selection`

To be decided by the §3.1 rule, not assumed: `media.rs:405
identity_repair_snapshot · :1045 apply_series_tmdb_hint`,
`cache.rs:271 stale_cache_claims`, `shared_cache.rs:375
stale_shared_cache_claims · :564 shared_cache_gc_candidates`,
`sessions.rs:3808 producer_recovery_for_epoch`, `offline.rs:607
offline_package_claim_is_current`.

### 2.5 Classification triggers

```sql
-- crates/plurx-core/src/store/classification.rs:19-32
CREATE TRIGGER IF NOT EXISTS classification_ai AFTER INSERT ON media_classifications BEGIN
 INSERT INTO classification_fts(rowid,terms) VALUES(new.item_id, new.terms || ' ' || …title… || …overview… || …genres… || …tags…);
END;
CREATE TRIGGER IF NOT EXISTS classification_ad AFTER DELETE ON media_classifications BEGIN
 DELETE FROM classification_fts WHERE rowid=old.item_id;
END;
CREATE TRIGGER IF NOT EXISTS classification_au AFTER UPDATE OF terms ON media_classifications BEGIN
 DELETE FROM classification_fts WHERE rowid=old.item_id;
 INSERT INTO classification_fts(rowid,terms) VALUES(new.item_id, …);
END;
CREATE TRIGGER IF NOT EXISTS classification_source_changed AFTER UPDATE OF title,overview,genres,tags,year,tmdb_id,kind ON items
WHEN old.kind IS NOT new.kind OR old.title IS NOT new.title OR … BEGIN
 DELETE FROM classification_fts WHERE rowid=new.id;
END;
```

The last trigger is the semantics that matter: a title change **deletes the
FTS row and keeps the `media_classifications` row** (for regeneration by
the classifier, which re-checks `source_json` against `SOURCE` before it
writes). So "this item has a current classification index entry" is
membership of `classification_fts`, never existence of a
`media_classifications` row. The current predicate `rowid NOT IN (SELECT
rowid FROM classification_fts)` is correct and costs a full scan of the FTS
table's rowids per query; the withdrawn `NOT EXISTS (… media_classifications
…)` would hide every renamed title from both branches.

### 2.6 Poison, boot checks, row layout

A panic inside any closure poisons the mutex, and every later call returns
`StoreError::Task("sqlite connection mutex poisoned")` (`:1583`, `:1678`)
until restart. No `quick_check` runs at open. `files` was created with
`probe_json TEXT` before `scanned_at` (`:112-131`); every later column
(`hdr_format`, `audio_offset_ms`, `dv_*`, `video_codec_tag`) was appended
after it, so reading a trailing column walks past a 5–30 KB text cell and
its overflow pages. `FILE_COLS` (`:1218-1223`) already selects `(probe_json
IS NOT NULL)` instead of the text, which is why ordinary file reads are not
as bad as the layout suggests.

## 3. Change

### 3.1 `with_read` migration rule (S6, F-sc-6)

A method moves from `with_conn` to `with_read` only when all four hold, and
the PR lists them per method:

1. **No DML anywhere it can reach**: the closure, every helper it calls,
   and every `const` SQL it references (resolve constants the way
   `placeholder_census.rs` does). The read connection is `READ_ONLY`, so a
   mistake fails at runtime; the rule exists so it fails in review instead.
2. **Snapshot need declared.** A single-statement closure keeps per-statement
   consistency and needs nothing. A multi-statement closure whose statements
   must agree (page + count in `list_top_items_in_genre`; the census in
   `sealed_trakt_row_census`) uses the new `with_read_txn`, which wraps the
   closure in `BEGIN DEFERRED … COMMIT` on the read connection so it sees one
   WAL snapshot. Every migrated multi-statement closure names which it chose.
3. **Not a pre-mutation read.** A read whose result decides a write in the
   same request (`get_item` before `set_watched_tree`) stays on the writer's
   serialization; the mutex is what made read-then-write atomic.
4. **Not dependent on the writer's uncommitted state.** Nothing in the
   candidate list is, but a closure called from inside another writer
   closure would be; the rule says so.

Order of migration: Home and library-page reads first (`watch.rs`,
`media.rs` catalogue reads), then users/apikeys/library, then the rest.
Each batch is one PR.

### 3.2 `user_for_token` split (S6)

```sql
-- read, on with_read; adds t.last_seen_at like the hiqlite twin
SELECT u.id, u.username, u.password_hash, u.is_admin, u.created_at, t.last_seen_at
  FROM users u JOIN tokens t ON t.user_id = u.id WHERE t.token_hash = ?1;
-- write, on with_conn, only when activity_refresh_due(last_seen_at, now):
UPDATE tokens SET last_seen_at = ?2 WHERE token_hash = ?1 AND last_seen_at < ?3;
```

with `now` and `now − ACTIVITY_REFRESH_SECS` bound as parameters and the
same process-local singleflight the hiqlite side uses. Races, and why each
is closed:

- **Delete before read.** `delete_token` commits under the writer; a read
  transaction that begins afterwards sees the deletion (WAL readers observe
  every commit completed before their start). Same guarantee the mutex gave.
- **Delete between read and write.** The `UPDATE` matches zero rows; an
  `UPDATE` cannot resurrect a row. The request that already read the token
  is authenticated for that request, exactly as today when the delete lands
  one instruction after the read returned.
- **Concurrent window-edge burst.** The singleflight admits one write per
  credential per process; the SQL predicate is the final guard.

### 3.3 Read-pool size by measurement (S6, F-sc-6)

Bench first, then decide. A fixture generator
(`scripts/bench/catalogue-fixture`, new) builds `fixture.db` with 4
libraries, 20k movies, 500 shows × 8 × 12 episodes, 100k files with real
probe JSON sizes, 5 users with watch rows on 10 % of items. A driver runs the
Home shape (auth read + `home_preview_pages` + `watch_map` + `watch_rollups`
+ `continue_watching` + `next_up`) at concurrency 32 for 256 iterations
while a scan-shaped writer inserts 50 files/s, for `READ_CONNS ∈ {2, 4, 8}`,
recording read p50/p95, write p99 and process RSS. Adopt the smallest value
whose read p95 improves ≥ 20 % without raising write p99 or RSS beyond the
recorded budget; otherwise keep 2. Each connection carries its own page
cache (`cache_size` default ≈ 2 MiB) and prepared-statement cache, which is
the multiplication the assessment warns about; §3.8 prefers `mmap_size`
over per-connection cache for the same reason. If a change is warranted,
`READ_CONNS` becomes `storage.sqlite_read_conns` (`1..=16`, default the
measured value) with the range documented in OPERATIONS.md.

### 3.4 `EXPLAIN QUERY PLAN` protocol (S7, F-sc-7)

For each statement in §2.3, plus `item_by_external_id`, `watch_map`,
`files_for_item`, `item_media_facts`:

1. Take the SQL **as executed** (interpolations resolved) from a debug
   log line added behind `RUST_LOG=plurx_core::store::sqlite=trace`.
2. Run `.eqp full` in `sqlite3 fixture.db` and record the plan verbatim.
3. Time it (`.timer on`, five runs, median) on the fixture, cold and warm.
4. Propose an index only against a plan line that shows `SCAN items`,
   `USE TEMP B-TREE FOR ORDER BY` on a paged query, or a correlated
   subquery executed per row; write the new plan and timing beside the old.
5. Repeat on the hiqlite state-machine database (same schema, `$N`
   placeholders) — the plan is per schema, and both backends must show it.

Evidence lands in `benchmarks/evidence/query-plans-<sha>.md`; the PR body
carries the before/after table. Expected findings, stated as hypotheses the
plans will confirm or refute, not as results:

| Statement | Hypothesis | Candidate |
|---|---|---|
| `list_top_items_in_genre`, `Title` | index on `(library_id, kind)` then `TEMP B-TREE` for `ORDER BY sort_title`; `OFFSET` walks the sorted set | `items(library_id, kind, sort_title, id)`; with keyset (§3.5) the sort disappears |
| `list_top_items_in_genre`, genre filter | `json_each` per row; unavoidable without a genre table | none here; note the cost |
| `recently_added` | window function over every top-level item; `SCAN` | §3.5's widening candidate window over `idx_items_added` |
| `item_by_external_id` (scan hot path) | `SCAN items` per lookup on `tmdb_id` | `items(tmdb_id)` partial `WHERE tmdb_id IS NOT NULL` |
| `next_up` | four correlated subqueries per episode row | one CTE per user (§3.5) |
| `continue_watching` | `SEARCH watch_state USING INDEX idx_watch_updated`; fine | none |
| `search_items` | `SCAN classification_fts` for the `NOT IN` per query | §3.6 |

The review's warning is honoured: a composite that leads with `kind` is
not proposed for a query that filters on `library_id`; every candidate above
leads with the equality column the plan shows.

### 3.5 Keyset pagination and the `recently_added` window (S7)

**Order equivalence first.** `ORDER BY sort_title ASC` is not a total order;
two items with the same `sort_title` can swap between pages under `OFFSET`
today. Before any keyset change, every sort gains `, id ASC` (or `DESC`
matching the leading direction) as the final key in **both** the offset and
the keyset variants, so the two are the same total order. The proof is a
test: a fixture with duplicated `sort_title`, `year` and `added_at` values,
paged fully by offset and by keyset with page sizes 1, 7 and 50, must
produce identical item-id sequences for every `ItemSort`. The `Resolution`
and `Recorded` sorts order by an expression; their cursor carries the
expression's value and the id, and the same test covers them.

**`recently_added`.** Dedupe-by-group over a stream ordered by
`(added_at DESC, id DESC)` emits the correct top-L representatives if it
sees every row down to the L-th distinct group: a group's representative is
its newest row, and no row outside the newest-K window can outrank a row
inside it. So: read the newest K = 8 × limit rows through `idx_items_added`
(extended to `(added_at DESC, id DESC)` so the cut is on the total order
and ties at the boundary cannot straddle it), apply the same kind/library/
recordings predicates, dedupe, and if fewer than `limit` groups resulted,
double K and repeat until complete or the table is exhausted. The
assessment's condition — "correct only if widening continues until the
original distinct-group result is complete, including ties and filters" —
is the loop's exit condition and its test: a fixture where one show has
9 × limit new episodes proves the second pass.

**`next_up`.** Rewrite as one CTE that computes, per show the user has
watched anything in, `max_watched_ord` and `has_in_progress`, then join
candidate episodes to that set — the same predicates, evaluated once per
show instead of once per episode. Equivalence: a property test over seeded
random watch states (200 seeds) compares old and new outputs item-for-item
on both backends.

### 3.6 The search predicate (F-sc-8, corrected)

```sql
WITH hits AS MATERIALIZED (
  SELECT rowid, rank AS score FROM items_fts
   WHERE items_fts MATCH ?1
     AND NOT EXISTS (SELECT 1 FROM classification_fts c WHERE c.rowid = items_fts.rowid)
  UNION ALL
  SELECT rowid, rank AS score FROM classification_fts WHERE classification_fts MATCH ?1)
…unchanged…
```

Same relation as `rowid NOT IN (SELECT rowid FROM classification_fts)` —
membership of the *current* FTS table — evaluated as a rowid point lookup
per hit (FTS5 answers `rowid = ?` from its content table's primary key)
instead of a materialized scan of every FTS rowid per query. Semantics are
unchanged by construction: a renamed title's FTS row was deleted by
`classification_source_changed`, `NOT EXISTS` is true, and the item is found
through `items_fts`, whose own `items_fts_au` trigger already carries the new
title. The `media_classifications` table is not consulted. Tests on both
backends: rename a movie ("Harbor Lights" → "Night Tide") after it was
classified; searching "Night Tide" finds it, searching "Harbor Lights" does
not, and the classification row still exists; a classified, unrenamed item
scores from `classification_fts` only (no duplicate hit). Plan: `.eqp` must
show a `SEARCH classification_fts` correlated lookup, not `SCAN`.

### 3.7 `PRAGMA optimize` and `ANALYZE` cadence (S7)

Standalone SQLite: `PRAGMA analysis_limit = 400` at open; `PRAGMA optimize`
on the writer at `SqliteStore` shutdown and after every completed library
scan (the scan already ends on the writer); `ANALYZE items; ANALYZE files;
ANALYZE watch_state;` after the SQLite→hiqlite import and after a scan that
inserted or deleted more than 10 % of `items`. Hiqlite: the vendored writer
already runs `PRAGMA optimize` after every snapshot, migration and backup
([writer.rs:496, 520, 681](../../vendor/hiqlite/src/store/state_machine/sqlite/writer.rs)),
which at the current cadence is every 10k entries; plurx adds the same
bounded `ANALYZE` batch as a replicated statement after a scan completes.
That statement is deterministic in effect (each voter samples its own
identical rows) but not byte-identical across voters; `replicated.rs`
forbids clock/random identifiers, which `ANALYZE` is not, and `sqlite_stat1`
is optimizer advice, not application state — recorded as open question 2
because it is the one place this plan puts a non-idempotent-looking
statement on the log.

### 3.8 Housekeeping pragmas, measured (S11, F-sc-14)

Candidates, each adopted only with a §3.3 bench delta attached:
`journal_size_limit = 64 MiB` (bounds the WAL file after checkpoint; no
per-connection cost), `mmap_size = 256 MiB` (one OS page cache shared by
every connection — the alternative to multiplying `cache_size`),
`temp_store = MEMORY` (the sort and `json_each` temporaries), and
`prepare_cached` for `user_for_token`, `watch_map`, `get_item`,
`files_for_item`, `item_media_facts` (measure prepare time first; it is
tens of microseconds per statement and may not register). `cache_size` is
left at the default unless the bench shows the read pool is cache-bound.

### 3.9 Poison and boot integrity (S11, F-sc-14)

On `PoisonError` from either mutex: take `into_inner()`, then **validate
before use** — `SELECT 1`, `PRAGMA quick_check(1)`, and `conn.is_autocommit()`;
if a transaction is open, `ROLLBACK`; if any check fails, reopen the
connection from the stored path with the same flags and pragmas and swap it
into the slot. Count `plurx_sqlite_connection_recoveries_total{pool="writer"|
"read",outcome="validated"|"reopened"|"failed"}` and log once per event.
A closure panic mid-transaction is the case that matters: rusqlite's
transaction guards roll back on drop, so the state after `into_inner` is
usually consistent, and the validation is what proves "usually" for this
instance instead of assuming it.

Boot: `PRAGMA quick_check(1)` on the writer inside `spawn_blocking` with a
30 s budget. `ok` → continue. An error → refuse startup, naming the file, the
first error line, and the two recoveries (restore the newest artefact per
[CLUSTER-BACKUP-AND-RESTORE.md](CLUSTER-BACKUP-AND-RESTORE.md) for an
activated node; `sqlite3 .recover` into a new file for standalone). A timeout
→ start with a warning and schedule a full `integrity_check` in the
background at the next idle hour, reporting through the same metric with
`outcome="deferred"`. The policy is explicit so an operator never meets a
silent slow start or a silent corrupt one.

### 3.10 `probe_json` row layout (S11)

A `file_probes(file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE
CASCADE, probe_json TEXT NOT NULL)` side table is a **row-access** change:
trailing `files` columns stop paying the overflow walk. It is not a snapshot
or backup size change (every page is still copied —
[RAFT-SNAPSHOT-CADENCE-AND-CONSISTENT-CUT.md](RAFT-SNAPSHOT-CADENCE-AND-CONSISTENT-CUT.md)
§3.3). It costs a schema step on both backends and every `FILE_COLS` reader.
Decide from the §3.4 timings of `files_for_item` and `item_media_facts` on
the fixture: adopt only if the trailing-column reads are measurably in the
overflow path; otherwise record the measurement and stop.

## 4. Guardrails (non-goals)

- **No `NOT EXISTS` on `media_classifications`** (assessment correction 11).
  §3.6 tests the renamed-title case on both backends.
- **No `with_read` for a pre-mutation read**, and no multi-statement closure
  on a read connection without `with_read_txn` when its statements must
  agree (rule 2/3 in §3.1).
- **No read-pool size without the §3.3 bench**; eight is not a default.
- **No index without its plan**; no composite leading with a column the
  query does not filter on.
- **No pagination change without the order-equivalence test** and the
  `, id` tiebreak landing first.
- **No `into_inner` without validation**; no silent `quick_check` policy.
- **No `probe_json` move as a snapshot remedy.**
- **Recipe identity, cache digests and the catalogue digests
  (`hiqlite_catalog.rs:301-306`) are untouched**: indexes and statistics
  change no row bytes. If §3.10 is adopted, the catalogue digest inputs are
  re-checked in that PR because `files` columns move.

## 5. Milestones

### 5.1 M0 — fixture, plans and timings

Fixture generator; the debug log line with resolved SQL; plans and timings
for every §3.4 statement on both schemas; `benchmarks/evidence/query-plans-<sha>.md`.

Acceptance: the evidence file lists each statement with its verbatim `.eqp`
output and median timing; no code change to queries in this PR.

### 5.2 M1 — `with_read` batch 1 and `with_read_txn`

Home and library-page reads from §2.4 (`watch.rs`, `media.rs` catalogue
methods, `library.rs`), each with the four-rule checklist in the PR.

Acceptance: `cargo test -p plurx-core store::sqlite` green; the existing
`reads_see_writes_and_do_not_queue_behind_the_writer` (`mod.rs:2139`) is
extended to hold the writer in a 2 s transaction while `home_preview_pages`
and `list_top_items_in_genre` return in < 100 ms; a new test proves
`with_read_txn` gives page and count from one snapshot under a concurrent
insert. `make cluster-store-check` unchanged (the trait contract is the same).

### 5.3 M2 — `user_for_token` split

Acceptance: `cargo test -p plurx-core store::sqlite::users::token_` : 100
concurrent authentications write `last_seen_at` once; delete-then-read
returns `None`; read-then-delete-then-touch leaves the token deleted; the
writer mutex is not taken inside the window (assert via a held writer
transaction while authenticating).

### 5.4 M3 — search predicate, both backends

Acceptance: `cargo test -p plurx-core search_renamed_title` on SQLite and
under `make cluster-store-check` on hiqlite; `.eqp` in the PR body shows
`SEARCH classification_fts` for the subquery.

### 5.5 M4 — tiebreaks, keyset, `recently_added` window, `next_up` CTE

Acceptance: `cargo test -p plurx-core pagination_order_equivalence` (all
sorts, page sizes 1/7/50, duplicated keys); `recently_added_widens_window`
(9 × limit episodes in one show); `next_up_property` (200 seeds, both
backends); the M0 plans re-taken show the `TEMP B-TREE` gone for `Title`
paging.

### 5.6 M5 — indexes justified by M0/M4 plans

One migration step on each backend per adopted index; `EXPLAIN` before/after
in the PR body.

Acceptance: `cargo test -p plurx-core migration` (schema version bump both
sides, `database-upgrades` check); M0 timings re-run show the improvement
claimed.

### 5.7 M6 — statistics, pragmas, poison recovery, boot check

Acceptance: `cargo test -p plurx-core store::sqlite::housekeeping` :
`sqlite_stat1` populated after a scan; a closure that panics leaves the next
call successful with `outcome="validated"` incremented; a deliberately
truncated fixture refuses startup with the documented message; pragma
adoptions carry bench deltas in the PR body.

### 5.8 M7 — read-pool decision and `probe_json` decision

Acceptance: both decisions recorded in the PR body with the §3.3 table; if
`READ_CONNS` changes, the config key exists with its range test; if §3.10 is
adopted, `FILE_COLS` readers and the catalogue digest tests are updated in
the same PR.

## 6. Verification and rollout

Fast lane per PR: `make unit` (this is the lane that runs the SQLite
contracts) plus the named filters; M3 and M5 also `make cluster-store-check`
and the `database-upgrades` check. Rollout: M1–M4 are behaviour-preserving
on both backends and ship in order; M5's schema steps ride the documented
replicated-migration procedure in OPERATIONS.md (stop traffic, update every
voter as one operation) and are the only milestones an older binary refuses
after; M6's boot check ships with the refusal text in OPERATIONS.md. Rollback
for M1–M4 and M6 is a redeploy; for M5 it is the existing newer-schema
refusal, so M5 is deployed last and alone.

## 7. Open questions

1. §3.3's 20 % threshold and the 32 × 256 load shape — proposals.
2. Whether a replicated `ANALYZE` (§3.7) is acceptable on the hiqlite log,
   or whether each voter should run it locally on its writer at snapshot
   time (a vendor-side hook) — the latter keeps the log free of it; the
   former is one line.
3. `next_up`'s ordering key `season*100000 + episode` overflows nothing
   realistic but is a magic constant shared with clients' expectations of
   "next"; the CTE rewrite keeps it — confirm nobody wants air-date order
   while the statement is open.
4. If the Resolution sort's correlated `MAX(f.height)` shows up in M0 as
   the slowest page, a materialized `items.max_height` maintained by the
   scan is a schema change beyond this plan's scope; flag, do not build.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | M0 (partial) | [#429](http://192.168.4.7:3000/noirr/plurx/pulls/429) | Added the production-schema fixture generator and regenerated it from committed generator `d87861a1afcc4cdedfdec91f3aa81c079b48cd6d`: 4 libraries; 20,000 movies; 500 shows × 8 seasons × 12 episodes; Home with 100 folders, 1,000 videos and 1,000 photos; 1,000 recordings; 100,000 semantically owned files with 5–30 KiB probe JSON; 5 users and 35,000 playable-leaf watch rows. Its executable census proved all four top-level Home rails, the non-empty Recordings exclusion and zero invalid file/watch owners. The disposable database was 1,870,753,792 bytes and generated in 18.34 s from the warm release build. M0 remains incomplete: no actual Hiqlite state-machine fixture, verbatim plans, cold/warm medians or 32-way writer-contention run exists. Per §§3.3–3.4, M1–M7 did not start. The requested `scripts/bench/catalogue-fixture` path cannot exist because `scripts/bench` is the tracked playback benchmark file; the equivalent executable is `cargo run -p plurx-core --example catalogue_fixture --release -- <new-output.db>`. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M0 (rest) | [#502](http://192.168.4.7:3000/noirr/plurx/pulls/502) | `012d8a3a3`: each hot read on both backends logs its statement name and SQL (interpolations resolved) at TRACE under `plurx_core::store::{sqlite,hiqlite}` (`trace_statement`), and the `query_plans` example drives the real `SqliteStore` on the fixture with a subscriber that keeps only those events, so every plan is taken on the SQL that ran. An ignored `store_contract` test (`k05_capture_hiqlite_statements`) does the same against a bootstrapped three-voter cluster and records the state machine's `sqlite_master` as the cluster created it; `query_plans build-hiqlite` creates that schema in a file and loads the same fixture population into it by column name (items, files, watch rows, users, tokens, classifications; FTS rebuilt), and `measure` records each plan verbatim with five cold (file written back and evicted from the page cache, fresh connection) and five warm runs. The Hiqlite database is therefore the replicated schema with the fixture's rows, **not** a Raft-replicated load: a plan depends on schema, indexes, statistics (none on either) and data, all of which match, so the plans are the replicated backend's; timings exclude Raft and the client round trip. Evidence: `benchmarks/evidence/query-plans-012d8a3a.md` (nuc3, 16 cores, bundled SQLite 3.53.2; lab measurement, not fleet evidence). Hypotheses: **confirmed** — library pages search `idx_items_library_kind (library_id=?)` then sort in a temp B-tree (7-42 ms warm for the 50-row page at offset 10,000, Resolution the slowest because of its correlated `MAX(height)`); the genre filter is a `json_each` scan per row; `recently_added` scans every item (164 ms warm, the slowest statement); `item_by_external_id` scans `items` (3.3 ms per scanner lookup); `continue_watching` searches `idx_watch_updated` and is cheap; search scans all of `classification_fts` for the `NOT IN`. **Refuted** — `next_up` is not four correlated subqueries per episode row: SQLite evaluates three of them once as list subqueries with Bloom filters and only the `MAX` per show (8.6 ms warm). **Found** — `watch_map` walked every watch row the user has and scanned the id list per row (15.6 ms warm for a 50-id page); `home_preview_pages` sorts every top-level item (29 ms). The 32 × 256 read-pool bench is recorded under M7. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M1 | [#502](http://192.168.4.7:3000/noirr/plurx/pulls/502) | `51bb100a8`: 17 methods move to the read pool — `list_top_items_in_genre` (on the new `with_read_txn`, `BEGIN DEFERRED` held for the closure, so page and count come from one WAL snapshot), `recently_added`, `search_items`, `get_item_children`, `episodes_for_show`, `files_for_item`, `child_counts`, `item_max_heights`, `item_media_facts` (media.rs); `watch_map`, `watch_rollup`, `watch_rollups`, `continue_watching`, `next_up` (watch.rs); `get_library`, `list_libraries` (library.rs). Section 3.1 checklist, all 17: **(1) no DML** — each closure is `SELECT`/`WITH … SELECT` only, and the helpers it reaches (`item_from_row`, `file_from_row`, `watch_from_row`, `library_from_row`, `sql_source::next_up`, `sql_source::recently_added`) build values or SQL text and execute nothing; **(2) snapshot** — every closure is one statement except `list_top_items_in_genre` (count + page, `with_read_txn`) and, after M4, `recently_added` (one statement per widening pass; each pass is exact for the state it read, so it declares no shared snapshot); **(3) not pre-mutation** — none decides a write in the same closure; their callers are the browse, Plex and Home handlers, which render; the scanner's lookups that decide inserts (`find_*`, `item_by_external_id`, `get_file_by_path`) deliberately stay on the writer; **(4) not dependent on the writer's uncommitted state** — all are trait methods, never called from inside a writer closure. Premise correction: the plan's example of a pre-mutation read, `get_item` before `set_watched_tree`, was already on `with_read` before this plan (`media.rs:701`), and line numbers in section 2.4 have drifted. Tests: `home_and_library_page_reads_do_not_queue_behind_a_held_writer_transaction` (every migrated read returns within a 2 s bound while the writer holds `BEGIN IMMEDIATE` with an uncommitted insert, and none sees it; the writer is released only after all return, so a regression times out rather than passing slowly — a stronger form of the plan's "< 100 ms under a 2 s hold") and `read_transactions_see_one_snapshot_across_statements` (a commit between two statements is invisible under `with_read_txn`, visible under `with_read` as the control). Reverting `list_libraries` to `with_conn` and `with_read_txn` to a bare `f(conn)` fails each. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M2 | [#502](http://192.168.4.7:3000/noirr/plurx/pulls/502) | `faddc8a5f`. Premise correction: `user_for_token` is now the trait default over `authenticate_token`, which also reads the token-expiry policy in the same statement; the split applies to it. The read (user, token, policy, `unixepoch()`) is on `with_read`; an expired token returns without a write; a due refresh (`activity_refresh_due`) runs on the writer only for the one request per token per `ACTIVITY_REFRESH_SECS` that the new process-local `TokenActivityGate` admits (the standalone twin of the replicated `ActivityRefreshGate`; a reservation is kept only when its write committed), and binds `now` and `now - ACTIVITY_REFRESH_SECS` from the read's own clock. Tests (`store::sqlite::users::tests::token_*`): `token_refresh_is_single_flight_and_reads_skip_the_writer` (100 concurrent requests from a due token while the writer is held: 99 are served, exactly one waits, the refresh lands once), `token_deleted_before_the_read_is_unknown`, `token_read_then_deleted_then_touched_stays_deleted`. Reverting the read to `with_conn` fails the first and third. `fixture_set_token_last_seen` now also forgets the process's reservation for that token. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M3 | [#502](http://192.168.4.7:3000/noirr/plurx/pulls/502) | `6ae989dc0`: both backends use `NOT EXISTS (SELECT 1 FROM classification_fts c WHERE c.rowid = items_fts.rowid)`. `search_renamed_title_is_found_by_its_new_title_only` passes on the in-memory and file SQLite stores and on three Hiqlite voters; swapping in the withdrawn `media_classifications` predicate fails it ("memory: new title", `[]`). Premise correction on the acceptance: `EXPLAIN QUERY PLAN` never prints `SEARCH` for a virtual table. The plan went from `LIST SUBQUERY 1 / SCAN classification_fts VIRTUAL TABLE INDEX 0:` (every rowid, per query) to `CORRELATED SCALAR SUBQUERY 1 / SCAN c VIRTUAL TABLE INDEX 0:=`, where idxStr `=` is FTS5's rowid-equality plan — the per-hit point lookup section 3.6 asks for — on both schemas; search 1.0 → 0.6 ms warm. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M4 (partial) | [#502](http://192.168.4.7:3000/noirr/plurx/pulls/502) | **Already on main before this PR:** every `ItemSort` ends in `, id` (`37b974059`, "give every library sort a total order and ship its key"), so the offset order is total. **Built:** `d493e121b` `recently_added` reads a widening window (8 rows a card, doubled while a cut window yields too few cards; cut on `added_at` alone so boundary ties stay inside) through one `sql_source` statement for both dialects and all three call sites; `recently_added_widens_window` and `recently_added_matches_the_whole_catalogue_ranking` (200 seeds, the old statement as oracle) pin it, and removing the widening loop fails the first (left `[135]`, right `[135, 1003, 1002, 1001]`). On the fixture: 164 → 17 ms warm for the global rail (five passes), 98 → 16 ms for one library. `668c01d3f` also rewrites `watch_map` (from the M0 plans; no index needed) to drive from the id list with one `(user_id, item_id)` key lookup per id: 15.6 → 0.016 ms warm, pinned by `watch_map_looks_up_each_requested_id`'s plan assertion; restoring the old statement fails it (`SEARCH w USING INDEX idx_watch_updated (user_id=?)`, `SCAN j VIRTUAL TABLE INDEX 1:`). **Found while merging the three call sites:** the replicated Authority `recently_added` had never excluded Recordings from the catalogue-wide rail, unlike the local read and the standalone store; fixed in this PR (row "Fix" below). **Not built:** keyset pagination (no endpoint takes a cursor today; adding one is an API change the client plans would have to adopt, so the Title page's temp B-tree stays), and the `next_up` CTE (M0 refuted the per-episode premise; 8.6 ms warm). |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M5 | [#502](http://192.168.4.7:3000/noirr/plurx/pulls/502) | **Not built.** Candidates the plans justify: `item_by_external_id` scans `items` (3.3 ms warm per scanner lookup; a partial `items(tmdb_id) WHERE tmdb_id IS NOT NULL` plus an `imdb_id COLLATE NOCASE` index, with the OR rewritten so the planner can use them), and the library pages' temp-B-tree sorts. Each is a schema step on both backends that the plan deploys last and alone; it is left for its own PR. |
| 2026-09-25 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M6 | [#502](http://192.168.4.7:3000/noirr/plurx/pulls/502) | `213820c2c`: poisoned-connection recovery on both pools (roll back, `SELECT 1`, `quick_check(1)`, writer foreign keys; else reopen from the path; in memory, report `failed`) and the boot `quick_check(1)` (30 s budget; refuse with the recovery named; over budget, warn and run `integrity_check` in the background at the top of the next hour), counted in `plurx_sqlite_connection_recoveries_total{pool,outcome}` and `plurx_sqlite_integrity_checks_total{phase,outcome}` on `/metrics` (17 placeholders, 17 arguments); refusal text and series in OPERATIONS.md ("Standalone SQLite: connection recovery and the boot integrity check"). Seven tests under `store::sqlite::housekeeping`; restoring the plain `lock()` error and dropping the boot check fails five of them (`a_panicking_writer_closure_is_validated_and_rolled_back`, `a_panicking_read_closure_leaves_reads_working`, `a_writer_left_without_foreign_keys_is_reopened`, `an_in_memory_writer_that_fails_validation_is_not_replaced`, and `a_truncated_database_refuses_startup_with_the_recovery_named`, which then gets SQLite's bare "database disk image is malformed" from migration); the budget and exposition tests are not behaviour pins. Deviations: the plan's `outcome="deferred"` on the recoveries metric is a separate integrity-check series, because a deferred boot check is not a connection recovery; "the next idle hour" is implemented as the top of the next wall-clock hour. **Section 3.7 statistics not adopted:** with `analysis_limit = 400; ANALYZE` on the fixture, no measured statement improved beyond noise and `watch_rollups` became 3.5× slower (2.3 → 8.0 ms warm; the planner then scans every item through `idx_items_library_kind` instead of driving from the show trees), so neither `PRAGMA optimize` after scans nor the `ANALYZE` batch ships, the M6 acceptance item "`sqlite_stat1` populated after a scan" does not apply, and open question 2 (replicated `ANALYZE`) does not arise. **Section 3.8 pragmas were not benched and are not adopted:** the plan admits each only with a §3.3 bench delta, and none was measured. |
| 2026-09-25 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M7 | [#502](http://192.168.4.7:3000/noirr/plurx/pulls/502) | `27f729521` adds `query_plans bench <fixture.db> <reads>` (and the doc-hidden `SqliteStore::open_with_read_connections`): the §3.3 Home shape 256 times across 32 workers, users 1-5, with a writer upserting 50 files a second, one process per size, two interleaved rounds on nuc3 (load average 3.6-4.6). Read p95 (ms) 1432.9/1727.8 at 2 connections, 1377.6/1421.9 at 4, 1754.0/1805.6 at 8; write p99 3.1/2.9, 3.4/3.7, 4.3/4.3 ms; peak RSS 188/171, 204/203, 205/195 MiB. **Decision: `READ_CONNS` stays 2** — four improved p95 by 3.9 % and 17.7 % (under the 20 % rule) while raising write p99 and RSS; eight was worse on both. No config key is added. Observed, not investigated: wall time (7.3-9.9 s per 256 Homes) did not fall with more connections, so this load does not queue on the pool. **§3.10 `probe_json` not adopted:** `files_for_item` is 0.02 ms warm (1.0-1.2 ms cold) and `item_media_facts` 0.26 ms for a 50-item page, too little for a schema split to repay. Table and method in the evidence doc's M7 section. |
| 2026-09-25 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | Fix (found in M4) | [#502](http://192.168.4.7:3000/noirr/plurx/pulls/502) | `ea62fa80d`: the replicated Authority `recently_added` (the consistent path the bounded catalogue reader falls back to, and every caller of the trait method on a cluster) now leaves Recordings libraries out of the catalogue-wide rail like the standalone store and the replicated local read; the `exclude_recordings` flag is gone from `sql_source::recently_added`. `catalogue_recently_added_leaves_out_recordings_on_every_backend` (store contract: in-memory and file SQLite, three Hiqlite voters) asserts the catalogue rail is exactly the movie and the Recordings library's own rail exactly the recording; restoring the old Authority statement fails it on `hiqlite-3-voter` (left `[2, 1]`, right `[1]`). Coverage: `validation/regressions.d/ea62fa80-authority-recent-recordings.toml`. The Hiqlite "after" timings in the evidence doc predate this predicate. |
