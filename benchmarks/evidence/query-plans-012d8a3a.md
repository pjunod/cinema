# K-05 query-plan evidence — M0 plans and timings, before and after

**Status:** M0 plans and cold/warm medians for both schemas, before and after
this PR; the M7 read-pool bench; the §3.7 statistics trial · **Tool:**
`012d8a3a3` (`query_plans` example, statement capture), bench `27f729521` ·
**Base:** `main@0e2c3fd47` · **Captured:** 2026-09-24/25 on nuc3 (16 cores,
30 GiB, bundled SQLite 3.53.2) · **Plan:**
[SQLITE-READ-PATH-AND-QUERY-PLANS.md](../../docs/cluster/SQLITE-READ-PATH-AND-QUERY-PLANS.md)

This replaces the prerequisite boundary recorded in
[query-plans-d87861a1.md](query-plans-d87861a1.md): the statement plans and
medians it listed as missing are below, for both backends.

## How it was taken

```sh
cargo run -p plurx-core --release --example catalogue_fixture -- fixture.db
query_plans capture-sqlite fixture.db sqlite.json
K05_HIQLITE_CAPTURE=hiqlite.json cargo test -p plurx-core \
  --features cluster-read-cost-validation,hiqlite-contract-tests \
  --test store_contract -- --ignored --exact k05_capture_hiqlite_statements
query_plans build-hiqlite fixture.db hiqlite.json hiqlite-fixture.db
query_plans measure fixture.db sqlite.json
query_plans measure hiqlite-fixture.db hiqlite.json
query_plans bench fixture.db 2   # and 4, 8
```

- **Statements as executed.** Each hot read logs its statement name and SQL
  at TRACE (`trace_statement`); `capture-sqlite` drives the real
  `SqliteStore` against the fixture and keeps those events, and the ignored
  contract test does the same against a bootstrapped three-voter Hiqlite
  cluster. The calls and their arguments are shared (`examples/query_plans/calls.rs`)
  and name fixture rows: a 50-row page at offset 10,000 of the 20,000-movie
  library, user 1 (7,000 watch rows), the 24-card rails.
- **The Hiqlite database** is the state machine's schema exactly as the
  cluster created it (`sqlite_master`, creation order), loaded with the same
  population by column name, FTS tables rebuilt. It is not a Raft-replicated
  load: plans depend on schema, indexes, statistics (none on either database)
  and rows, which all match; timings exclude Raft and the client round trip.
- **Timings** are medians of five runs. Cold: the database file is written back
  and evicted from the OS page cache (`posix_fadvise(DONTNEED)`) and a fresh
  read-only connection runs the statement. Warm: one connection, after one
  untimed run. The host was shared with other builds (load average up to 14
  on 16 cores); differences under ~15% between separate runs are noise.
- **After** is this branch (`27f729521` for the timings; the tool binds every
  `recently_added` pass its own window, so a call that widens shows one row
  per pass and the per-call table sums them).

## What changed, in one table

Warm medians in ms; the full rows, plans and bindings are in the appendices.
The Hiqlite "after" run was taken under heavier host load than the rerun of
"before" taken beside it: statements this PR did not touch
(`library_page_title.page`, `next_up`) ran up to 1.6× slower in it, so read
the Hiqlite deltas as orders of magnitude, not percentages.

| Call | SQLite before | SQLite after | Hiqlite before (rerun) | Hiqlite after | Change |
|---|---:|---:|---:|---:|---|
| `recently_added_all` (per call) | 164.302 | 17.087 (5 passes) | 175.877 | 21.206 (5 passes) | M4 widening window |
| `recently_added_shows` (per call) | 97.708 | 15.617 (5 passes) | 113.376 | 23.011 (5 passes) | M4 widening window |
| `watch_map` | 15.636 | 0.016 | 20.537 | 0.036 | `668c01d3f` id-driven `CROSS JOIN` |
| `search` | 1.008 | 0.593 | 0.994 | 1.195 | M3 per-hit `classification_fts` lookup (plan change below; the Hiqlite timing is within that run's load noise) |
| `authenticate_token` | 0.009 | 0.011 | 0.012 | 0.013 | M2 moves it off the writer; the statement is unchanged |

The Hiqlite `recently_added` rows were captured from the Authority read
before `ea62fa80d` added the Recordings predicate to it; the SQLite rows
carry that predicate (the standalone store always had it). The predicate is
a `NOT EXISTS` on `libraries` by primary key per candidate row.

## M7 — read-pool size (plan §3.3)

`query_plans bench fixture.db <reads>` (`27f729521`, release build): the
Home request shape (token authentication, `home_preview_pages(24)`, the
page's `watch_map`, `watch_rollups` for the shows on it,
`continue_watching(24)`, `next_up(24)`) 256 times across 32 concurrent
workers, users 1-5, while a writer upserts 50 files a second through the
same store. One process per configuration, warmed by one untimed Home first;
two rounds, interleaved (2, 4, 8, 2, 4, 8), on nuc3 at load average 3.6-4.6.

| Read connections | Homes | Read p50 (ms) | Read p95 (ms) | Wall (s) | Writes | Write p50 (ms) | Write p99 (ms) | Peak RSS (MiB) |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 2 | 256 | 971.1 | 1432.9 | 8.2 | 409 | 1.1 | 3.1 | 188 |
| 4 | 256 | 930.2 | 1377.6 | 7.8 | 392 | 1.2 | 3.4 | 204 |
| 8 | 256 | 1066.0 | 1754.0 | 9.1 | 457 | 1.3 | 4.3 | 205 |
| 2 | 256 | 1270.5 | 1727.8 | 9.9 | 499 | 1.0 | 2.9 | 171 |
| 4 | 256 | 852.8 | 1421.9 | 7.3 | 368 | 1.2 | 3.7 | 203 |
| 8 | 256 | 1032.1 | 1805.6 | 8.8 | 442 | 1.3 | 4.3 | 195 |

**Decision: `READ_CONNS` stays 2.** The rule is the smallest value whose
read p95 improves by at least 20 % without raising write p99 or RSS. Four
connections improved p95 by 3.9 % and 17.7 % in the two rounds (below the
threshold) while raising write p99 (3.1/2.9 → 3.4/3.7 ms) and RSS
(+16/+32 MiB); eight made p95 worse (+22 %/+4.5 %) and write p99 worse
(4.3 ms both rounds). No `storage.sqlite_read_conns` key is added.

Observation, not investigated: wall time did not fall with more read
connections (7.3-9.9 s for 256 Homes in every configuration), so under this
load the Home shape is not queueing on the pool; the per-statement warm
medians above sum to about 41 ms of connection time per Home, and two
connections would already allow roughly 49 Homes a second against the
observed 26-35.

## §3.10 — `probe_json` out of `files` (not adopted)

`files_for_item` is 0.02 ms warm and 1.0-1.2 ms cold on the fixture (each
file row carries 5-30 KiB of probe JSON), and `item_media_facts` 0.26 ms
warm for a 50-item page. Neither is on the Home or library-page critical
path at a size that a schema split would repay, so the column stays.

## Statistics trial (plan §3.7, not adopted)

`PRAGMA analysis_limit = 400; ANALYZE;` on a copy of the fixture, then the
same `measure` run. `sqlite_stat1` afterwards:

```text
items|idx_items_book_work|0 0
items|idx_items_missing_artwork|75600 401
items|idx_items_added|75600 1
items|idx_items_parent|75600 23
items|idx_items_library_kind|75600 401 401
files|idx_files_item|100000 2
files|sqlite_autoindex_files_1|100000 1
watch_state|idx_watch_updated|35000 401 1
watch_state|sqlite_autoindex_watch_state_1|35000 401 1
```

No statement improved beyond run-to-run noise; `watch_rollups` became 3.5×
slower (2.271 → 7.992 ms warm): with statistics the planner scans every item
through `idx_items_library_kind` and joins the show trees through an
automatic index, instead of driving from the 24 trees (Appendix E). Neither `PRAGMA optimize` after scans nor an
`ANALYZE` batch ships, and open question 2 (a replicated `ANALYZE`) does not
arise.

## Appendix A — SQLite, before (main@0e2c3fd47 plus the M0 tool)


| Call | Statement | Rows | Cold median | Warm median |
|---|---|---:|---:|---:|
| `library_page_title` | `list_top_items_in_genre.count` | 1 | 2.99 ms | 1.633 ms |
| `library_page_title` | `list_top_items_in_genre.page` | 50 | 11.50 ms | 9.493 ms |
| `library_page_title_genre` | `list_top_items_in_genre.count` | 1 | 7.35 ms | 5.186 ms |
| `library_page_title_genre` | `list_top_items_in_genre.page` | 0 | 10.20 ms | 8.247 ms |
| `library_page_added` | `list_top_items_in_genre.count` | 1 | 3.01 ms | 1.660 ms |
| `library_page_added` | `list_top_items_in_genre.page` | 50 | 24.59 ms | 21.366 ms |
| `library_page_year` | `list_top_items_in_genre.count` | 1 | 3.06 ms | 1.760 ms |
| `library_page_year` | `list_top_items_in_genre.page` | 50 | 26.45 ms | 23.598 ms |
| `library_page_resolution` | `list_top_items_in_genre.count` | 1 | 2.90 ms | 1.615 ms |
| `library_page_resolution` | `list_top_items_in_genre.page` | 50 | 43.30 ms | 42.504 ms |
| `shows_page_title` | `list_top_items_in_genre.count` | 1 | 8.52 ms | 6.688 ms |
| `shows_page_title` | `list_top_items_in_genre.page` | 50 | 9.79 ms | 7.234 ms |
| `home_page_recorded` | `list_top_items_in_genre.count` | 1 | 1.62 ms | 0.340 ms |
| `home_page_recorded` | `list_top_items_in_genre.page` | 50 | 1.82 ms | 0.482 ms |
| `home_preview_pages` | `home_preview_pages` | 96 | 30.50 ms | 29.459 ms |
| `recently_added_all` | `recently_added` | 24 | 169.60 ms | 164.302 ms |
| `recently_added_shows` | `recently_added` | 24 | 109.49 ms | 97.708 ms |
| `search` | `search_items` | 10 | 2.21 ms | 1.008 ms |
| `item_by_external_id` | `item_by_external_id` | 1 | 4.51 ms | 3.297 ms |
| `files_for_item` | `files_for_item` | 2 | 1.03 ms | 0.019 ms |
| `item_media_facts` | `item_media_facts` | 50 | 1.35 ms | 0.257 ms |
| `watch_map` | `watch_map` | 5 | 16.82 ms | 15.636 ms |
| `watch_rollups` | `watch_rollups` | 24 | 3.64 ms | 2.271 ms |
| `continue_watching` | `continue_watching` | 24 | 1.08 ms | 0.077 ms |
| `next_up` | `next_up` | 0 | 9.25 ms | 8.552 ms |
| `authenticate_token` | `authenticate_token` | 1 | 1.03 ms | 0.009 ms |

#### `library_page_title` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_title` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?4 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE))
                 ORDER BY sort_title ASC, id ASC LIMIT ?3 OFFSET ?2
```

Bound: `[1,10000,50,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_title_genre` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[1,"Drama"]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_title_genre` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?4 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE))
                 ORDER BY sort_title ASC, id ASC LIMIT ?3 OFFSET ?2
```

Bound: `[1,10000,50,"Drama"]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_added` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_added` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?4 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE))
                 ORDER BY added_at DESC, id DESC LIMIT ?3 OFFSET ?2
```

Bound: `[1,10000,50,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_year` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_year` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?4 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE))
                 ORDER BY year IS NULL, year DESC, sort_title ASC, id ASC LIMIT ?3 OFFSET ?2
```

Bound: `[1,10000,50,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_resolution` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_resolution` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?4 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE))
                 ORDER BY CASE WHEN kind IN ('movie','video') THEN COALESCE((SELECT MAX(f.height) FROM files f WHERE f.item_id = items.id), -1) ELSE -1 END DESC, sort_title ASC, id ASC LIMIT ?3 OFFSET ?2
```

Bound: `[1,10000,50,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
CORRELATED SCALAR SUBQUERY 2
  SEARCH f USING INDEX idx_files_item (item_id=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `shows_page_title` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[2,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `shows_page_title` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?4 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE))
                 ORDER BY sort_title ASC, id ASC LIMIT ?3 OFFSET ?2
```

Bound: `[2,200,50,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `home_page_recorded` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[3,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `home_page_recorded` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?4 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE))
                 ORDER BY (recorded_at IS NULL), recorded_at DESC, sort_title ASC, id ASC LIMIT ?3 OFFSET ?2
```

Bound: `[3,0,50,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `home_preview_pages` — `home_preview_pages`

```sql
WITH ranked AS (
                     SELECT id, library_id,
                            COUNT(*) OVER (PARTITION BY library_id) AS library_total,
                            ROW_NUMBER() OVER (
                                PARTITION BY library_id
                                ORDER BY added_at DESC, id DESC
                            ) AS preview_rank
                       FROM items
                      WHERE (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL))
                 ), selected AS (
                     SELECT id, library_id, library_total, preview_rank
                       FROM ranked
                      WHERE preview_rank <= ?1
                 )
                 SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, selected.library_total
                   FROM selected
                   JOIN items i ON i.id = selected.id
                  ORDER BY selected.library_id, selected.preview_rank
```

Bound: `[24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-4)
    CO-ROUTINE (subquery-5)
      SCAN items USING INDEX idx_items_library_kind
      USE TEMP B-TREE FOR LAST 2 TERMS OF ORDER BY
    SCAN (subquery-5)
  SCAN (subquery-4)
SCAN ranked
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_all` — `recently_added`

```sql
WITH ranked AS (
                     SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source,
                            show.title AS rail_show_title,
                            season.poster_path AS rail_season_poster,
                            ROW_NUMBER() OVER (
                                PARTITION BY
                                    CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL
                                         THEN 'show:' || show.id
                                         ELSE 'item:' || i.id END
                                ORDER BY i.added_at DESC,
                                         COALESCE(season.season_number, -1) DESC,
                                         COALESCE(i.episode_number, -1) DESC,
                                         i.id DESC
                            ) AS rail_rank
                     FROM items i
                     LEFT JOIN items season
                            ON season.id = i.parent_id AND i.kind = 'episode'
                     LEFT JOIN items show ON show.id = season.parent_id
                     WHERE i.kind IN ('movie','episode','video','folder','book','audiobook')
                       AND (?1 IS NULL OR i.library_id = ?1)
                       AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings'))
                 )
                 SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster
                 FROM ranked r
                 WHERE r.rail_rank = 1
                 ORDER BY r.added_at DESC, r.id DESC
                 LIMIT ?2
```

Bound: `[null,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-4)
    SCAN i
    CORRELATED SCALAR SUBQUERY 1
      SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-4)
SCAN r
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_shows` — `recently_added`

```sql
WITH ranked AS (
                     SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source,
                            show.title AS rail_show_title,
                            season.poster_path AS rail_season_poster,
                            ROW_NUMBER() OVER (
                                PARTITION BY
                                    CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL
                                         THEN 'show:' || show.id
                                         ELSE 'item:' || i.id END
                                ORDER BY i.added_at DESC,
                                         COALESCE(season.season_number, -1) DESC,
                                         COALESCE(i.episode_number, -1) DESC,
                                         i.id DESC
                            ) AS rail_rank
                     FROM items i
                     LEFT JOIN items season
                            ON season.id = i.parent_id AND i.kind = 'episode'
                     LEFT JOIN items show ON show.id = season.parent_id
                     WHERE i.kind IN ('movie','episode','video','folder','book','audiobook')
                       AND (?1 IS NULL OR i.library_id = ?1)
                       AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings'))
                 )
                 SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster
                 FROM ranked r
                 WHERE r.rail_rank = 1
                 ORDER BY r.added_at DESC, r.id DESC
                 LIMIT ?2
```

Bound: `[2,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-4)
    SCAN i
    CORRELATED SCALAR SUBQUERY 1
      SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-4)
SCAN r
USE TEMP B-TREE FOR ORDER BY
```

#### `search` — `search_items`

```sql
WITH hits AS MATERIALIZED (SELECT rowid,rank AS score FROM items_fts WHERE items_fts MATCH ?1 AND rowid NOT IN (SELECT rowid FROM classification_fts) UNION ALL SELECT rowid,rank AS score FROM classification_fts WHERE classification_fts MATCH ?1) SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title, season.poster_path
                 FROM (SELECT rowid,min(score) AS score FROM hits GROUP BY rowid) f
                 JOIN items i ON i.id = f.rowid
                 LEFT JOIN items season
                        ON season.id = i.parent_id AND i.kind = 'episode'
                 LEFT JOIN items show ON show.id = season.parent_id
                 WHERE i.kind IN ('movie','show','episode','folder','video','photo','book','audiobook')
                 ORDER BY f.score, i.id LIMIT ?2
```

Bound: `["(\"movie\") AND (\"0123\"*)",50]`

```text
QUERY PLAN
CO-ROUTINE f
  MATERIALIZE hits
    COMPOUND QUERY
      LEFT-MOST SUBQUERY
        SCAN items_fts VIRTUAL TABLE INDEX 0:M3
        LIST SUBQUERY 1
          SCAN classification_fts VIRTUAL TABLE INDEX 0:
          CREATE BLOOM FILTER
      UNION ALL
        SCAN classification_fts VIRTUAL TABLE INDEX 0:M1
  SCAN hits
  USE TEMP B-TREE FOR GROUP BY
SCAN f
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
USE TEMP B-TREE FOR ORDER BY
```

#### `item_by_external_id` — `item_by_external_id`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE kind = ?1
                   AND ((?2 IS NOT NULL AND tmdb_id = ?2)
                     OR (?3 IS NOT NULL AND imdb_id = ?3 COLLATE NOCASE))
                 ORDER BY (?2 IS NOT NULL AND tmdb_id = ?2) DESC, id
                 LIMIT 1
```

Bound: `["movie",1012345,null]`

```text
QUERY PLAN
SCAN items USING INDEX idx_items_library_kind
USE TEMP B-TREE FOR ORDER BY
```

#### `files_for_item` — `files_for_item`

```sql
SELECT id, item_id, path, size, mtime, duration_ms, container, video_codec, video_profile, width, height, bit_depth, hdr, bitrate, audio_streams, subtitle_streams, scanned_at, hdr_format, audio_offset_ms, (probe_json IS NOT NULL), dv_profile, dv_level, dv_bl_compat_id, dv_el_present, dv_rpu_present, video_codec_tag, field_order, max_cll, max_fall, mastering_max_luminance, luminance_source, downloaded_subtitles FROM files WHERE item_id = ?1
                 ORDER BY height DESC, bitrate DESC, path
```

Bound: `[12345]`

```text
QUERY PLAN
SEARCH files USING INDEX idx_files_item (item_id=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `item_media_facts` — `item_media_facts`

```sql
WITH ranked AS (
                     SELECT item_id,
                            COUNT(*)  OVER (PARTITION BY item_id) AS n_files,
                            SUM(size) OVER (PARTITION BY item_id) AS total_bytes,
                            ROW_NUMBER() OVER (
                                PARTITION BY item_id
                                ORDER BY COALESCE(height, 0) DESC,
                                         COALESCE(bitrate, 0) DESC,
                                         size DESC,
                                         id ASC) AS pick,
                            container, video_codec, height, hdr, hdr_format,
                            audio_streams
                     FROM files
                     WHERE item_id IN (1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50)
                 )
                 SELECT item_id, n_files, total_bytes, container, video_codec,
                        height, hdr, hdr_format, audio_streams
                 FROM ranked WHERE pick = 1
```

Bound: `[]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-3)
    CO-ROUTINE (subquery-4)
      SEARCH files USING INDEX idx_files_item (item_id=?)
      USE TEMP B-TREE FOR LAST 4 TERMS OF ORDER BY
    SCAN (subquery-4)
  SCAN (subquery-3)
SCAN ranked
```

#### `watch_map` — `watch_map`

```sql
SELECT w.item_id, w.position_ms, w.duration_ms, w.watched, w.updated_at
                 FROM watch_state w
                 JOIN json_each(?2) j ON j.value = w.item_id
                 WHERE w.user_id = ?1
```

Bound: `[1,"[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50]"]`

```text
QUERY PLAN
SEARCH w USING INDEX idx_watch_updated (user_id=?)
SCAN j VIRTUAL TABLE INDEX 1:
```

#### `watch_rollups` — `watch_rollups`

```sql
WITH RECURSIVE tree(root, id) AS (
                     SELECT id, id FROM items WHERE id IN (20001,20106,20211,20316,20421,20526,20631,20736,20841,20946,21051,21156,21261,21366,21471,21576,21681,21786,21891,21996,22101,22206,22311,22416)
                     UNION
                     SELECT t.root, i.id FROM items i JOIN tree t ON i.parent_id = t.id
                 )
                 SELECT t.root, COUNT(*), COALESCE(SUM(w.watched), 0)
                 FROM tree t
                 JOIN items i ON i.id = t.id
                 LEFT JOIN watch_state w ON w.item_id = i.id AND w.user_id = ?1
                 WHERE i.kind IN ('movie','episode','video','audiobook')
                 GROUP BY t.root
```

Bound: `[1]`

```text
QUERY PLAN
CO-ROUTINE tree
  SETUP
    SEARCH items USING INTEGER PRIMARY KEY (rowid=?)
  RECURSIVE STEP
    SCAN t
    SEARCH i USING COVERING INDEX idx_items_parent (parent_id=?)
SCAN t
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
SEARCH w USING INDEX sqlite_autoindex_watch_state_1 (user_id=? AND item_id=?) LEFT-JOIN
USE TEMP B-TREE FOR GROUP BY
```

#### `continue_watching` — `continue_watching`

```sql
SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title,
                        w.position_ms, w.duration_ms, w.watched, w.updated_at,
                        season.poster_path
                 FROM watch_state w
                 JOIN items i ON i.id = w.item_id
                 LEFT JOIN items season
                        ON season.id = i.parent_id AND i.kind = 'episode'
                 LEFT JOIN items show ON show.id = season.parent_id
                 WHERE w.user_id = ?1 AND w.watched = 0 AND w.position_ms > 0
                   AND i.kind IN ('movie','episode','video','audiobook')
                 ORDER BY w.updated_at DESC LIMIT ?2
```

Bound: `[1,24]`

```text
QUERY PLAN
SEARCH w USING INDEX idx_watch_updated (user_id=?)
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
```

#### `next_up` — `next_up`

```sql
SELECT e.id, e.library_id, e.kind, e.parent_id, e.title, e.sort_title, e.year, e.overview, e.tmdb_id, e.imdb_id, e.season_number, e.episode_number, e.air_date, e.runtime_ms, e.poster_path, e.backdrop_path, e.added_at, e.updated_at, e.recorded_at, e.tags, e.nfo_seeded_at, e.artwork_attempted_at, e.artwork_error, e.genres, e.author, e.book_work_id, e.book_edition_id, e.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, MIN(season.season_number*100000 + e.episode_number) AS ord FROM items e JOIN items season ON season.id = e.parent_id JOIN items show ON show.id = season.parent_id WHERE e.kind = 'episode' AND e.id NOT IN (SELECT item_id FROM watch_state WHERE user_id = ?1 AND (watched = 1 OR position_ms > 0)) AND (season.season_number*100000 + e.episode_number) > ( SELECT COALESCE(MAX(se.season_number*100000 + ep.episode_number), -1) FROM watch_state w JOIN items ep ON ep.id = w.item_id AND ep.kind = 'episode' JOIN items se ON se.id = ep.parent_id WHERE w.user_id = ?1 AND w.watched = 1 AND se.parent_id = show.id) AND show.id IN (SELECT sh.id FROM watch_state w JOIN items ep ON ep.id = w.item_id AND ep.kind = 'episode' JOIN items se ON se.id = ep.parent_id JOIN items sh ON sh.id = se.parent_id WHERE w.user_id = ?1 AND w.watched = 1) AND show.id NOT IN (SELECT sh.id FROM watch_state w JOIN items ep ON ep.id = w.item_id AND ep.kind = 'episode' JOIN items se ON se.id = ep.parent_id JOIN items sh ON sh.id = se.parent_id WHERE w.user_id = ?1 AND w.watched = 0 AND w.position_ms > 0) GROUP BY show.id ORDER BY show.sort_title LIMIT ?2
```

Bound: `[1,24]`

```text
QUERY PLAN
SEARCH show USING INTEGER PRIMARY KEY (rowid=?)
LIST SUBQUERY 3
  SEARCH w USING INDEX idx_watch_updated (user_id=?)
  SEARCH ep USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH se USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH sh USING INTEGER PRIMARY KEY (rowid=?)
LIST SUBQUERY 4
  SEARCH w USING INDEX idx_watch_updated (user_id=?)
  SEARCH ep USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH se USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH sh USING INTEGER PRIMARY KEY (rowid=?)
  CREATE BLOOM FILTER
SEARCH season USING INDEX idx_items_parent (parent_id=?)
SEARCH e USING INDEX idx_items_parent (parent_id=?)
LIST SUBQUERY 1
  SEARCH watch_state USING INDEX idx_watch_updated (user_id=?)
  CREATE BLOOM FILTER
CORRELATED SCALAR SUBQUERY 2
  SEARCH w USING INDEX idx_watch_updated (user_id=?)
  SEARCH ep USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH se USING INTEGER PRIMARY KEY (rowid=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `authenticate_token` — `authenticate_token`

```sql
SELECT u.id, u.username, u.password_hash, u.is_admin, u.created_at,
                            t.last_seen_at,
                            (SELECT value FROM settings WHERE key = ?2),
                            (SELECT value FROM settings WHERE key = ?3),
                            (SELECT value FROM settings WHERE key = ?4),
                            unixepoch()
                     FROM users u
                     JOIN tokens t ON t.user_id = u.id
                     WHERE t.token_hash = ?1
```

Bound: `["fixture-token-1","auth.token_expiry_enabled","auth.token_idle_days","auth.token_expiry_since"]`

```text
QUERY PLAN
SEARCH t USING INDEX sqlite_autoindex_tokens_1 (token_hash=?)
SEARCH u USING INTEGER PRIMARY KEY (rowid=?)
SCALAR SUBQUERY 1
  SEARCH settings USING INDEX sqlite_autoindex_settings_1 (key=?)
SCALAR SUBQUERY 2
  SEARCH settings USING INDEX sqlite_autoindex_settings_1 (key=?)
SCALAR SUBQUERY 3
  SEARCH settings USING INDEX sqlite_autoindex_settings_1 (key=?)
```


## Appendix B — Hiqlite schema, before


| Call | Statement | Rows | Cold median | Warm median |
|---|---|---:|---:|---:|
| `library_page_title` | `list_top_items_in_genre.count` | 1 | 3.91 ms | 1.680 ms |
| `library_page_title` | `list_top_items_in_genre.page` | 50 | 11.96 ms | 9.547 ms |
| `library_page_title_genre` | `list_top_items_in_genre.count` | 1 | 7.61 ms | 5.715 ms |
| `library_page_title_genre` | `list_top_items_in_genre.page` | 0 | 10.83 ms | 8.744 ms |
| `library_page_added` | `list_top_items_in_genre.count` | 1 | 3.10 ms | 1.759 ms |
| `library_page_added` | `list_top_items_in_genre.page` | 50 | 24.74 ms | 21.642 ms |
| `library_page_year` | `list_top_items_in_genre.count` | 1 | 3.02 ms | 1.666 ms |
| `library_page_year` | `list_top_items_in_genre.page` | 50 | 26.33 ms | 23.990 ms |
| `library_page_resolution` | `list_top_items_in_genre.count` | 1 | 3.09 ms | 1.621 ms |
| `library_page_resolution` | `list_top_items_in_genre.page` | 50 | 47.14 ms | 45.814 ms |
| `shows_page_title` | `list_top_items_in_genre.count` | 1 | 8.45 ms | 7.148 ms |
| `shows_page_title` | `list_top_items_in_genre.page` | 50 | 9.33 ms | 7.796 ms |
| `home_page_recorded` | `list_top_items_in_genre.count` | 1 | 1.69 ms | 0.363 ms |
| `home_page_recorded` | `list_top_items_in_genre.page` | 50 | 1.88 ms | 0.508 ms |
| `home_preview_pages` | `home_preview_pages` | 96 | 31.09 ms | 29.783 ms |
| `recently_added_all` | `recently_added` | 24 | 166.46 ms | 165.696 ms |
| `recently_added_shows` | `recently_added` | 24 | 113.11 ms | 111.838 ms |
| `search` | `search_items` | 10 | 2.65 ms | 0.991 ms |
| `item_by_external_id` | `item_by_external_id` | 1 | 5.65 ms | 4.274 ms |
| `files_for_item` | `files_for_item` | 2 | 1.29 ms | 0.024 ms |
| `item_media_facts` | `item_media_facts` | 50 | 1.66 ms | 0.269 ms |
| `watch_map` | `watch_map` | 5 | 20.60 ms | 19.277 ms |
| `watch_rollups` | `watch_rollups` | 24 | 4.24 ms | 2.745 ms |
| `continue_watching` | `continue_watching` | 24 | 1.40 ms | 0.096 ms |
| `next_up` | `next_up` | 0 | 11.41 ms | 9.952 ms |
| `authenticate_token` | `authenticate_token` | 1 | 1.25 ms | 0.011 ms |

#### `library_page_title` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_title` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE)) ORDER BY sort_title ASC, id ASC LIMIT $3 OFFSET $4
```

Bound: `[1,null,50,10000]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_title_genre` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[1,"Drama"]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_title_genre` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE)) ORDER BY sort_title ASC, id ASC LIMIT $3 OFFSET $4
```

Bound: `[1,"Drama",50,10000]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_added` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_added` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE)) ORDER BY added_at DESC, id DESC LIMIT $3 OFFSET $4
```

Bound: `[1,null,50,10000]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_year` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_year` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE)) ORDER BY year IS NULL, year DESC, sort_title ASC, id ASC LIMIT $3 OFFSET $4
```

Bound: `[1,null,50,10000]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_resolution` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_resolution` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE)) ORDER BY CASE WHEN kind IN ('movie','video') THEN COALESCE((SELECT MAX(f.height) FROM files f WHERE f.item_id = items.id), -1) ELSE -1 END DESC, sort_title ASC, id ASC LIMIT $3 OFFSET $4
```

Bound: `[1,null,50,10000]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
CORRELATED SCALAR SUBQUERY 2
  SEARCH f USING INDEX idx_files_item (item_id=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `shows_page_title` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[2,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `shows_page_title` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE)) ORDER BY sort_title ASC, id ASC LIMIT $3 OFFSET $4
```

Bound: `[2,null,50,200]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `home_page_recorded` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[3,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `home_page_recorded` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE)) ORDER BY (recorded_at IS NULL), recorded_at DESC, sort_title ASC, id ASC LIMIT $3 OFFSET $4
```

Bound: `[3,null,50,0]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `home_preview_pages` — `home_preview_pages`

```sql
WITH ranked AS (
                 SELECT id, library_id,
                        COUNT(*) OVER (PARTITION BY library_id) AS library_total,
                        ROW_NUMBER() OVER (
                            PARTITION BY library_id
                            ORDER BY added_at DESC, id DESC
                        ) AS preview_rank
                   FROM items
                  WHERE (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL))
             ), selected AS (
                 SELECT id, library_id, library_total, preview_rank
                   FROM ranked
                  WHERE preview_rank <= $1
             )
             SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, selected.library_total
               FROM selected
               JOIN items i ON i.id = selected.id
              ORDER BY selected.library_id, selected.preview_rank
```

Bound: `[24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-4)
    CO-ROUTINE (subquery-5)
      SCAN items USING INDEX idx_items_library_kind
      USE TEMP B-TREE FOR LAST 2 TERMS OF ORDER BY
    SCAN (subquery-5)
  SCAN (subquery-4)
SCAN ranked
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_all` — `recently_added`

```sql
WITH ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT $2
```

Bound: `[null,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-3)
    SCAN i
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-3)
SCAN r
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_shows` — `recently_added`

```sql
WITH ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT $2
```

Bound: `[2,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-3)
    SCAN i
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-3)
SCAN r
USE TEMP B-TREE FOR ORDER BY
```

#### `search` — `search_items`

```sql
WITH hits AS MATERIALIZED (SELECT rowid,rank AS score FROM items_fts WHERE items_fts MATCH $1 AND rowid NOT IN (SELECT rowid FROM classification_fts) UNION ALL SELECT rowid,rank AS score FROM classification_fts WHERE classification_fts MATCH $1) SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster FROM (SELECT rowid,min(score) AS score FROM hits GROUP BY rowid) f JOIN items i ON i.id = f.rowid LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','show','episode','folder','video','photo','book','audiobook') ORDER BY f.score, i.id LIMIT $2
```

Bound: `["(\"movie\") AND (\"0123\"*)",50]`

```text
QUERY PLAN
CO-ROUTINE f
  MATERIALIZE hits
    COMPOUND QUERY
      LEFT-MOST SUBQUERY
        SCAN items_fts VIRTUAL TABLE INDEX 0:M3
        LIST SUBQUERY 1
          SCAN classification_fts VIRTUAL TABLE INDEX 0:
          CREATE BLOOM FILTER
      UNION ALL
        SCAN classification_fts VIRTUAL TABLE INDEX 0:M1
  SCAN hits
  USE TEMP B-TREE FOR GROUP BY
SCAN f
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
USE TEMP B-TREE FOR ORDER BY
```

#### `item_by_external_id` — `item_by_external_id`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE kind = $1 AND (($2 IS NOT NULL AND tmdb_id = $2) OR ($3 IS NOT NULL AND imdb_id = $3 COLLATE NOCASE)) ORDER BY ($2 IS NOT NULL AND tmdb_id = $2) DESC, id LIMIT 1
```

Bound: `["movie",1012345,null]`

```text
QUERY PLAN
SCAN items USING INDEX idx_items_library_kind
USE TEMP B-TREE FOR ORDER BY
```

#### `files_for_item` — `files_for_item`

```sql
SELECT id, item_id, path, size, mtime, duration_ms, container, video_codec, video_profile, width, height, bit_depth, hdr, bitrate, audio_streams, subtitle_streams, scanned_at, hdr_format, audio_offset_ms, dv_profile, dv_level, dv_bl_compat_id, dv_el_present, dv_rpu_present, (probe_json IS NOT NULL) AS probed, video_codec_tag, field_order, max_cll, max_fall, mastering_max_luminance, luminance_source, downloaded_subtitles FROM files WHERE item_id = $1 ORDER BY height DESC, bitrate DESC, path
```

Bound: `[12345]`

```text
QUERY PLAN
SEARCH files USING INDEX idx_files_item (item_id=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `item_media_facts` — `item_media_facts`

```sql
WITH ranked AS ( SELECT item_id, COUNT(*) OVER (PARTITION BY item_id) AS files, SUM(size) OVER (PARTITION BY item_id) AS bytes, ROW_NUMBER() OVER (PARTITION BY item_id ORDER BY COALESCE(height, 0) DESC, COALESCE(bitrate, 0) DESC, size DESC, id ASC) AS pick, container, video_codec, height, hdr, hdr_format, audio_streams FROM files WHERE item_id IN (SELECT value FROM json_each($1)) ) SELECT item_id, files, bytes, container, video_codec, height, hdr, hdr_format, audio_streams FROM ranked WHERE pick = 1
```

Bound: `["[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50]"]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-4)
    CO-ROUTINE (subquery-5)
      SEARCH files USING INDEX idx_files_item (item_id=?)
      LIST SUBQUERY 1
        SCAN json_each VIRTUAL TABLE INDEX 1:
      USE TEMP B-TREE FOR LAST 4 TERMS OF ORDER BY
    SCAN (subquery-5)
  SCAN (subquery-4)
SCAN ranked
```

#### `watch_map` — `watch_map`

```sql
SELECT w.item_id, w.position_ms, w.duration_ms, w.watched, w.updated_at FROM watch_state w JOIN json_each($1) j ON j.value = w.item_id WHERE w.user_id = $2
```

Bound: `["[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50]",1]`

```text
QUERY PLAN
SEARCH w USING INDEX idx_watch_updated (user_id=?)
SCAN j VIRTUAL TABLE INDEX 1:
```

#### `watch_rollups` — `watch_rollups`

```sql
WITH RECURSIVE tree(root, id) AS ( SELECT id, id FROM items WHERE id IN (SELECT value FROM json_each($1)) UNION SELECT t.root, i.id FROM items i JOIN tree t ON i.parent_id = t.id ) SELECT t.root AS root, COUNT(*) AS leaves, COALESCE(SUM(w.watched), 0) AS watched FROM tree t JOIN items i ON i.id = t.id LEFT JOIN watch_state w ON w.item_id = i.id AND w.user_id = $2 WHERE i.kind IN ('movie','episode','video','audiobook') GROUP BY t.root
```

Bound: `["[20001,20106,20211,20316,20421,20526,20631,20736,20841,20946,21051,21156,21261,21366,21471,21576,21681,21786,21891,21996,22101,22206,22311,22416]",1]`

```text
QUERY PLAN
CO-ROUTINE tree
  SETUP
    SEARCH items USING INTEGER PRIMARY KEY (rowid=?)
    LIST SUBQUERY 1
      SCAN json_each VIRTUAL TABLE INDEX 1:
  RECURSIVE STEP
    SCAN t
    SEARCH i USING COVERING INDEX idx_items_parent (parent_id=?)
SCAN t
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
SEARCH w USING INDEX sqlite_autoindex_watch_state_1 (user_id=? AND item_id=?) LEFT-JOIN
USE TEMP B-TREE FOR GROUP BY
```

#### `continue_watching` — `continue_watching`

```sql
SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, w.position_ms AS watch_position_ms, w.duration_ms AS watch_duration_ms, w.watched AS watch_watched, w.updated_at AS watch_updated_at FROM watch_state w JOIN items i ON i.id = w.item_id LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE w.user_id = $1 AND w.watched = 0 AND w.position_ms > 0 AND i.kind IN ('movie','episode','video','audiobook') ORDER BY w.updated_at DESC LIMIT $2
```

Bound: `[1,24]`

```text
QUERY PLAN
SEARCH w USING INDEX idx_watch_updated (user_id=?)
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
```

#### `next_up` — `next_up`

```sql
SELECT e.id, e.library_id, e.kind, e.parent_id, e.title, e.sort_title, e.year, e.overview, e.tmdb_id, e.imdb_id, e.season_number, e.episode_number, e.air_date, e.runtime_ms, e.poster_path, e.backdrop_path, e.added_at, e.updated_at, e.recorded_at, e.tags, e.nfo_seeded_at, e.artwork_attempted_at, e.artwork_error, e.genres, e.author, e.book_work_id, e.book_edition_id, e.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, MIN(season.season_number*100000 + e.episode_number) AS ord FROM items e JOIN items season ON season.id = e.parent_id JOIN items show ON show.id = season.parent_id WHERE e.kind = 'episode' AND e.id NOT IN (SELECT item_id FROM watch_state WHERE user_id = $1 AND (watched = 1 OR position_ms > 0)) AND (season.season_number*100000 + e.episode_number) > ( SELECT COALESCE(MAX(se.season_number*100000 + ep.episode_number), -1) FROM watch_state w JOIN items ep ON ep.id = w.item_id AND ep.kind = 'episode' JOIN items se ON se.id = ep.parent_id WHERE w.user_id = $1 AND w.watched = 1 AND se.parent_id = show.id) AND show.id IN (SELECT sh.id FROM watch_state w JOIN items ep ON ep.id = w.item_id AND ep.kind = 'episode' JOIN items se ON se.id = ep.parent_id JOIN items sh ON sh.id = se.parent_id WHERE w.user_id = $1 AND w.watched = 1) AND show.id NOT IN (SELECT sh.id FROM watch_state w JOIN items ep ON ep.id = w.item_id AND ep.kind = 'episode' JOIN items se ON se.id = ep.parent_id JOIN items sh ON sh.id = se.parent_id WHERE w.user_id = $1 AND w.watched = 0 AND w.position_ms > 0) GROUP BY show.id ORDER BY show.sort_title LIMIT $2
```

Bound: `[1,24]`

```text
QUERY PLAN
SEARCH show USING INTEGER PRIMARY KEY (rowid=?)
LIST SUBQUERY 3
  SEARCH w USING INDEX idx_watch_updated (user_id=?)
  SEARCH ep USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH se USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH sh USING INTEGER PRIMARY KEY (rowid=?)
LIST SUBQUERY 4
  SEARCH w USING INDEX idx_watch_updated (user_id=?)
  SEARCH ep USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH se USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH sh USING INTEGER PRIMARY KEY (rowid=?)
  CREATE BLOOM FILTER
SEARCH season USING INDEX idx_items_parent (parent_id=?)
SEARCH e USING INDEX idx_items_parent (parent_id=?)
LIST SUBQUERY 1
  SEARCH watch_state USING INDEX idx_watch_updated (user_id=?)
  CREATE BLOOM FILTER
CORRELATED SCALAR SUBQUERY 2
  SEARCH w USING INDEX idx_watch_updated (user_id=?)
  SEARCH ep USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH se USING INTEGER PRIMARY KEY (rowid=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `authenticate_token` — `authenticate_token`

```sql
SELECT u.id, u.username, u.password_hash, u.is_admin, u.created_at, t.last_seen_at, (SELECT value FROM settings WHERE key = $1) AS expiry_enabled, (SELECT value FROM settings WHERE key = $2) AS expiry_idle_days, (SELECT value FROM settings WHERE key = $3) AS expiry_since FROM users u JOIN tokens t ON t.user_id = u.id WHERE t.token_hash = $4
```

Bound: `["auth.token_expiry_enabled","auth.token_idle_days","auth.token_expiry_since","fixture-token-1"]`

```text
QUERY PLAN
SEARCH t USING INDEX sqlite_autoindex_tokens_1 (token_hash=?)
SEARCH u USING INTEGER PRIMARY KEY (rowid=?)
SCALAR SUBQUERY 1
  SEARCH settings USING INDEX sqlite_autoindex_settings_1 (key=?)
SCALAR SUBQUERY 2
  SEARCH settings USING INDEX sqlite_autoindex_settings_1 (key=?)
SCALAR SUBQUERY 3
  SEARCH settings USING INDEX sqlite_autoindex_settings_1 (key=?)
```


## Appendix C — SQLite, after (this branch)


| Call | Statement | Rows | Cold median | Warm median |
|---|---|---:|---:|---:|
| `library_page_title` | `list_top_items_in_genre.count` | 1 | 3.91 ms | 2.247 ms |
| `library_page_title` | `list_top_items_in_genre.page` | 50 | 11.14 ms | 8.769 ms |
| `library_page_title_genre` | `list_top_items_in_genre.count` | 1 | 6.93 ms | 5.202 ms |
| `library_page_title_genre` | `list_top_items_in_genre.page` | 0 | 10.04 ms | 7.980 ms |
| `library_page_added` | `list_top_items_in_genre.count` | 1 | 2.85 ms | 1.647 ms |
| `library_page_added` | `list_top_items_in_genre.page` | 50 | 23.68 ms | 20.587 ms |
| `library_page_year` | `list_top_items_in_genre.count` | 1 | 2.87 ms | 1.551 ms |
| `library_page_year` | `list_top_items_in_genre.page` | 50 | 24.71 ms | 23.797 ms |
| `library_page_resolution` | `list_top_items_in_genre.count` | 1 | 3.03 ms | 1.563 ms |
| `library_page_resolution` | `list_top_items_in_genre.page` | 50 | 44.78 ms | 42.764 ms |
| `shows_page_title` | `list_top_items_in_genre.count` | 1 | 8.06 ms | 5.957 ms |
| `shows_page_title` | `list_top_items_in_genre.page` | 50 | 9.56 ms | 7.157 ms |
| `home_page_recorded` | `list_top_items_in_genre.count` | 1 | 1.58 ms | 0.338 ms |
| `home_page_recorded` | `list_top_items_in_genre.page` | 50 | 1.77 ms | 0.490 ms |
| `home_preview_pages` | `home_preview_pages` | 96 | 29.81 ms | 28.104 ms |
| `recently_added_all` | `recently_added` | 2 | 2.12 ms | 0.851 ms |
| `recently_added_all` | `recently_added` | 4 | 2.55 ms | 1.321 ms |
| `recently_added_all` | `recently_added` | 8 | 3.50 ms | 2.278 ms |
| `recently_added_all` | `recently_added` | 16 | 5.42 ms | 4.199 ms |
| `recently_added_all` | `recently_added` | 24 | 9.91 ms | 8.438 ms |
| `recently_added_shows` | `recently_added` | 2 | 2.03 ms | 0.795 ms |
| `recently_added_shows` | `recently_added` | 4 | 2.48 ms | 1.231 ms |
| `recently_added_shows` | `recently_added` | 8 | 3.40 ms | 2.119 ms |
| `recently_added_shows` | `recently_added` | 16 | 5.18 ms | 3.892 ms |
| `recently_added_shows` | `recently_added` | 24 | 9.05 ms | 7.580 ms |
| `search` | `search_items` | 10 | 1.96 ms | 0.593 ms |
| `item_by_external_id` | `item_by_external_id` | 1 | 5.12 ms | 3.813 ms |
| `files_for_item` | `files_for_item` | 2 | 1.20 ms | 0.023 ms |
| `item_media_facts` | `item_media_facts` | 50 | 1.49 ms | 0.260 ms |
| `watch_map` | `watch_map` | 5 | 1.14 ms | 0.016 ms |
| `watch_rollups` | `watch_rollups` | 24 | 3.95 ms | 2.686 ms |
| `continue_watching` | `continue_watching` | 24 | 1.36 ms | 0.094 ms |
| `next_up` | `next_up` | 0 | 10.56 ms | 9.295 ms |
| `authenticate_token` | `authenticate_token` | 1 | 1.13 ms | 0.011 ms |

Per call (sums of the statement medians above):

| Call | Statements | Cold | Warm |
|---|---:|---:|---:|
| `library_page_title` | 2 | 15.05 ms | 11.015 ms |
| `library_page_title_genre` | 2 | 16.97 ms | 13.182 ms |
| `library_page_added` | 2 | 26.53 ms | 22.234 ms |
| `library_page_year` | 2 | 27.59 ms | 25.348 ms |
| `library_page_resolution` | 2 | 47.81 ms | 44.327 ms |
| `shows_page_title` | 2 | 17.63 ms | 13.114 ms |
| `home_page_recorded` | 2 | 3.35 ms | 0.828 ms |
| `home_preview_pages` | 1 | 29.81 ms | 28.104 ms |
| `recently_added_all` | 5 | 23.50 ms | 17.087 ms |
| `recently_added_shows` | 5 | 22.16 ms | 15.617 ms |
| `search` | 1 | 1.96 ms | 0.593 ms |
| `item_by_external_id` | 1 | 5.12 ms | 3.813 ms |
| `files_for_item` | 1 | 1.20 ms | 0.023 ms |
| `item_media_facts` | 1 | 1.49 ms | 0.260 ms |
| `watch_map` | 1 | 1.14 ms | 0.016 ms |
| `watch_rollups` | 1 | 3.95 ms | 2.686 ms |
| `continue_watching` | 1 | 1.36 ms | 0.094 ms |
| `next_up` | 1 | 10.56 ms | 9.295 ms |
| `authenticate_token` | 1 | 1.13 ms | 0.011 ms |

#### `library_page_title` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_title` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?4 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE))
                 ORDER BY sort_title ASC, id ASC LIMIT ?3 OFFSET ?2
```

Bound: `[1,10000,50,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_title_genre` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[1,"Drama"]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_title_genre` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?4 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE))
                 ORDER BY sort_title ASC, id ASC LIMIT ?3 OFFSET ?2
```

Bound: `[1,10000,50,"Drama"]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_added` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_added` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?4 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE))
                 ORDER BY added_at DESC, id DESC LIMIT ?3 OFFSET ?2
```

Bound: `[1,10000,50,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_year` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_year` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?4 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE))
                 ORDER BY year IS NULL, year DESC, sort_title ASC, id ASC LIMIT ?3 OFFSET ?2
```

Bound: `[1,10000,50,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_resolution` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_resolution` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?4 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE))
                 ORDER BY CASE WHEN kind IN ('movie','video') THEN COALESCE((SELECT MAX(f.height) FROM files f WHERE f.item_id = items.id), -1) ELSE -1 END DESC, sort_title ASC, id ASC LIMIT ?3 OFFSET ?2
```

Bound: `[1,10000,50,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
CORRELATED SCALAR SUBQUERY 2
  SEARCH f USING INDEX idx_files_item (item_id=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `shows_page_title` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[2,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `shows_page_title` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?4 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE))
                 ORDER BY sort_title ASC, id ASC LIMIT ?3 OFFSET ?2
```

Bound: `[2,200,50,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `home_page_recorded` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[3,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `home_page_recorded` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?4 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE))
                 ORDER BY (recorded_at IS NULL), recorded_at DESC, sort_title ASC, id ASC LIMIT ?3 OFFSET ?2
```

Bound: `[3,0,50,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `home_preview_pages` — `home_preview_pages`

```sql
WITH ranked AS (
                     SELECT id, library_id,
                            COUNT(*) OVER (PARTITION BY library_id) AS library_total,
                            ROW_NUMBER() OVER (
                                PARTITION BY library_id
                                ORDER BY added_at DESC, id DESC
                            ) AS preview_rank
                       FROM items
                      WHERE (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL))
                 ), selected AS (
                     SELECT id, library_id, library_total, preview_rank
                       FROM ranked
                      WHERE preview_rank <= ?1
                 )
                 SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, selected.library_total
                   FROM selected
                   JOIN items i ON i.id = selected.id
                  ORDER BY selected.library_id, selected.preview_rank
```

Bound: `[24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-4)
    CO-ROUTINE (subquery-5)
      SCAN items USING INDEX idx_items_library_kind
      USE TEMP B-TREE FOR LAST 2 TERMS OF ORDER BY
    SCAN (subquery-5)
  SCAN (subquery-4)
SCAN ranked
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_all` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) ORDER BY i.added_at DESC LIMIT 1 OFFSET ?2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT ?3
```

Bound: `[null,191,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-8)
    SEARCH i USING INDEX idx_items_added (added_at>?)
    SCALAR SUBQUERY 4
      MATERIALIZE cut
        SCAN i USING INDEX idx_items_added
        CORRELATED SCALAR SUBQUERY 1
          SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
      SCAN cut
    CORRELATED SCALAR SUBQUERY 3
      SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-8)
SCAN r
SCALAR SUBQUERY 6
  SCAN cut
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_all` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) ORDER BY i.added_at DESC LIMIT 1 OFFSET ?2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT ?3
```

Bound: `[null,383,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-8)
    SEARCH i USING INDEX idx_items_added (added_at>?)
    SCALAR SUBQUERY 4
      MATERIALIZE cut
        SCAN i USING INDEX idx_items_added
        CORRELATED SCALAR SUBQUERY 1
          SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
      SCAN cut
    CORRELATED SCALAR SUBQUERY 3
      SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-8)
SCAN r
SCALAR SUBQUERY 6
  SCAN cut
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_all` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) ORDER BY i.added_at DESC LIMIT 1 OFFSET ?2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT ?3
```

Bound: `[null,767,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-8)
    SEARCH i USING INDEX idx_items_added (added_at>?)
    SCALAR SUBQUERY 4
      MATERIALIZE cut
        SCAN i USING INDEX idx_items_added
        CORRELATED SCALAR SUBQUERY 1
          SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
      SCAN cut
    CORRELATED SCALAR SUBQUERY 3
      SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-8)
SCAN r
SCALAR SUBQUERY 6
  SCAN cut
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_all` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) ORDER BY i.added_at DESC LIMIT 1 OFFSET ?2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT ?3
```

Bound: `[null,1535,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-8)
    SEARCH i USING INDEX idx_items_added (added_at>?)
    SCALAR SUBQUERY 4
      MATERIALIZE cut
        SCAN i USING INDEX idx_items_added
        CORRELATED SCALAR SUBQUERY 1
          SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
      SCAN cut
    CORRELATED SCALAR SUBQUERY 3
      SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-8)
SCAN r
SCALAR SUBQUERY 6
  SCAN cut
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_all` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) ORDER BY i.added_at DESC LIMIT 1 OFFSET ?2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT ?3
```

Bound: `[null,3071,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-8)
    SEARCH i USING INDEX idx_items_added (added_at>?)
    SCALAR SUBQUERY 4
      MATERIALIZE cut
        SCAN i USING INDEX idx_items_added
        CORRELATED SCALAR SUBQUERY 1
          SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
      SCAN cut
    CORRELATED SCALAR SUBQUERY 3
      SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-8)
SCAN r
SCALAR SUBQUERY 6
  SCAN cut
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_shows` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) ORDER BY i.added_at DESC LIMIT 1 OFFSET ?2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT ?3
```

Bound: `[2,191,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-8)
    SEARCH i USING INDEX idx_items_added (added_at>?)
    SCALAR SUBQUERY 4
      MATERIALIZE cut
        SCAN i USING INDEX idx_items_added
        CORRELATED SCALAR SUBQUERY 1
          SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
      SCAN cut
    CORRELATED SCALAR SUBQUERY 3
      SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-8)
SCAN r
SCALAR SUBQUERY 6
  SCAN cut
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_shows` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) ORDER BY i.added_at DESC LIMIT 1 OFFSET ?2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT ?3
```

Bound: `[2,383,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-8)
    SEARCH i USING INDEX idx_items_added (added_at>?)
    SCALAR SUBQUERY 4
      MATERIALIZE cut
        SCAN i USING INDEX idx_items_added
        CORRELATED SCALAR SUBQUERY 1
          SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
      SCAN cut
    CORRELATED SCALAR SUBQUERY 3
      SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-8)
SCAN r
SCALAR SUBQUERY 6
  SCAN cut
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_shows` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) ORDER BY i.added_at DESC LIMIT 1 OFFSET ?2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT ?3
```

Bound: `[2,767,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-8)
    SEARCH i USING INDEX idx_items_added (added_at>?)
    SCALAR SUBQUERY 4
      MATERIALIZE cut
        SCAN i USING INDEX idx_items_added
        CORRELATED SCALAR SUBQUERY 1
          SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
      SCAN cut
    CORRELATED SCALAR SUBQUERY 3
      SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-8)
SCAN r
SCALAR SUBQUERY 6
  SCAN cut
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_shows` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) ORDER BY i.added_at DESC LIMIT 1 OFFSET ?2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT ?3
```

Bound: `[2,1535,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-8)
    SEARCH i USING INDEX idx_items_added (added_at>?)
    SCALAR SUBQUERY 4
      MATERIALIZE cut
        SCAN i USING INDEX idx_items_added
        CORRELATED SCALAR SUBQUERY 1
          SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
      SCAN cut
    CORRELATED SCALAR SUBQUERY 3
      SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-8)
SCAN r
SCALAR SUBQUERY 6
  SCAN cut
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_shows` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) ORDER BY i.added_at DESC LIMIT 1 OFFSET ?2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT ?3
```

Bound: `[2,3071,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-8)
    SEARCH i USING INDEX idx_items_added (added_at>?)
    SCALAR SUBQUERY 4
      MATERIALIZE cut
        SCAN i USING INDEX idx_items_added
        CORRELATED SCALAR SUBQUERY 1
          SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
      SCAN cut
    CORRELATED SCALAR SUBQUERY 3
      SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-8)
SCAN r
SCALAR SUBQUERY 6
  SCAN cut
USE TEMP B-TREE FOR ORDER BY
```

#### `search` — `search_items`

```sql
WITH hits AS MATERIALIZED (SELECT rowid,rank AS score FROM items_fts WHERE items_fts MATCH ?1 AND NOT EXISTS (SELECT 1 FROM classification_fts c WHERE c.rowid = items_fts.rowid) UNION ALL SELECT rowid,rank AS score FROM classification_fts WHERE classification_fts MATCH ?1) SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title, season.poster_path
                 FROM (SELECT rowid,min(score) AS score FROM hits GROUP BY rowid) f
                 JOIN items i ON i.id = f.rowid
                 LEFT JOIN items season
                        ON season.id = i.parent_id AND i.kind = 'episode'
                 LEFT JOIN items show ON show.id = season.parent_id
                 WHERE i.kind IN ('movie','show','episode','folder','video','photo','book','audiobook')
                 ORDER BY f.score, i.id LIMIT ?2
```

Bound: `["(\"movie\") AND (\"0123\"*)",50]`

```text
QUERY PLAN
CO-ROUTINE f
  MATERIALIZE hits
    COMPOUND QUERY
      LEFT-MOST SUBQUERY
        SCAN items_fts VIRTUAL TABLE INDEX 0:M3
        CORRELATED SCALAR SUBQUERY 1
          SCAN c VIRTUAL TABLE INDEX 0:=
      UNION ALL
        SCAN classification_fts VIRTUAL TABLE INDEX 0:M1
  SCAN hits
  USE TEMP B-TREE FOR GROUP BY
SCAN f
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
USE TEMP B-TREE FOR ORDER BY
```

#### `item_by_external_id` — `item_by_external_id`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE kind = ?1
                   AND ((?2 IS NOT NULL AND tmdb_id = ?2)
                     OR (?3 IS NOT NULL AND imdb_id = ?3 COLLATE NOCASE))
                 ORDER BY (?2 IS NOT NULL AND tmdb_id = ?2) DESC, id
                 LIMIT 1
```

Bound: `["movie",1012345,null]`

```text
QUERY PLAN
SCAN items USING INDEX idx_items_library_kind
USE TEMP B-TREE FOR ORDER BY
```

#### `files_for_item` — `files_for_item`

```sql
SELECT id, item_id, path, size, mtime, duration_ms, container, video_codec, video_profile, width, height, bit_depth, hdr, bitrate, audio_streams, subtitle_streams, scanned_at, hdr_format, audio_offset_ms, (probe_json IS NOT NULL), dv_profile, dv_level, dv_bl_compat_id, dv_el_present, dv_rpu_present, video_codec_tag, field_order, max_cll, max_fall, mastering_max_luminance, luminance_source, downloaded_subtitles FROM files WHERE item_id = ?1
                 ORDER BY height DESC, bitrate DESC, path
```

Bound: `[12345]`

```text
QUERY PLAN
SEARCH files USING INDEX idx_files_item (item_id=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `item_media_facts` — `item_media_facts`

```sql
WITH ranked AS (
                     SELECT item_id,
                            COUNT(*)  OVER (PARTITION BY item_id) AS n_files,
                            SUM(size) OVER (PARTITION BY item_id) AS total_bytes,
                            ROW_NUMBER() OVER (
                                PARTITION BY item_id
                                ORDER BY COALESCE(height, 0) DESC,
                                         COALESCE(bitrate, 0) DESC,
                                         size DESC,
                                         id ASC) AS pick,
                            container, video_codec, height, hdr, hdr_format,
                            audio_streams
                     FROM files
                     WHERE item_id IN (1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50)
                 )
                 SELECT item_id, n_files, total_bytes, container, video_codec,
                        height, hdr, hdr_format, audio_streams
                 FROM ranked WHERE pick = 1
```

Bound: `[]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-3)
    CO-ROUTINE (subquery-4)
      SEARCH files USING INDEX idx_files_item (item_id=?)
      USE TEMP B-TREE FOR LAST 4 TERMS OF ORDER BY
    SCAN (subquery-4)
  SCAN (subquery-3)
SCAN ranked
```

#### `watch_map` — `watch_map`

```sql
SELECT w.item_id, w.position_ms, w.duration_ms, w.watched, w.updated_at
     FROM json_each(?2) j
     CROSS JOIN watch_state w ON w.user_id = ?1 AND w.item_id = j.value
```

Bound: `[1,"[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50]"]`

```text
QUERY PLAN
SCAN j VIRTUAL TABLE INDEX 1:
SEARCH w USING INDEX sqlite_autoindex_watch_state_1 (user_id=? AND item_id=?)
```

#### `watch_rollups` — `watch_rollups`

```sql
WITH RECURSIVE tree(root, id) AS (
                     SELECT id, id FROM items WHERE id IN (20001,20106,20211,20316,20421,20526,20631,20736,20841,20946,21051,21156,21261,21366,21471,21576,21681,21786,21891,21996,22101,22206,22311,22416)
                     UNION
                     SELECT t.root, i.id FROM items i JOIN tree t ON i.parent_id = t.id
                 )
                 SELECT t.root, COUNT(*), COALESCE(SUM(w.watched), 0)
                 FROM tree t
                 JOIN items i ON i.id = t.id
                 LEFT JOIN watch_state w ON w.item_id = i.id AND w.user_id = ?1
                 WHERE i.kind IN ('movie','episode','video','audiobook')
                 GROUP BY t.root
```

Bound: `[1]`

```text
QUERY PLAN
CO-ROUTINE tree
  SETUP
    SEARCH items USING INTEGER PRIMARY KEY (rowid=?)
  RECURSIVE STEP
    SCAN t
    SEARCH i USING COVERING INDEX idx_items_parent (parent_id=?)
SCAN t
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
SEARCH w USING INDEX sqlite_autoindex_watch_state_1 (user_id=? AND item_id=?) LEFT-JOIN
USE TEMP B-TREE FOR GROUP BY
```

#### `continue_watching` — `continue_watching`

```sql
SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title,
                        w.position_ms, w.duration_ms, w.watched, w.updated_at,
                        season.poster_path
                 FROM watch_state w
                 JOIN items i ON i.id = w.item_id
                 LEFT JOIN items season
                        ON season.id = i.parent_id AND i.kind = 'episode'
                 LEFT JOIN items show ON show.id = season.parent_id
                 WHERE w.user_id = ?1 AND w.watched = 0 AND w.position_ms > 0
                   AND i.kind IN ('movie','episode','video','audiobook')
                 ORDER BY w.updated_at DESC LIMIT ?2
```

Bound: `[1,24]`

```text
QUERY PLAN
SEARCH w USING INDEX idx_watch_updated (user_id=?)
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
```

#### `next_up` — `next_up`

```sql
SELECT e.id, e.library_id, e.kind, e.parent_id, e.title, e.sort_title, e.year, e.overview, e.tmdb_id, e.imdb_id, e.season_number, e.episode_number, e.air_date, e.runtime_ms, e.poster_path, e.backdrop_path, e.added_at, e.updated_at, e.recorded_at, e.tags, e.nfo_seeded_at, e.artwork_attempted_at, e.artwork_error, e.genres, e.author, e.book_work_id, e.book_edition_id, e.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, MIN(season.season_number*100000 + e.episode_number) AS ord FROM items e JOIN items season ON season.id = e.parent_id JOIN items show ON show.id = season.parent_id WHERE e.kind = 'episode' AND e.id NOT IN (SELECT item_id FROM watch_state WHERE user_id = ?1 AND (watched = 1 OR position_ms > 0)) AND (season.season_number*100000 + e.episode_number) > ( SELECT COALESCE(MAX(se.season_number*100000 + ep.episode_number), -1) FROM watch_state w JOIN items ep ON ep.id = w.item_id AND ep.kind = 'episode' JOIN items se ON se.id = ep.parent_id WHERE w.user_id = ?1 AND w.watched = 1 AND se.parent_id = show.id) AND show.id IN (SELECT sh.id FROM watch_state w JOIN items ep ON ep.id = w.item_id AND ep.kind = 'episode' JOIN items se ON se.id = ep.parent_id JOIN items sh ON sh.id = se.parent_id WHERE w.user_id = ?1 AND w.watched = 1) AND show.id NOT IN (SELECT sh.id FROM watch_state w JOIN items ep ON ep.id = w.item_id AND ep.kind = 'episode' JOIN items se ON se.id = ep.parent_id JOIN items sh ON sh.id = se.parent_id WHERE w.user_id = ?1 AND w.watched = 0 AND w.position_ms > 0) GROUP BY show.id ORDER BY show.sort_title LIMIT ?2
```

Bound: `[1,24]`

```text
QUERY PLAN
SEARCH show USING INTEGER PRIMARY KEY (rowid=?)
LIST SUBQUERY 3
  SEARCH w USING INDEX idx_watch_updated (user_id=?)
  SEARCH ep USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH se USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH sh USING INTEGER PRIMARY KEY (rowid=?)
LIST SUBQUERY 4
  SEARCH w USING INDEX idx_watch_updated (user_id=?)
  SEARCH ep USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH se USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH sh USING INTEGER PRIMARY KEY (rowid=?)
  CREATE BLOOM FILTER
SEARCH season USING INDEX idx_items_parent (parent_id=?)
SEARCH e USING INDEX idx_items_parent (parent_id=?)
LIST SUBQUERY 1
  SEARCH watch_state USING INDEX idx_watch_updated (user_id=?)
  CREATE BLOOM FILTER
CORRELATED SCALAR SUBQUERY 2
  SEARCH w USING INDEX idx_watch_updated (user_id=?)
  SEARCH ep USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH se USING INTEGER PRIMARY KEY (rowid=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `authenticate_token` — `authenticate_token`

```sql
SELECT u.id, u.username, u.password_hash, u.is_admin, u.created_at,
                            t.last_seen_at,
                            (SELECT value FROM settings WHERE key = ?2),
                            (SELECT value FROM settings WHERE key = ?3),
                            (SELECT value FROM settings WHERE key = ?4),
                            unixepoch()
                     FROM users u
                     JOIN tokens t ON t.user_id = u.id
                     WHERE t.token_hash = ?1
```

Bound: `["fixture-token-1","auth.token_expiry_enabled","auth.token_idle_days","auth.token_expiry_since"]`

```text
QUERY PLAN
SEARCH t USING INDEX sqlite_autoindex_tokens_1 (token_hash=?)
SEARCH u USING INTEGER PRIMARY KEY (rowid=?)
SCALAR SUBQUERY 1
  SEARCH settings USING INDEX sqlite_autoindex_settings_1 (key=?)
SCALAR SUBQUERY 2
  SEARCH settings USING INDEX sqlite_autoindex_settings_1 (key=?)
SCALAR SUBQUERY 3
  SEARCH settings USING INDEX sqlite_autoindex_settings_1 (key=?)
```


## Appendix D — Hiqlite schema, after (this branch, before ea62fa80d)


| Call | Statement | Rows | Cold median | Warm median |
|---|---|---:|---:|---:|
| `library_page_title` | `list_top_items_in_genre.count` | 1 | 5.00 ms | 3.219 ms |
| `library_page_title` | `list_top_items_in_genre.page` | 50 | 20.38 ms | 15.041 ms |
| `library_page_title_genre` | `list_top_items_in_genre.count` | 1 | 12.05 ms | 8.326 ms |
| `library_page_title_genre` | `list_top_items_in_genre.page` | 0 | 16.30 ms | 12.726 ms |
| `library_page_added` | `list_top_items_in_genre.count` | 1 | 4.40 ms | 2.552 ms |
| `library_page_added` | `list_top_items_in_genre.page` | 50 | 39.34 ms | 33.813 ms |
| `library_page_year` | `list_top_items_in_genre.count` | 1 | 4.98 ms | 2.976 ms |
| `library_page_year` | `list_top_items_in_genre.page` | 50 | 42.94 ms | 36.876 ms |
| `library_page_resolution` | `list_top_items_in_genre.count` | 1 | 5.01 ms | 2.866 ms |
| `library_page_resolution` | `list_top_items_in_genre.page` | 50 | 74.08 ms | 75.958 ms |
| `shows_page_title` | `list_top_items_in_genre.count` | 1 | 14.02 ms | 11.479 ms |
| `shows_page_title` | `list_top_items_in_genre.page` | 50 | 16.00 ms | 12.557 ms |
| `home_page_recorded` | `list_top_items_in_genre.count` | 1 | 2.49 ms | 0.554 ms |
| `home_page_recorded` | `list_top_items_in_genre.page` | 50 | 2.78 ms | 0.845 ms |
| `home_preview_pages` | `home_preview_pages` | 96 | 46.63 ms | 29.783 ms |
| `recently_added_all` | `recently_added` | 2 | 2.89 ms | 0.799 ms |
| `recently_added_all` | `recently_added` | 4 | 2.69 ms | 1.281 ms |
| `recently_added_all` | `recently_added` | 8 | 3.61 ms | 2.190 ms |
| `recently_added_all` | `recently_added` | 16 | 5.46 ms | 4.009 ms |
| `recently_added_all` | `recently_added` | 24 | 15.25 ms | 12.927 ms |
| `recently_added_shows` | `recently_added` | 2 | 3.35 ms | 1.389 ms |
| `recently_added_shows` | `recently_added` | 4 | 4.26 ms | 2.288 ms |
| `recently_added_shows` | `recently_added` | 8 | 6.04 ms | 3.091 ms |
| `recently_added_shows` | `recently_added` | 16 | 7.03 ms | 5.023 ms |
| `recently_added_shows` | `recently_added` | 24 | 11.27 ms | 11.221 ms |
| `search` | `search_items` | 10 | 3.18 ms | 1.195 ms |
| `item_by_external_id` | `item_by_external_id` | 1 | 8.84 ms | 6.767 ms |
| `files_for_item` | `files_for_item` | 2 | 2.03 ms | 0.055 ms |
| `item_media_facts` | `item_media_facts` | 50 | 2.65 ms | 0.490 ms |
| `watch_map` | `watch_map` | 5 | 2.02 ms | 0.036 ms |
| `watch_rollups` | `watch_rollups` | 24 | 6.78 ms | 4.602 ms |
| `continue_watching` | `continue_watching` | 24 | 2.20 ms | 0.192 ms |
| `next_up` | `next_up` | 0 | 19.27 ms | 16.916 ms |
| `authenticate_token` | `authenticate_token` | 1 | 1.89 ms | 0.013 ms |

Per call (sums of the statement medians above):

| Call | Statements | Cold | Warm |
|---|---:|---:|---:|
| `library_page_title` | 2 | 25.38 ms | 18.260 ms |
| `library_page_title_genre` | 2 | 28.35 ms | 21.052 ms |
| `library_page_added` | 2 | 43.74 ms | 36.365 ms |
| `library_page_year` | 2 | 47.92 ms | 39.852 ms |
| `library_page_resolution` | 2 | 79.09 ms | 78.825 ms |
| `shows_page_title` | 2 | 30.02 ms | 24.036 ms |
| `home_page_recorded` | 2 | 5.27 ms | 1.399 ms |
| `home_preview_pages` | 1 | 46.63 ms | 29.783 ms |
| `recently_added_all` | 5 | 29.90 ms | 21.206 ms |
| `recently_added_shows` | 5 | 31.95 ms | 23.011 ms |
| `search` | 1 | 3.18 ms | 1.195 ms |
| `item_by_external_id` | 1 | 8.84 ms | 6.767 ms |
| `files_for_item` | 1 | 2.03 ms | 0.055 ms |
| `item_media_facts` | 1 | 2.65 ms | 0.490 ms |
| `watch_map` | 1 | 2.02 ms | 0.036 ms |
| `watch_rollups` | 1 | 6.78 ms | 4.602 ms |
| `continue_watching` | 1 | 2.20 ms | 0.192 ms |
| `next_up` | 1 | 19.27 ms | 16.916 ms |
| `authenticate_token` | 1 | 1.89 ms | 0.013 ms |

#### `library_page_title` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_title` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE)) ORDER BY sort_title ASC, id ASC LIMIT $3 OFFSET $4
```

Bound: `[1,null,50,10000]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_title_genre` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[1,"Drama"]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_title_genre` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE)) ORDER BY sort_title ASC, id ASC LIMIT $3 OFFSET $4
```

Bound: `[1,"Drama",50,10000]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_added` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_added` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE)) ORDER BY added_at DESC, id DESC LIMIT $3 OFFSET $4
```

Bound: `[1,null,50,10000]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_year` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_year` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE)) ORDER BY year IS NULL, year DESC, sort_title ASC, id ASC LIMIT $3 OFFSET $4
```

Bound: `[1,null,50,10000]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_resolution` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_resolution` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE)) ORDER BY CASE WHEN kind IN ('movie','video') THEN COALESCE((SELECT MAX(f.height) FROM files f WHERE f.item_id = items.id), -1) ELSE -1 END DESC, sort_title ASC, id ASC LIMIT $3 OFFSET $4
```

Bound: `[1,null,50,10000]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
CORRELATED SCALAR SUBQUERY 2
  SEARCH f USING INDEX idx_files_item (item_id=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `shows_page_title` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[2,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `shows_page_title` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE)) ORDER BY sort_title ASC, id ASC LIMIT $3 OFFSET $4
```

Bound: `[2,null,50,200]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `home_page_recorded` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[3,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `home_page_recorded` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE)) ORDER BY (recorded_at IS NULL), recorded_at DESC, sort_title ASC, id ASC LIMIT $3 OFFSET $4
```

Bound: `[3,null,50,0]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `home_preview_pages` — `home_preview_pages`

```sql
WITH ranked AS (
                 SELECT id, library_id,
                        COUNT(*) OVER (PARTITION BY library_id) AS library_total,
                        ROW_NUMBER() OVER (
                            PARTITION BY library_id
                            ORDER BY added_at DESC, id DESC
                        ) AS preview_rank
                   FROM items
                  WHERE (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL))
             ), selected AS (
                 SELECT id, library_id, library_total, preview_rank
                   FROM ranked
                  WHERE preview_rank <= $1
             )
             SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, selected.library_total
               FROM selected
               JOIN items i ON i.id = selected.id
              ORDER BY selected.library_id, selected.preview_rank
```

Bound: `[24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-4)
    CO-ROUTINE (subquery-5)
      SCAN items USING INDEX idx_items_library_kind
      USE TEMP B-TREE FOR LAST 2 TERMS OF ORDER BY
    SCAN (subquery-5)
  SCAN (subquery-4)
SCAN ranked
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_all` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) ORDER BY i.added_at DESC LIMIT 1 OFFSET $2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT $3
```

Bound: `[null,191,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-6)
    SEARCH i USING INDEX idx_items_added (added_at>?)
    SCALAR SUBQUERY 2
      MATERIALIZE cut
        SCAN i USING INDEX idx_items_added
      SCAN cut
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-6)
SCAN r
SCALAR SUBQUERY 4
  SCAN cut
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_all` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) ORDER BY i.added_at DESC LIMIT 1 OFFSET $2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT $3
```

Bound: `[null,383,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-6)
    SEARCH i USING INDEX idx_items_added (added_at>?)
    SCALAR SUBQUERY 2
      MATERIALIZE cut
        SCAN i USING INDEX idx_items_added
      SCAN cut
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-6)
SCAN r
SCALAR SUBQUERY 4
  SCAN cut
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_all` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) ORDER BY i.added_at DESC LIMIT 1 OFFSET $2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT $3
```

Bound: `[null,767,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-6)
    SEARCH i USING INDEX idx_items_added (added_at>?)
    SCALAR SUBQUERY 2
      MATERIALIZE cut
        SCAN i USING INDEX idx_items_added
      SCAN cut
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-6)
SCAN r
SCALAR SUBQUERY 4
  SCAN cut
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_all` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) ORDER BY i.added_at DESC LIMIT 1 OFFSET $2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT $3
```

Bound: `[null,1535,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-6)
    SEARCH i USING INDEX idx_items_added (added_at>?)
    SCALAR SUBQUERY 2
      MATERIALIZE cut
        SCAN i USING INDEX idx_items_added
      SCAN cut
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-6)
SCAN r
SCALAR SUBQUERY 4
  SCAN cut
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_all` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) ORDER BY i.added_at DESC LIMIT 1 OFFSET $2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT $3
```

Bound: `[null,3071,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-6)
    SEARCH i USING INDEX idx_items_added (added_at>?)
    SCALAR SUBQUERY 2
      MATERIALIZE cut
        SCAN i USING INDEX idx_items_added
      SCAN cut
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-6)
SCAN r
SCALAR SUBQUERY 4
  SCAN cut
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_shows` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) ORDER BY i.added_at DESC LIMIT 1 OFFSET $2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT $3
```

Bound: `[2,191,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-6)
    SEARCH i USING INDEX idx_items_added (added_at>?)
    SCALAR SUBQUERY 2
      MATERIALIZE cut
        SCAN i USING INDEX idx_items_added
      SCAN cut
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-6)
SCAN r
SCALAR SUBQUERY 4
  SCAN cut
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_shows` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) ORDER BY i.added_at DESC LIMIT 1 OFFSET $2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT $3
```

Bound: `[2,383,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-6)
    SEARCH i USING INDEX idx_items_added (added_at>?)
    SCALAR SUBQUERY 2
      MATERIALIZE cut
        SCAN i USING INDEX idx_items_added
      SCAN cut
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-6)
SCAN r
SCALAR SUBQUERY 4
  SCAN cut
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_shows` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) ORDER BY i.added_at DESC LIMIT 1 OFFSET $2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT $3
```

Bound: `[2,767,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-6)
    SEARCH i USING INDEX idx_items_added (added_at>?)
    SCALAR SUBQUERY 2
      MATERIALIZE cut
        SCAN i USING INDEX idx_items_added
      SCAN cut
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-6)
SCAN r
SCALAR SUBQUERY 4
  SCAN cut
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_shows` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) ORDER BY i.added_at DESC LIMIT 1 OFFSET $2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT $3
```

Bound: `[2,1535,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-6)
    SEARCH i USING INDEX idx_items_added (added_at>?)
    SCALAR SUBQUERY 2
      MATERIALIZE cut
        SCAN i USING INDEX idx_items_added
      SCAN cut
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-6)
SCAN r
SCALAR SUBQUERY 4
  SCAN cut
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_shows` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) ORDER BY i.added_at DESC LIMIT 1 OFFSET $2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT $3
```

Bound: `[2,3071,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-6)
    SEARCH i USING INDEX idx_items_added (added_at>?)
    SCALAR SUBQUERY 2
      MATERIALIZE cut
        SCAN i USING INDEX idx_items_added
      SCAN cut
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-6)
SCAN r
SCALAR SUBQUERY 4
  SCAN cut
USE TEMP B-TREE FOR ORDER BY
```

#### `search` — `search_items`

```sql
WITH hits AS MATERIALIZED (SELECT rowid,rank AS score FROM items_fts WHERE items_fts MATCH $1 AND NOT EXISTS (SELECT 1 FROM classification_fts c WHERE c.rowid = items_fts.rowid) UNION ALL SELECT rowid,rank AS score FROM classification_fts WHERE classification_fts MATCH $1) SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster FROM (SELECT rowid,min(score) AS score FROM hits GROUP BY rowid) f JOIN items i ON i.id = f.rowid LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','show','episode','folder','video','photo','book','audiobook') ORDER BY f.score, i.id LIMIT $2
```

Bound: `["(\"movie\") AND (\"0123\"*)",50]`

```text
QUERY PLAN
CO-ROUTINE f
  MATERIALIZE hits
    COMPOUND QUERY
      LEFT-MOST SUBQUERY
        SCAN items_fts VIRTUAL TABLE INDEX 0:M3
        CORRELATED SCALAR SUBQUERY 1
          SCAN c VIRTUAL TABLE INDEX 0:=
      UNION ALL
        SCAN classification_fts VIRTUAL TABLE INDEX 0:M1
  SCAN hits
  USE TEMP B-TREE FOR GROUP BY
SCAN f
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
USE TEMP B-TREE FOR ORDER BY
```

#### `item_by_external_id` — `item_by_external_id`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE kind = $1 AND (($2 IS NOT NULL AND tmdb_id = $2) OR ($3 IS NOT NULL AND imdb_id = $3 COLLATE NOCASE)) ORDER BY ($2 IS NOT NULL AND tmdb_id = $2) DESC, id LIMIT 1
```

Bound: `["movie",1012345,null]`

```text
QUERY PLAN
SCAN items USING INDEX idx_items_library_kind
USE TEMP B-TREE FOR ORDER BY
```

#### `files_for_item` — `files_for_item`

```sql
SELECT id, item_id, path, size, mtime, duration_ms, container, video_codec, video_profile, width, height, bit_depth, hdr, bitrate, audio_streams, subtitle_streams, scanned_at, hdr_format, audio_offset_ms, dv_profile, dv_level, dv_bl_compat_id, dv_el_present, dv_rpu_present, (probe_json IS NOT NULL) AS probed, video_codec_tag, field_order, max_cll, max_fall, mastering_max_luminance, luminance_source, downloaded_subtitles FROM files WHERE item_id = $1 ORDER BY height DESC, bitrate DESC, path
```

Bound: `[12345]`

```text
QUERY PLAN
SEARCH files USING INDEX idx_files_item (item_id=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `item_media_facts` — `item_media_facts`

```sql
WITH ranked AS ( SELECT item_id, COUNT(*) OVER (PARTITION BY item_id) AS files, SUM(size) OVER (PARTITION BY item_id) AS bytes, ROW_NUMBER() OVER (PARTITION BY item_id ORDER BY COALESCE(height, 0) DESC, COALESCE(bitrate, 0) DESC, size DESC, id ASC) AS pick, container, video_codec, height, hdr, hdr_format, audio_streams FROM files WHERE item_id IN (SELECT value FROM json_each($1)) ) SELECT item_id, files, bytes, container, video_codec, height, hdr, hdr_format, audio_streams FROM ranked WHERE pick = 1
```

Bound: `["[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50]"]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-4)
    CO-ROUTINE (subquery-5)
      SEARCH files USING INDEX idx_files_item (item_id=?)
      LIST SUBQUERY 1
        SCAN json_each VIRTUAL TABLE INDEX 1:
      USE TEMP B-TREE FOR LAST 4 TERMS OF ORDER BY
    SCAN (subquery-5)
  SCAN (subquery-4)
SCAN ranked
```

#### `watch_map` — `watch_map`

```sql
SELECT w.item_id, w.position_ms, w.duration_ms, w.watched, w.updated_at FROM json_each($1) j CROSS JOIN watch_state w ON w.user_id = $2 AND w.item_id = j.value
```

Bound: `["[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50]",1]`

```text
QUERY PLAN
SCAN j VIRTUAL TABLE INDEX 1:
SEARCH w USING INDEX sqlite_autoindex_watch_state_1 (user_id=? AND item_id=?)
```

#### `watch_rollups` — `watch_rollups`

```sql
WITH RECURSIVE tree(root, id) AS ( SELECT id, id FROM items WHERE id IN (SELECT value FROM json_each($1)) UNION SELECT t.root, i.id FROM items i JOIN tree t ON i.parent_id = t.id ) SELECT t.root AS root, COUNT(*) AS leaves, COALESCE(SUM(w.watched), 0) AS watched FROM tree t JOIN items i ON i.id = t.id LEFT JOIN watch_state w ON w.item_id = i.id AND w.user_id = $2 WHERE i.kind IN ('movie','episode','video','audiobook') GROUP BY t.root
```

Bound: `["[20001,20106,20211,20316,20421,20526,20631,20736,20841,20946,21051,21156,21261,21366,21471,21576,21681,21786,21891,21996,22101,22206,22311,22416]",1]`

```text
QUERY PLAN
CO-ROUTINE tree
  SETUP
    SEARCH items USING INTEGER PRIMARY KEY (rowid=?)
    LIST SUBQUERY 1
      SCAN json_each VIRTUAL TABLE INDEX 1:
  RECURSIVE STEP
    SCAN t
    SEARCH i USING COVERING INDEX idx_items_parent (parent_id=?)
SCAN t
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
SEARCH w USING INDEX sqlite_autoindex_watch_state_1 (user_id=? AND item_id=?) LEFT-JOIN
USE TEMP B-TREE FOR GROUP BY
```

#### `continue_watching` — `continue_watching`

```sql
SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, w.position_ms AS watch_position_ms, w.duration_ms AS watch_duration_ms, w.watched AS watch_watched, w.updated_at AS watch_updated_at FROM watch_state w JOIN items i ON i.id = w.item_id LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE w.user_id = $1 AND w.watched = 0 AND w.position_ms > 0 AND i.kind IN ('movie','episode','video','audiobook') ORDER BY w.updated_at DESC LIMIT $2
```

Bound: `[1,24]`

```text
QUERY PLAN
SEARCH w USING INDEX idx_watch_updated (user_id=?)
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
```

#### `next_up` — `next_up`

```sql
SELECT e.id, e.library_id, e.kind, e.parent_id, e.title, e.sort_title, e.year, e.overview, e.tmdb_id, e.imdb_id, e.season_number, e.episode_number, e.air_date, e.runtime_ms, e.poster_path, e.backdrop_path, e.added_at, e.updated_at, e.recorded_at, e.tags, e.nfo_seeded_at, e.artwork_attempted_at, e.artwork_error, e.genres, e.author, e.book_work_id, e.book_edition_id, e.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, MIN(season.season_number*100000 + e.episode_number) AS ord FROM items e JOIN items season ON season.id = e.parent_id JOIN items show ON show.id = season.parent_id WHERE e.kind = 'episode' AND e.id NOT IN (SELECT item_id FROM watch_state WHERE user_id = $1 AND (watched = 1 OR position_ms > 0)) AND (season.season_number*100000 + e.episode_number) > ( SELECT COALESCE(MAX(se.season_number*100000 + ep.episode_number), -1) FROM watch_state w JOIN items ep ON ep.id = w.item_id AND ep.kind = 'episode' JOIN items se ON se.id = ep.parent_id WHERE w.user_id = $1 AND w.watched = 1 AND se.parent_id = show.id) AND show.id IN (SELECT sh.id FROM watch_state w JOIN items ep ON ep.id = w.item_id AND ep.kind = 'episode' JOIN items se ON se.id = ep.parent_id JOIN items sh ON sh.id = se.parent_id WHERE w.user_id = $1 AND w.watched = 1) AND show.id NOT IN (SELECT sh.id FROM watch_state w JOIN items ep ON ep.id = w.item_id AND ep.kind = 'episode' JOIN items se ON se.id = ep.parent_id JOIN items sh ON sh.id = se.parent_id WHERE w.user_id = $1 AND w.watched = 0 AND w.position_ms > 0) GROUP BY show.id ORDER BY show.sort_title LIMIT $2
```

Bound: `[1,24]`

```text
QUERY PLAN
SEARCH show USING INTEGER PRIMARY KEY (rowid=?)
LIST SUBQUERY 3
  SEARCH w USING INDEX idx_watch_updated (user_id=?)
  SEARCH ep USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH se USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH sh USING INTEGER PRIMARY KEY (rowid=?)
LIST SUBQUERY 4
  SEARCH w USING INDEX idx_watch_updated (user_id=?)
  SEARCH ep USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH se USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH sh USING INTEGER PRIMARY KEY (rowid=?)
  CREATE BLOOM FILTER
SEARCH season USING INDEX idx_items_parent (parent_id=?)
SEARCH e USING INDEX idx_items_parent (parent_id=?)
LIST SUBQUERY 1
  SEARCH watch_state USING INDEX idx_watch_updated (user_id=?)
  CREATE BLOOM FILTER
CORRELATED SCALAR SUBQUERY 2
  SEARCH w USING INDEX idx_watch_updated (user_id=?)
  SEARCH ep USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH se USING INTEGER PRIMARY KEY (rowid=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `authenticate_token` — `authenticate_token`

```sql
SELECT u.id, u.username, u.password_hash, u.is_admin, u.created_at, t.last_seen_at, (SELECT value FROM settings WHERE key = $1) AS expiry_enabled, (SELECT value FROM settings WHERE key = $2) AS expiry_idle_days, (SELECT value FROM settings WHERE key = $3) AS expiry_since FROM users u JOIN tokens t ON t.user_id = u.id WHERE t.token_hash = $4
```

Bound: `["auth.token_expiry_enabled","auth.token_idle_days","auth.token_expiry_since","fixture-token-1"]`

```text
QUERY PLAN
SEARCH t USING INDEX sqlite_autoindex_tokens_1 (token_hash=?)
SEARCH u USING INTEGER PRIMARY KEY (rowid=?)
SCALAR SUBQUERY 1
  SEARCH settings USING INDEX sqlite_autoindex_settings_1 (key=?)
SCALAR SUBQUERY 2
  SEARCH settings USING INDEX sqlite_autoindex_settings_1 (key=?)
SCALAR SUBQUERY 3
  SEARCH settings USING INDEX sqlite_autoindex_settings_1 (key=?)
```


## Appendix E — SQLite with statistics (analysis_limit = 400; ANALYZE), after M1-M4, first recently_added pass only


| Call | Statement | Rows | Cold median | Warm median |
|---|---|---:|---:|---:|
| `library_page_title` | `list_top_items_in_genre.count` | 1 | 4.20 ms | 2.386 ms |
| `library_page_title` | `list_top_items_in_genre.page` | 50 | 10.26 ms | 8.638 ms |
| `library_page_title_genre` | `list_top_items_in_genre.count` | 1 | 6.95 ms | 5.270 ms |
| `library_page_title_genre` | `list_top_items_in_genre.page` | 0 | 10.22 ms | 8.188 ms |
| `library_page_added` | `list_top_items_in_genre.count` | 1 | 2.93 ms | 1.620 ms |
| `library_page_added` | `list_top_items_in_genre.page` | 50 | 23.05 ms | 19.526 ms |
| `library_page_year` | `list_top_items_in_genre.count` | 1 | 2.61 ms | 1.385 ms |
| `library_page_year` | `list_top_items_in_genre.page` | 50 | 24.70 ms | 21.706 ms |
| `library_page_resolution` | `list_top_items_in_genre.count` | 1 | 3.09 ms | 1.672 ms |
| `library_page_resolution` | `list_top_items_in_genre.page` | 50 | 43.64 ms | 42.554 ms |
| `shows_page_title` | `list_top_items_in_genre.count` | 1 | 8.26 ms | 7.111 ms |
| `shows_page_title` | `list_top_items_in_genre.page` | 50 | 8.89 ms | 7.502 ms |
| `home_page_recorded` | `list_top_items_in_genre.count` | 1 | 1.58 ms | 0.334 ms |
| `home_page_recorded` | `list_top_items_in_genre.page` | 50 | 1.76 ms | 0.469 ms |
| `home_preview_pages` | `home_preview_pages` | 96 | 29.92 ms | 27.757 ms |
| `recently_added_all` | `recently_added` | 2 | 2.12 ms | 0.780 ms |
| `recently_added_shows` | `recently_added` | 2 | 2.09 ms | 0.730 ms |
| `search` | `search_items` | 10 | 1.85 ms | 0.581 ms |
| `item_by_external_id` | `item_by_external_id` | 1 | 4.62 ms | 3.439 ms |
| `files_for_item` | `files_for_item` | 2 | 1.08 ms | 0.020 ms |
| `item_media_facts` | `item_media_facts` | 50 | 1.44 ms | 0.236 ms |
| `watch_map` | `watch_map` | 5 | 1.11 ms | 0.016 ms |
| `watch_rollups` | `watch_rollups` | 24 | 9.40 ms | 7.992 ms |
| `continue_watching` | `continue_watching` | 24 | 1.30 ms | 0.093 ms |
| `next_up` | `next_up` | 0 | 10.44 ms | 9.571 ms |
| `authenticate_token` | `authenticate_token` | 1 | 1.01 ms | 0.011 ms |

#### `library_page_title` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_title` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?4 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE))
                 ORDER BY sort_title ASC, id ASC LIMIT ?3 OFFSET ?2
```

Bound: `[1,10000,50,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_title_genre` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[1,"Drama"]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_title_genre` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?4 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE))
                 ORDER BY sort_title ASC, id ASC LIMIT ?3 OFFSET ?2
```

Bound: `[1,10000,50,"Drama"]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_added` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_added` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?4 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE))
                 ORDER BY added_at DESC, id DESC LIMIT ?3 OFFSET ?2
```

Bound: `[1,10000,50,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_year` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_year` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?4 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE))
                 ORDER BY year IS NULL, year DESC, sort_title ASC, id ASC LIMIT ?3 OFFSET ?2
```

Bound: `[1,10000,50,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_resolution` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_resolution` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?4 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE))
                 ORDER BY CASE WHEN kind IN ('movie','video') THEN COALESCE((SELECT MAX(f.height) FROM files f WHERE f.item_id = items.id), -1) ELSE -1 END DESC, sort_title ASC, id ASC LIMIT ?3 OFFSET ?2
```

Bound: `[1,10000,50,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
CORRELATED SCALAR SUBQUERY 2
  SEARCH f USING INDEX idx_files_item (item_id=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `shows_page_title` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[2,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `shows_page_title` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?4 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE))
                 ORDER BY sort_title ASC, id ASC LIMIT ?3 OFFSET ?2
```

Bound: `[2,200,50,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `home_page_recorded` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[3,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `home_page_recorded` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?4 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE))
                 ORDER BY (recorded_at IS NULL), recorded_at DESC, sort_title ASC, id ASC LIMIT ?3 OFFSET ?2
```

Bound: `[3,0,50,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `home_preview_pages` — `home_preview_pages`

```sql
WITH ranked AS (
                     SELECT id, library_id,
                            COUNT(*) OVER (PARTITION BY library_id) AS library_total,
                            ROW_NUMBER() OVER (
                                PARTITION BY library_id
                                ORDER BY added_at DESC, id DESC
                            ) AS preview_rank
                       FROM items
                      WHERE (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL))
                 ), selected AS (
                     SELECT id, library_id, library_total, preview_rank
                       FROM ranked
                      WHERE preview_rank <= ?1
                 )
                 SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, selected.library_total
                   FROM selected
                   JOIN items i ON i.id = selected.id
                  ORDER BY selected.library_id, selected.preview_rank
```

Bound: `[24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-4)
    CO-ROUTINE (subquery-5)
      SCAN items USING INDEX idx_items_library_kind
      USE TEMP B-TREE FOR LAST 2 TERMS OF ORDER BY
    SCAN (subquery-5)
  SCAN (subquery-4)
SCAN ranked
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_all` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) ORDER BY i.added_at DESC LIMIT 1 OFFSET ?2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT ?3
```

Bound: `[null,191,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-8)
    SEARCH i USING INDEX idx_items_added (added_at>?)
    SCALAR SUBQUERY 4
      MATERIALIZE cut
        SCAN i USING INDEX idx_items_added
        CORRELATED SCALAR SUBQUERY 1
          SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
      SCAN cut
    CORRELATED SCALAR SUBQUERY 3
      SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-8)
SCAN r
SCALAR SUBQUERY 6
  SCAN cut
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_shows` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) ORDER BY i.added_at DESC LIMIT 1 OFFSET ?2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND (?1 IS NULL OR i.library_id = ?1) AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT ?3
```

Bound: `[2,191,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-8)
    SEARCH i USING INDEX idx_items_added (added_at>?)
    SCALAR SUBQUERY 4
      MATERIALIZE cut
        SCAN i USING INDEX idx_items_added
        CORRELATED SCALAR SUBQUERY 1
          SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
      SCAN cut
    CORRELATED SCALAR SUBQUERY 3
      SEARCH l USING INTEGER PRIMARY KEY (rowid=?)
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-8)
SCAN r
SCALAR SUBQUERY 6
  SCAN cut
USE TEMP B-TREE FOR ORDER BY
```

#### `search` — `search_items`

```sql
WITH hits AS MATERIALIZED (SELECT rowid,rank AS score FROM items_fts WHERE items_fts MATCH ?1 AND NOT EXISTS (SELECT 1 FROM classification_fts c WHERE c.rowid = items_fts.rowid) UNION ALL SELECT rowid,rank AS score FROM classification_fts WHERE classification_fts MATCH ?1) SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title, season.poster_path
                 FROM (SELECT rowid,min(score) AS score FROM hits GROUP BY rowid) f
                 JOIN items i ON i.id = f.rowid
                 LEFT JOIN items season
                        ON season.id = i.parent_id AND i.kind = 'episode'
                 LEFT JOIN items show ON show.id = season.parent_id
                 WHERE i.kind IN ('movie','show','episode','folder','video','photo','book','audiobook')
                 ORDER BY f.score, i.id LIMIT ?2
```

Bound: `["(\"movie\") AND (\"0123\"*)",50]`

```text
QUERY PLAN
CO-ROUTINE f
  MATERIALIZE hits
    COMPOUND QUERY
      LEFT-MOST SUBQUERY
        SCAN items_fts VIRTUAL TABLE INDEX 0:M3
        CORRELATED SCALAR SUBQUERY 1
          SCAN c VIRTUAL TABLE INDEX 0:=
      UNION ALL
        SCAN classification_fts VIRTUAL TABLE INDEX 0:M1
  SCAN hits
  USE TEMP B-TREE FOR GROUP BY
SCAN f
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
USE TEMP B-TREE FOR ORDER BY
```

#### `item_by_external_id` — `item_by_external_id`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items
                 WHERE kind = ?1
                   AND ((?2 IS NOT NULL AND tmdb_id = ?2)
                     OR (?3 IS NOT NULL AND imdb_id = ?3 COLLATE NOCASE))
                 ORDER BY (?2 IS NOT NULL AND tmdb_id = ?2) DESC, id
                 LIMIT 1
```

Bound: `["movie",1012345,null]`

```text
QUERY PLAN
SCAN items USING INDEX idx_items_library_kind
USE TEMP B-TREE FOR ORDER BY
```

#### `files_for_item` — `files_for_item`

```sql
SELECT id, item_id, path, size, mtime, duration_ms, container, video_codec, video_profile, width, height, bit_depth, hdr, bitrate, audio_streams, subtitle_streams, scanned_at, hdr_format, audio_offset_ms, (probe_json IS NOT NULL), dv_profile, dv_level, dv_bl_compat_id, dv_el_present, dv_rpu_present, video_codec_tag, field_order, max_cll, max_fall, mastering_max_luminance, luminance_source, downloaded_subtitles FROM files WHERE item_id = ?1
                 ORDER BY height DESC, bitrate DESC, path
```

Bound: `[12345]`

```text
QUERY PLAN
SEARCH files USING INDEX idx_files_item (item_id=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `item_media_facts` — `item_media_facts`

```sql
WITH ranked AS (
                     SELECT item_id,
                            COUNT(*)  OVER (PARTITION BY item_id) AS n_files,
                            SUM(size) OVER (PARTITION BY item_id) AS total_bytes,
                            ROW_NUMBER() OVER (
                                PARTITION BY item_id
                                ORDER BY COALESCE(height, 0) DESC,
                                         COALESCE(bitrate, 0) DESC,
                                         size DESC,
                                         id ASC) AS pick,
                            container, video_codec, height, hdr, hdr_format,
                            audio_streams
                     FROM files
                     WHERE item_id IN (1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50)
                 )
                 SELECT item_id, n_files, total_bytes, container, video_codec,
                        height, hdr, hdr_format, audio_streams
                 FROM ranked WHERE pick = 1
```

Bound: `[]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-3)
    CO-ROUTINE (subquery-4)
      SEARCH files USING INDEX idx_files_item (item_id=?)
      USE TEMP B-TREE FOR LAST 4 TERMS OF ORDER BY
    SCAN (subquery-4)
  SCAN (subquery-3)
SCAN ranked
```

#### `watch_map` — `watch_map`

```sql
SELECT w.item_id, w.position_ms, w.duration_ms, w.watched, w.updated_at
     FROM json_each(?2) j
     CROSS JOIN watch_state w ON w.user_id = ?1 AND w.item_id = j.value
```

Bound: `[1,"[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50]"]`

```text
QUERY PLAN
SCAN j VIRTUAL TABLE INDEX 1:
SEARCH w USING INDEX sqlite_autoindex_watch_state_1 (user_id=? AND item_id=?)
```

#### `watch_rollups` — `watch_rollups`

```sql
WITH RECURSIVE tree(root, id) AS (
                     SELECT id, id FROM items WHERE id IN (20001,20106,20211,20316,20421,20526,20631,20736,20841,20946,21051,21156,21261,21366,21471,21576,21681,21786,21891,21996,22101,22206,22311,22416)
                     UNION
                     SELECT t.root, i.id FROM items i JOIN tree t ON i.parent_id = t.id
                 )
                 SELECT t.root, COUNT(*), COALESCE(SUM(w.watched), 0)
                 FROM tree t
                 JOIN items i ON i.id = t.id
                 LEFT JOIN watch_state w ON w.item_id = i.id AND w.user_id = ?1
                 WHERE i.kind IN ('movie','episode','video','audiobook')
                 GROUP BY t.root
```

Bound: `[1]`

```text
QUERY PLAN
CO-ROUTINE tree
  SETUP
    SEARCH items USING INTEGER PRIMARY KEY (rowid=?)
  RECURSIVE STEP
    SCAN t
    SEARCH i USING COVERING INDEX idx_items_parent (parent_id=?)
SCAN i USING COVERING INDEX idx_items_library_kind
BLOOM FILTER ON t (id=?)
SEARCH t USING AUTOMATIC COVERING INDEX (id=?)
SEARCH w USING INDEX sqlite_autoindex_watch_state_1 (user_id=? AND item_id=?) LEFT-JOIN
USE TEMP B-TREE FOR GROUP BY
```

#### `continue_watching` — `continue_watching`

```sql
SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title,
                        w.position_ms, w.duration_ms, w.watched, w.updated_at,
                        season.poster_path
                 FROM watch_state w
                 JOIN items i ON i.id = w.item_id
                 LEFT JOIN items season
                        ON season.id = i.parent_id AND i.kind = 'episode'
                 LEFT JOIN items show ON show.id = season.parent_id
                 WHERE w.user_id = ?1 AND w.watched = 0 AND w.position_ms > 0
                   AND i.kind IN ('movie','episode','video','audiobook')
                 ORDER BY w.updated_at DESC LIMIT ?2
```

Bound: `[1,24]`

```text
QUERY PLAN
SEARCH w USING INDEX idx_watch_updated (user_id=?)
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
```

#### `next_up` — `next_up`

```sql
SELECT e.id, e.library_id, e.kind, e.parent_id, e.title, e.sort_title, e.year, e.overview, e.tmdb_id, e.imdb_id, e.season_number, e.episode_number, e.air_date, e.runtime_ms, e.poster_path, e.backdrop_path, e.added_at, e.updated_at, e.recorded_at, e.tags, e.nfo_seeded_at, e.artwork_attempted_at, e.artwork_error, e.genres, e.author, e.book_work_id, e.book_edition_id, e.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, MIN(season.season_number*100000 + e.episode_number) AS ord FROM items e JOIN items season ON season.id = e.parent_id JOIN items show ON show.id = season.parent_id WHERE e.kind = 'episode' AND e.id NOT IN (SELECT item_id FROM watch_state WHERE user_id = ?1 AND (watched = 1 OR position_ms > 0)) AND (season.season_number*100000 + e.episode_number) > ( SELECT COALESCE(MAX(se.season_number*100000 + ep.episode_number), -1) FROM watch_state w JOIN items ep ON ep.id = w.item_id AND ep.kind = 'episode' JOIN items se ON se.id = ep.parent_id WHERE w.user_id = ?1 AND w.watched = 1 AND se.parent_id = show.id) AND show.id IN (SELECT sh.id FROM watch_state w JOIN items ep ON ep.id = w.item_id AND ep.kind = 'episode' JOIN items se ON se.id = ep.parent_id JOIN items sh ON sh.id = se.parent_id WHERE w.user_id = ?1 AND w.watched = 1) AND show.id NOT IN (SELECT sh.id FROM watch_state w JOIN items ep ON ep.id = w.item_id AND ep.kind = 'episode' JOIN items se ON se.id = ep.parent_id JOIN items sh ON sh.id = se.parent_id WHERE w.user_id = ?1 AND w.watched = 0 AND w.position_ms > 0) GROUP BY show.id ORDER BY show.sort_title LIMIT ?2
```

Bound: `[1,24]`

```text
QUERY PLAN
SEARCH show USING INTEGER PRIMARY KEY (rowid=?)
LIST SUBQUERY 3
  SEARCH w USING INDEX idx_watch_updated (user_id=?)
  SEARCH ep USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH se USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH sh USING INTEGER PRIMARY KEY (rowid=?)
LIST SUBQUERY 4
  SEARCH w USING INDEX idx_watch_updated (user_id=?)
  SEARCH ep USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH se USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH sh USING INTEGER PRIMARY KEY (rowid=?)
  CREATE BLOOM FILTER
SEARCH season USING INDEX idx_items_parent (parent_id=?)
SEARCH e USING INDEX idx_items_parent (parent_id=?)
LIST SUBQUERY 1
  SEARCH watch_state USING INDEX idx_watch_updated (user_id=?)
  CREATE BLOOM FILTER
CORRELATED SCALAR SUBQUERY 2
  SEARCH se USING INDEX idx_items_parent (parent_id=?)
  SEARCH ep USING INDEX idx_items_parent (parent_id=?)
  SEARCH w USING INDEX sqlite_autoindex_watch_state_1 (user_id=? AND item_id=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `authenticate_token` — `authenticate_token`

```sql
SELECT u.id, u.username, u.password_hash, u.is_admin, u.created_at,
                            t.last_seen_at,
                            (SELECT value FROM settings WHERE key = ?2),
                            (SELECT value FROM settings WHERE key = ?3),
                            (SELECT value FROM settings WHERE key = ?4),
                            unixepoch()
                     FROM users u
                     JOIN tokens t ON t.user_id = u.id
                     WHERE t.token_hash = ?1
```

Bound: `["fixture-token-1","auth.token_expiry_enabled","auth.token_idle_days","auth.token_expiry_since"]`

```text
QUERY PLAN
SEARCH t USING INDEX sqlite_autoindex_tokens_1 (token_hash=?)
SEARCH u USING INTEGER PRIMARY KEY (rowid=?)
SCALAR SUBQUERY 1
  SEARCH settings USING INDEX sqlite_autoindex_settings_1 (key=?)
SCALAR SUBQUERY 2
  SEARCH settings USING INDEX sqlite_autoindex_settings_1 (key=?)
SCALAR SUBQUERY 3
  SEARCH settings USING INDEX sqlite_autoindex_settings_1 (key=?)
```


## Appendix F — Hiqlite schema, before, rerun beside Appendix D


| Call | Statement | Rows | Cold median | Warm median |
|---|---|---:|---:|---:|
| `library_page_title` | `list_top_items_in_genre.count` | 1 | 3.29 ms | 1.590 ms |
| `library_page_title` | `list_top_items_in_genre.page` | 50 | 11.38 ms | 9.458 ms |
| `library_page_title_genre` | `list_top_items_in_genre.count` | 1 | 7.43 ms | 5.619 ms |
| `library_page_title_genre` | `list_top_items_in_genre.page` | 0 | 10.75 ms | 8.636 ms |
| `library_page_added` | `list_top_items_in_genre.count` | 1 | 3.10 ms | 1.640 ms |
| `library_page_added` | `list_top_items_in_genre.page` | 50 | 24.22 ms | 23.156 ms |
| `library_page_year` | `list_top_items_in_genre.count` | 1 | 3.44 ms | 1.890 ms |
| `library_page_year` | `list_top_items_in_genre.page` | 50 | 28.38 ms | 26.207 ms |
| `library_page_resolution` | `list_top_items_in_genre.count` | 1 | 3.34 ms | 1.963 ms |
| `library_page_resolution` | `list_top_items_in_genre.page` | 50 | 55.43 ms | 50.909 ms |
| `shows_page_title` | `list_top_items_in_genre.count` | 1 | 8.74 ms | 7.216 ms |
| `shows_page_title` | `list_top_items_in_genre.page` | 50 | 9.65 ms | 7.996 ms |
| `home_page_recorded` | `list_top_items_in_genre.count` | 1 | 1.74 ms | 0.379 ms |
| `home_page_recorded` | `list_top_items_in_genre.page` | 50 | 1.93 ms | 0.508 ms |
| `home_preview_pages` | `home_preview_pages` | 96 | 31.60 ms | 31.855 ms |
| `recently_added_all` | `recently_added` | 24 | 176.67 ms | 175.877 ms |
| `recently_added_shows` | `recently_added` | 24 | 140.41 ms | 113.376 ms |
| `search` | `search_items` | 10 | 2.57 ms | 0.994 ms |
| `item_by_external_id` | `item_by_external_id` | 1 | 5.56 ms | 4.177 ms |
| `files_for_item` | `files_for_item` | 2 | 1.32 ms | 0.025 ms |
| `item_media_facts` | `item_media_facts` | 50 | 1.68 ms | 0.270 ms |
| `watch_map` | `watch_map` | 5 | 20.95 ms | 20.537 ms |
| `watch_rollups` | `watch_rollups` | 24 | 4.62 ms | 3.104 ms |
| `continue_watching` | `continue_watching` | 24 | 1.52 ms | 0.102 ms |
| `next_up` | `next_up` | 0 | 13.25 ms | 12.280 ms |
| `authenticate_token` | `authenticate_token` | 1 | 1.49 ms | 0.012 ms |

Per call (sums of the statement medians above):

| Call | Statements | Cold | Warm |
|---|---:|---:|---:|
| `library_page_title` | 2 | 14.67 ms | 11.048 ms |
| `library_page_title_genre` | 2 | 18.18 ms | 14.254 ms |
| `library_page_added` | 2 | 27.32 ms | 24.796 ms |
| `library_page_year` | 2 | 31.82 ms | 28.097 ms |
| `library_page_resolution` | 2 | 58.77 ms | 52.872 ms |
| `shows_page_title` | 2 | 18.39 ms | 15.212 ms |
| `home_page_recorded` | 2 | 3.67 ms | 0.887 ms |
| `home_preview_pages` | 1 | 31.60 ms | 31.855 ms |
| `recently_added_all` | 1 | 176.67 ms | 175.877 ms |
| `recently_added_shows` | 1 | 140.41 ms | 113.376 ms |
| `search` | 1 | 2.57 ms | 0.994 ms |
| `item_by_external_id` | 1 | 5.56 ms | 4.177 ms |
| `files_for_item` | 1 | 1.32 ms | 0.025 ms |
| `item_media_facts` | 1 | 1.68 ms | 0.270 ms |
| `watch_map` | 1 | 20.95 ms | 20.537 ms |
| `watch_rollups` | 1 | 4.62 ms | 3.104 ms |
| `continue_watching` | 1 | 1.52 ms | 0.102 ms |
| `next_up` | 1 | 13.25 ms | 12.280 ms |
| `authenticate_token` | 1 | 1.49 ms | 0.012 ms |

#### `library_page_title` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_title` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE)) ORDER BY sort_title ASC, id ASC LIMIT $3 OFFSET $4
```

Bound: `[1,null,50,10000]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_title_genre` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[1,"Drama"]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_title_genre` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE)) ORDER BY sort_title ASC, id ASC LIMIT $3 OFFSET $4
```

Bound: `[1,"Drama",50,10000]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_added` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_added` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE)) ORDER BY added_at DESC, id DESC LIMIT $3 OFFSET $4
```

Bound: `[1,null,50,10000]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_year` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_year` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE)) ORDER BY year IS NULL, year DESC, sort_title ASC, id ASC LIMIT $3 OFFSET $4
```

Bound: `[1,null,50,10000]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `library_page_resolution` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_resolution` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE)) ORDER BY CASE WHEN kind IN ('movie','video') THEN COALESCE((SELECT MAX(f.height) FROM files f WHERE f.item_id = items.id), -1) ELSE -1 END DESC, sort_title ASC, id ASC LIMIT $3 OFFSET $4
```

Bound: `[1,null,50,10000]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
CORRELATED SCALAR SUBQUERY 2
  SEARCH f USING INDEX idx_files_item (item_id=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `shows_page_title` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[2,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `shows_page_title` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE)) ORDER BY sort_title ASC, id ASC LIMIT $3 OFFSET $4
```

Bound: `[2,null,50,200]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `home_page_recorded` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[3,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `home_page_recorded` — `list_top_items_in_genre.page`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE)) ORDER BY (recorded_at IS NULL), recorded_at DESC, sort_title ASC, id ASC LIMIT $3 OFFSET $4
```

Bound: `[3,null,50,0]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

#### `home_preview_pages` — `home_preview_pages`

```sql
WITH ranked AS (
                 SELECT id, library_id,
                        COUNT(*) OVER (PARTITION BY library_id) AS library_total,
                        ROW_NUMBER() OVER (
                            PARTITION BY library_id
                            ORDER BY added_at DESC, id DESC
                        ) AS preview_rank
                   FROM items
                  WHERE (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL))
             ), selected AS (
                 SELECT id, library_id, library_total, preview_rank
                   FROM ranked
                  WHERE preview_rank <= $1
             )
             SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, selected.library_total
               FROM selected
               JOIN items i ON i.id = selected.id
              ORDER BY selected.library_id, selected.preview_rank
```

Bound: `[24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-4)
    CO-ROUTINE (subquery-5)
      SCAN items USING INDEX idx_items_library_kind
      USE TEMP B-TREE FOR LAST 2 TERMS OF ORDER BY
    SCAN (subquery-5)
  SCAN (subquery-4)
SCAN ranked
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_all` — `recently_added`

```sql
WITH ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT $2
```

Bound: `[null,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-3)
    SCAN i
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-3)
SCAN r
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_shows` — `recently_added`

```sql
WITH ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT $2
```

Bound: `[2,24]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-3)
    SCAN i
    SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
    USE TEMP B-TREE FOR ORDER BY
  SCAN (subquery-3)
SCAN r
USE TEMP B-TREE FOR ORDER BY
```

#### `search` — `search_items`

```sql
WITH hits AS MATERIALIZED (SELECT rowid,rank AS score FROM items_fts WHERE items_fts MATCH $1 AND rowid NOT IN (SELECT rowid FROM classification_fts) UNION ALL SELECT rowid,rank AS score FROM classification_fts WHERE classification_fts MATCH $1) SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster FROM (SELECT rowid,min(score) AS score FROM hits GROUP BY rowid) f JOIN items i ON i.id = f.rowid LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','show','episode','folder','video','photo','book','audiobook') ORDER BY f.score, i.id LIMIT $2
```

Bound: `["(\"movie\") AND (\"0123\"*)",50]`

```text
QUERY PLAN
CO-ROUTINE f
  MATERIALIZE hits
    COMPOUND QUERY
      LEFT-MOST SUBQUERY
        SCAN items_fts VIRTUAL TABLE INDEX 0:M3
        LIST SUBQUERY 1
          SCAN classification_fts VIRTUAL TABLE INDEX 0:
          CREATE BLOOM FILTER
      UNION ALL
        SCAN classification_fts VIRTUAL TABLE INDEX 0:M1
  SCAN hits
  USE TEMP B-TREE FOR GROUP BY
SCAN f
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
USE TEMP B-TREE FOR ORDER BY
```

#### `item_by_external_id` — `item_by_external_id`

```sql
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE kind = $1 AND (($2 IS NOT NULL AND tmdb_id = $2) OR ($3 IS NOT NULL AND imdb_id = $3 COLLATE NOCASE)) ORDER BY ($2 IS NOT NULL AND tmdb_id = $2) DESC, id LIMIT 1
```

Bound: `["movie",1012345,null]`

```text
QUERY PLAN
SCAN items USING INDEX idx_items_library_kind
USE TEMP B-TREE FOR ORDER BY
```

#### `files_for_item` — `files_for_item`

```sql
SELECT id, item_id, path, size, mtime, duration_ms, container, video_codec, video_profile, width, height, bit_depth, hdr, bitrate, audio_streams, subtitle_streams, scanned_at, hdr_format, audio_offset_ms, dv_profile, dv_level, dv_bl_compat_id, dv_el_present, dv_rpu_present, (probe_json IS NOT NULL) AS probed, video_codec_tag, field_order, max_cll, max_fall, mastering_max_luminance, luminance_source, downloaded_subtitles FROM files WHERE item_id = $1 ORDER BY height DESC, bitrate DESC, path
```

Bound: `[12345]`

```text
QUERY PLAN
SEARCH files USING INDEX idx_files_item (item_id=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `item_media_facts` — `item_media_facts`

```sql
WITH ranked AS ( SELECT item_id, COUNT(*) OVER (PARTITION BY item_id) AS files, SUM(size) OVER (PARTITION BY item_id) AS bytes, ROW_NUMBER() OVER (PARTITION BY item_id ORDER BY COALESCE(height, 0) DESC, COALESCE(bitrate, 0) DESC, size DESC, id ASC) AS pick, container, video_codec, height, hdr, hdr_format, audio_streams FROM files WHERE item_id IN (SELECT value FROM json_each($1)) ) SELECT item_id, files, bytes, container, video_codec, height, hdr, hdr_format, audio_streams FROM ranked WHERE pick = 1
```

Bound: `["[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50]"]`

```text
QUERY PLAN
CO-ROUTINE ranked
  CO-ROUTINE (subquery-4)
    CO-ROUTINE (subquery-5)
      SEARCH files USING INDEX idx_files_item (item_id=?)
      LIST SUBQUERY 1
        SCAN json_each VIRTUAL TABLE INDEX 1:
      USE TEMP B-TREE FOR LAST 4 TERMS OF ORDER BY
    SCAN (subquery-5)
  SCAN (subquery-4)
SCAN ranked
```

#### `watch_map` — `watch_map`

```sql
SELECT w.item_id, w.position_ms, w.duration_ms, w.watched, w.updated_at FROM watch_state w JOIN json_each($1) j ON j.value = w.item_id WHERE w.user_id = $2
```

Bound: `["[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50]",1]`

```text
QUERY PLAN
SEARCH w USING INDEX idx_watch_updated (user_id=?)
SCAN j VIRTUAL TABLE INDEX 1:
```

#### `watch_rollups` — `watch_rollups`

```sql
WITH RECURSIVE tree(root, id) AS ( SELECT id, id FROM items WHERE id IN (SELECT value FROM json_each($1)) UNION SELECT t.root, i.id FROM items i JOIN tree t ON i.parent_id = t.id ) SELECT t.root AS root, COUNT(*) AS leaves, COALESCE(SUM(w.watched), 0) AS watched FROM tree t JOIN items i ON i.id = t.id LEFT JOIN watch_state w ON w.item_id = i.id AND w.user_id = $2 WHERE i.kind IN ('movie','episode','video','audiobook') GROUP BY t.root
```

Bound: `["[20001,20106,20211,20316,20421,20526,20631,20736,20841,20946,21051,21156,21261,21366,21471,21576,21681,21786,21891,21996,22101,22206,22311,22416]",1]`

```text
QUERY PLAN
CO-ROUTINE tree
  SETUP
    SEARCH items USING INTEGER PRIMARY KEY (rowid=?)
    LIST SUBQUERY 1
      SCAN json_each VIRTUAL TABLE INDEX 1:
  RECURSIVE STEP
    SCAN t
    SEARCH i USING COVERING INDEX idx_items_parent (parent_id=?)
SCAN t
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
SEARCH w USING INDEX sqlite_autoindex_watch_state_1 (user_id=? AND item_id=?) LEFT-JOIN
USE TEMP B-TREE FOR GROUP BY
```

#### `continue_watching` — `continue_watching`

```sql
SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, w.position_ms AS watch_position_ms, w.duration_ms AS watch_duration_ms, w.watched AS watch_watched, w.updated_at AS watch_updated_at FROM watch_state w JOIN items i ON i.id = w.item_id LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE w.user_id = $1 AND w.watched = 0 AND w.position_ms > 0 AND i.kind IN ('movie','episode','video','audiobook') ORDER BY w.updated_at DESC LIMIT $2
```

Bound: `[1,24]`

```text
QUERY PLAN
SEARCH w USING INDEX idx_watch_updated (user_id=?)
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
SEARCH season USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
SEARCH show USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
```

#### `next_up` — `next_up`

```sql
SELECT e.id, e.library_id, e.kind, e.parent_id, e.title, e.sort_title, e.year, e.overview, e.tmdb_id, e.imdb_id, e.season_number, e.episode_number, e.air_date, e.runtime_ms, e.poster_path, e.backdrop_path, e.added_at, e.updated_at, e.recorded_at, e.tags, e.nfo_seeded_at, e.artwork_attempted_at, e.artwork_error, e.genres, e.author, e.book_work_id, e.book_edition_id, e.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, MIN(season.season_number*100000 + e.episode_number) AS ord FROM items e JOIN items season ON season.id = e.parent_id JOIN items show ON show.id = season.parent_id WHERE e.kind = 'episode' AND e.id NOT IN (SELECT item_id FROM watch_state WHERE user_id = $1 AND (watched = 1 OR position_ms > 0)) AND (season.season_number*100000 + e.episode_number) > ( SELECT COALESCE(MAX(se.season_number*100000 + ep.episode_number), -1) FROM watch_state w JOIN items ep ON ep.id = w.item_id AND ep.kind = 'episode' JOIN items se ON se.id = ep.parent_id WHERE w.user_id = $1 AND w.watched = 1 AND se.parent_id = show.id) AND show.id IN (SELECT sh.id FROM watch_state w JOIN items ep ON ep.id = w.item_id AND ep.kind = 'episode' JOIN items se ON se.id = ep.parent_id JOIN items sh ON sh.id = se.parent_id WHERE w.user_id = $1 AND w.watched = 1) AND show.id NOT IN (SELECT sh.id FROM watch_state w JOIN items ep ON ep.id = w.item_id AND ep.kind = 'episode' JOIN items se ON se.id = ep.parent_id JOIN items sh ON sh.id = se.parent_id WHERE w.user_id = $1 AND w.watched = 0 AND w.position_ms > 0) GROUP BY show.id ORDER BY show.sort_title LIMIT $2
```

Bound: `[1,24]`

```text
QUERY PLAN
SEARCH show USING INTEGER PRIMARY KEY (rowid=?)
LIST SUBQUERY 3
  SEARCH w USING INDEX idx_watch_updated (user_id=?)
  SEARCH ep USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH se USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH sh USING INTEGER PRIMARY KEY (rowid=?)
LIST SUBQUERY 4
  SEARCH w USING INDEX idx_watch_updated (user_id=?)
  SEARCH ep USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH se USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH sh USING INTEGER PRIMARY KEY (rowid=?)
  CREATE BLOOM FILTER
SEARCH season USING INDEX idx_items_parent (parent_id=?)
SEARCH e USING INDEX idx_items_parent (parent_id=?)
LIST SUBQUERY 1
  SEARCH watch_state USING INDEX idx_watch_updated (user_id=?)
  CREATE BLOOM FILTER
CORRELATED SCALAR SUBQUERY 2
  SEARCH w USING INDEX idx_watch_updated (user_id=?)
  SEARCH ep USING INTEGER PRIMARY KEY (rowid=?)
  SEARCH se USING INTEGER PRIMARY KEY (rowid=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `authenticate_token` — `authenticate_token`

```sql
SELECT u.id, u.username, u.password_hash, u.is_admin, u.created_at, t.last_seen_at, (SELECT value FROM settings WHERE key = $1) AS expiry_enabled, (SELECT value FROM settings WHERE key = $2) AS expiry_idle_days, (SELECT value FROM settings WHERE key = $3) AS expiry_since FROM users u JOIN tokens t ON t.user_id = u.id WHERE t.token_hash = $4
```

Bound: `["auth.token_expiry_enabled","auth.token_idle_days","auth.token_expiry_since","fixture-token-1"]`

```text
QUERY PLAN
SEARCH t USING INDEX sqlite_autoindex_tokens_1 (token_hash=?)
SEARCH u USING INTEGER PRIMARY KEY (rowid=?)
SCALAR SUBQUERY 1
  SEARCH settings USING INDEX sqlite_autoindex_settings_1 (key=?)
SCALAR SUBQUERY 2
  SEARCH settings USING INDEX sqlite_autoindex_settings_1 (key=?)
SCALAR SUBQUERY 3
  SEARCH settings USING INDEX sqlite_autoindex_settings_1 (key=?)
```

