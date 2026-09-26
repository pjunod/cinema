# K-05 query-plan evidence — M5 indexes, before and after

**Status:** M5's adopted indexes with before/after plans and cold/warm medians
on both schemas, with and without the statistics Hiqlite voters collect; the
candidates measured and rejected; the migration's build time · **Tool:**
`query_plans` from `012d8a3a3`/`27f729521` (unchanged), run from this branch
at `f782fc24f` for "after" · **Base:** `main@71d1c1ecd` · **Captured:**
2026-09-26 UTC on nuc3 (16 cores, bundled SQLite 3.53.2; the host was shared with
other agents' builds, load average 3.6-17) · **Plan:**
[SQLITE-READ-PATH-AND-QUERY-PLANS.md](../../docs/cluster/SQLITE-READ-PATH-AND-QUERY-PLANS.md)
§3.4, §5.6 · **M0 evidence:** [query-plans-012d8a3a.md](query-plans-012d8a3a.md)

This is lab evidence on the M0 fixture (75,600 items: 20,000 movies, 500
shows × 8 seasons × 12 episodes, 2,100 home items, 1,000 recordings) and on a
read-only copy of one fleet voter's Raft snapshot for plan shape only. It is
not fleet evidence; the post-merge steps are in the plan's execution log.

## How it was taken

Exactly as in the M0 evidence (`catalogue_fixture`, `capture-sqlite`, the
ignored `k05_capture_hiqlite_statements` contract test, `build-hiqlite`,
`measure`), with two additions:

- **"Hiqlite + stats".** The vendored Hiqlite state machine runs
  `PRAGMA optimize=0x10002` on every connection it opens and
  `PRAGMA optimize` after each snapshot, each migration and at shutdown
  (`vendor/hiqlite/src/store/state_machine/sqlite/state_machine.rs:593`,
  `writer.rs:496-706`). So a voter that has restarted or snapshotted
  since its tables filled **has** `sqlite_stat1` for `items`, unlike the M0
  assumption ("no statistics on either database"). A read-only copy of the
  nuc3 voter's current snapshot (schema 47, 6,871 items) confirms it: its
  `sqlite_stat1` has rows for every `items` and `watch_state` index. The
  "Hiqlite + stats" rows below are the Hiqlite fixture after running that same
  `PRAGMA optimize=0x10002` (23 `sqlite_stat1` rows; `analysis_limit` 400, as
  the pragma applies it), and again after the new indexes.
- **"before (rerun)"** is the same fixture with no M5 index, measured after the
  candidate runs, to show how far host load moved the medians between runs.
  Differences under ~15 % between runs are noise.

"Before" is `main@71d1c1ecd` (the `item_by_external_id` statement as it was);
"after" is this branch: SQLite fixture migrated v69 → v70 by opening it with
the branch's `SqliteStore`, and a Hiqlite fixture whose schema the branch's
three-voter bootstrap reported (v48).

## What changed, in one table

Warm medians in ms, per call (the sum of a call's statements). The 50-row
library pages are at offset 10,000 of the 20,000-movie library, as in M0.

| Call | SQLite before | SQLite before (rerun) | SQLite after | Hiqlite before | Hiqlite after | Hiqlite + stats before | Hiqlite + stats after |
|---|---:|---:|---:|---:|---:|---:|---:|
| `item_by_external_id` | 3.256 | 3.964 | **0.022** | 4.037 | **0.026** | 3.933 | **0.029** |
| `library_page_title` (count + page) | 10.494 | 11.123 | **0.782** | 9.499 | **0.671** | 11.799 | **0.799** |
| `shows_page_title` (count + page) | 12.347 | 14.170 | **0.087** | 12.698 | **0.093** | 22.175 | **0.099** |
| `library_page_title_genre` | 13.846 | 13.860 | 6.502 | 11.795 | 6.303 | 13.847 | 7.134 |
| `library_page_added` | 21.866 | 23.295 | 17.665 | 21.595 | 18.846 | 20.771 | 19.634 |
| `library_page_year` | 22.484 | 25.429 | 19.269 | 23.300 | 19.126 | 25.513 | 19.625 |
| `library_page_resolution` | 40.198 | 45.147 | 36.249 | 40.082 | 37.226 | 49.727 | 41.307 |
| `home_page_recorded` | 0.745 | 0.850 | 0.314 | 0.729 | 0.335 | 1.331 | 0.380 |
| `home_preview_pages` | 25.324 | 28.608 | 18.007 | 29.400 | 20.047 | 40.255 | 20.674 |

Per statement: the unfiltered counts behind every sort fall from 1.4-1.8 ms
to 0.41-0.52 ms (they now read the ~20,000 top-level index entries, with the
kind predicate implied by the index instead of evaluated per entry); the shows
library's count falls from 5.9 ms to 0.023 ms because it no longer visits
52,500 season and episode entries to find 500 shows. The Title page falls from
8.8 ms to 0.27 ms (no temp B-tree). Added, Year and Resolution pages still
sort (their keys are not in any index); they gain only from the smaller
candidate set. No statement outside the ones above changed plan, on either
schema, with or without statistics: the plan text of every other call is
identical before and after (the SQLite library-page SQL differs only in
indentation, from moving it into `library_page_statements`).

### Plans that changed (identical on SQLite, Hiqlite and Hiqlite + stats)

`item_by_external_id` — before (the `OR` statement, and still with the new
indexes present, which is why the statement changed):

```text
SCAN items USING INDEX idx_items_library_kind
USE TEMP B-TREE FOR ORDER BY
```

after (`kind = ? AND id IN (tmdb arm UNION ALL imdb arm)`, same `ORDER BY`):

```text
SEARCH items USING INTEGER PRIMARY KEY (rowid=?)
LIST SUBQUERY 2
  COMPOUND QUERY
    LEFT-MOST SUBQUERY
      SEARCH items USING COVERING INDEX idx_items_tmdb (tmdb_id=?)
    UNION ALL
      SEARCH items USING COVERING INDEX idx_items_imdb (imdb_id=?)
USE TEMP B-TREE FOR ORDER BY
```

(The remaining temp B-tree orders the one or two matching rows.)

`list_top_items_in_genre.page`, `ItemSort::Title` — before:

```text
SEARCH items USING INDEX idx_items_library_kind (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
USE TEMP B-TREE FOR ORDER BY
```

after:

```text
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

Every `list_top_items_in_genre.count`, and the Added/Year/Resolution/Recorded
pages: `SEARCH items USING INDEX idx_items_library_kind (library_id=?)` →
`SEARCH items USING INDEX idx_items_top_level_title (library_id=?)` (the
non-Title pages keep their `USE TEMP B-TREE FOR ORDER BY`).
`home_preview_pages`: its inner `SCAN items USING INDEX idx_items_library_kind`
becomes `SCAN items USING INDEX idx_items_top_level_title`.

**On a copy of the nuc3 voter's snapshot** (its own statistics, then
`PRAGMA optimize=0x10002` after adding the three indexes, as the voter would
at its next snapshot), the same `measure` run shows exactly these plan changes
and no others. Its timings are not reported: the calls' bindings name fixture
rows.

## Migration cost

`CREATE INDEX IF NOT EXISTS` × 3 in one transaction on the Hiqlite fixture
(75,600 items), with the state machine's pragmas (`journal_mode=WAL`,
`synchronous=OFF`), system `sqlite3` 3.46.1, five runs each:

| | total | `idx_items_tmdb` | `idx_items_imdb` | `idx_items_top_level_title` |
|---|---:|---:|---:|---:|
| cold (file evicted from the page cache) | 60.3-70.9 ms (median 63.3) | 6.1-9.2 | 3.7-4.3 | 50.2-57.0 |
| warm | 58.5-63.7 ms (median 59.1) | 5.8-7.3 | 3.6-4.4 | 48.7-51.7 |

The fixture has no IMDb ids, so `idx_items_imdb` is one page here; on the
nuc3 snapshot 455 of 6,871 items carry one. Sizes on the Hiqlite fixture
(`dbstat`): `idx_items_tmdb` 57 pages (228 KiB), `idx_items_imdb` 1 page,
`idx_items_top_level_title` 114 pages (456 KiB) — 688 KiB together, against
11.0 MiB for `items` and 1.2 MiB for `idx_items_library_kind`. The fleet
voter's catalogue (6,871 items) is 9 % of the fixture's.

On Hiqlite the migration is one Raft entry carrying the three statements and
the marker update; each voter builds the indexes when it applies that entry,
holding its state-machine writer (and so every later entry's apply, and every
consistent read waiting on it) for that time. Openraft 0.9.25 runs the state
machine in its own task (`openraft/src/core/sm/worker.rs`), so heartbeats and
elections do not wait on it. `CREATE INDEX` does not block WAL readers.

## Candidates measured and rejected

SQLite fixture, warm medians in ms, each candidate alone on top of the
before schema (runs `B`, `Bp`, `Bpp`, `BCpart` of this session; the Hiqlite
and Hiqlite + stats runs agree):

| Statement | before (rerun) | (a) `(library_id, sort_title)` | (b) `(library_id, sort_title, kind, parent_id)` | (c) `(library_id, sort_title, id, kind, parent_id)` | adopted: partial `(library_id, sort_title)` | adopted + (d) partial `(library_id, added_at DESC, id DESC)` |
|---|---:|---:|---:|---:|---:|---:|
| Title count | 1.835 | 2.503 | 2.321 | 2.521 | 0.441 | 0.858 |
| Title page | 9.288 | 1.004 | 6.032 | 1.376 | 0.334 | 0.255 |
| Title + genre count | 5.350 | 5.457 | 5.521 | 7.749 | 3.204 | 7.087 |
| Added page | 21.657 | 20.797 | 21.172 | 20.614 | 18.305 | 0.330 |
| Year page | 23.661 | 22.208 | 22.853 | 23.071 | 19.954 | 27.461 |
| Resolution page | 43.524 | 40.745 | 43.343 | 43.322 | 33.462 | 61.424 |
| Shows count | 6.829 | 10.551 | 7.160 | 8.096 | 0.019 | 0.020 |
| Shows page | 7.340 | 5.315 | 4.285 | 3.395 | 0.062 | 0.075 |
| `home_preview_pages` | 28.608 | 31.721 | 29.578 | 40.292 | 20.520 | 14.893 |

- **(a)** removes the Title sort but the planner then serves every count from
  it and seeks the table row for `kind` on each entry (+0.8 ms per count,
  +3.7 ms for the shows count): a regression on every non-Title call.
- **(b)** keeps `kind`/`parent_id` in the index, but its order is no longer
  `(sort_title, id)`, so the Title page still sorts ties (6.0 ms).
- **(c)** orders by `(sort_title, id)` and carries the predicate columns, but
  its counts still cost more than before (Title 2.5 ms, shows 8.1 ms, genre
  7.7 ms) and `home_preview_pages` regressed to 40 ms.
- **(d)** makes the Added page 66× faster, but the planner then reads the Year
  and Resolution pages through it (27.5 and 61.4 ms against 23.7 and 43.5
  before M5): a regression on two sorts to speed a third. Not adopted.
- **A plain `(library_id, added_at DESC)`** (run `all4`, taken under a load
  average of 17, so only its plans are quoted) was chosen for every count and
  made each count seek the table per entry, like (a).
- **The `OR` statement with the external-id indexes present** keeps the
  `SCAN items USING INDEX idx_items_library_kind` plan on the bundled 3.53.2,
  on both schemas and with statistics, so the statement had to change rather
  than rely on the planner (system `sqlite3` 3.46.1 does choose
  `MULTI-INDEX OR` for it on a schema-only database, which is why a CLI check
  is not evidence here).

## Statistics on Hiqlite voters (a finding, not an M5 change)

The M6 trial rejected `ANALYZE` for the standalone store because
`watch_rollups` became 3.5× slower with statistics. The Hiqlite voters already
run with statistics (above), and the Hiqlite fixture reproduces that
regression there: `watch_rollups` 2.566 ms without statistics, 7.853 ms with
the vendor's (before M5; 6.684 ms after, the index is not involved). So the
§3.7 question is not only "should plurx adopt statistics": on the replicated
backend the vendored Hiqlite already did, per voter and outside the Raft log.
Recorded in the plan's execution log for a follow-up; M5 does not change it.

## Appendix A — SQLite, after (this branch, v70)

### sqlite — `fixture.db` (SQLite 3.53.2, 5 runs each)

| Call | Statement | Rows | Cold median | Warm median |
|---|---|---:|---:|---:|
| `library_page_title` | `list_top_items_in_genre.count` | 1 | 2.63 ms | 0.512 ms |
| `library_page_title` | `list_top_items_in_genre.page` | 50 | 1.40 ms | 0.270 ms |
| `library_page_title_genre` | `list_top_items_in_genre.count` | 1 | 5.05 ms | 3.274 ms |
| `library_page_title_genre` | `list_top_items_in_genre.page` | 0 | 5.00 ms | 3.228 ms |
| `library_page_added` | `list_top_items_in_genre.count` | 1 | 1.56 ms | 0.428 ms |
| `library_page_added` | `list_top_items_in_genre.page` | 50 | 18.70 ms | 17.237 ms |
| `library_page_year` | `list_top_items_in_genre.count` | 1 | 1.57 ms | 0.430 ms |
| `library_page_year` | `list_top_items_in_genre.page` | 50 | 20.59 ms | 18.839 ms |
| `library_page_resolution` | `list_top_items_in_genre.count` | 1 | 1.63 ms | 0.441 ms |
| `library_page_resolution` | `list_top_items_in_genre.page` | 50 | 37.61 ms | 35.808 ms |
| `shows_page_title` | `list_top_items_in_genre.count` | 1 | 1.23 ms | 0.023 ms |
| `shows_page_title` | `list_top_items_in_genre.page` | 50 | 1.37 ms | 0.063 ms |
| `home_page_recorded` | `list_top_items_in_genre.count` | 1 | 1.10 ms | 0.037 ms |
| `home_page_recorded` | `list_top_items_in_genre.page` | 50 | 1.62 ms | 0.277 ms |
| `home_preview_pages` | `home_preview_pages` | 96 | 19.68 ms | 18.007 ms |
| `recently_added_all` | `recently_added` | 2 | 2.01 ms | 0.724 ms |
| `recently_added_all` | `recently_added` | 4 | 2.48 ms | 1.410 ms |
| `recently_added_all` | `recently_added` | 8 | 3.74 ms | 2.219 ms |
| `recently_added_all` | `recently_added` | 16 | 4.81 ms | 3.579 ms |
| `recently_added_all` | `recently_added` | 24 | 8.30 ms | 6.930 ms |
| `recently_added_shows` | `recently_added` | 2 | 1.79 ms | 0.680 ms |
| `recently_added_shows` | `recently_added` | 4 | 2.13 ms | 1.074 ms |
| `recently_added_shows` | `recently_added` | 8 | 2.93 ms | 1.838 ms |
| `recently_added_shows` | `recently_added` | 16 | 4.55 ms | 3.379 ms |
| `recently_added_shows` | `recently_added` | 24 | 7.89 ms | 6.597 ms |
| `search` | `search_items` | 10 | 1.77 ms | 0.530 ms |
| `item_by_external_id` | `item_by_external_id` | 1 | 1.08 ms | 0.022 ms |
| `files_for_item` | `files_for_item` | 2 | 1.07 ms | 0.023 ms |
| `item_media_facts` | `item_media_facts` | 50 | 1.34 ms | 0.222 ms |
| `watch_map` | `watch_map` | 5 | 1.07 ms | 0.013 ms |
| `watch_rollups` | `watch_rollups` | 24 | 3.50 ms | 2.227 ms |
| `continue_watching` | `continue_watching` | 24 | 1.17 ms | 0.091 ms |
| `next_up` | `next_up` | 0 | 9.82 ms | 8.204 ms |
| `authenticate_token` | `authenticate_token` | 1 | 1.04 ms | 0.011 ms |

Per call (sums of the statement medians above):

| Call | Statements | Cold | Warm |
|---|---:|---:|---:|
| `library_page_title` | 2 | 4.03 ms | 0.782 ms |
| `library_page_title_genre` | 2 | 10.05 ms | 6.502 ms |
| `library_page_added` | 2 | 20.26 ms | 17.665 ms |
| `library_page_year` | 2 | 22.16 ms | 19.269 ms |
| `library_page_resolution` | 2 | 39.24 ms | 36.249 ms |
| `shows_page_title` | 2 | 2.60 ms | 0.087 ms |
| `home_page_recorded` | 2 | 2.71 ms | 0.314 ms |
| `home_preview_pages` | 1 | 19.68 ms | 18.007 ms |
| `recently_added_all` | 5 | 21.33 ms | 14.861 ms |
| `recently_added_shows` | 5 | 19.27 ms | 13.569 ms |
| `search` | 1 | 1.77 ms | 0.530 ms |
| `item_by_external_id` | 1 | 1.08 ms | 0.022 ms |
| `files_for_item` | 1 | 1.07 ms | 0.023 ms |
| `item_media_facts` | 1 | 1.34 ms | 0.222 ms |
| `watch_map` | 1 | 1.07 ms | 0.013 ms |
| `watch_rollups` | 1 | 3.50 ms | 2.227 ms |
| `continue_watching` | 1 | 1.17 ms | 0.091 ms |
| `next_up` | 1 | 9.82 ms | 8.204 ms |
| `authenticate_token` | 1 | 1.04 ms | 0.011 ms |

#### `library_page_title` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_title_genre` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[1,"Drama"]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_added` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `home_page_recorded` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) FROM items WHERE library_id = ?1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND (?2 IS NULL OR EXISTS ( SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))
```

Bound: `[3,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
      SCAN items USING INDEX idx_items_top_level_title
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
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE kind = ?1 AND id IN ( SELECT id FROM items WHERE ?2 IS NOT NULL AND tmdb_id = ?2 UNION ALL SELECT id FROM items WHERE ?3 IS NOT NULL AND imdb_id = ?3 COLLATE NOCASE) ORDER BY (?2 IS NOT NULL AND tmdb_id = ?2) DESC, id LIMIT 1
```

Bound: `["movie",1012345,null]`

```text
QUERY PLAN
SEARCH items USING INTEGER PRIMARY KEY (rowid=?)
LIST SUBQUERY 2
  COMPOUND QUERY
    LEFT-MOST SUBQUERY
      SEARCH items USING COVERING INDEX idx_items_tmdb (tmdb_id=?)
    UNION ALL
      SEARCH items USING COVERING INDEX idx_items_imdb (imdb_id=?)
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


## Appendix B — Hiqlite schema, after (this branch, v48), no statistics

### hiqlite — `hiqlite-fixture.db` (SQLite 3.53.2, 5 runs each)

| Call | Statement | Rows | Cold median | Warm median |
|---|---|---:|---:|---:|
| `library_page_title` | `list_top_items_in_genre.count` | 1 | 2.50 ms | 0.414 ms |
| `library_page_title` | `list_top_items_in_genre.page` | 50 | 1.31 ms | 0.257 ms |
| `library_page_title_genre` | `list_top_items_in_genre.count` | 1 | 4.73 ms | 3.119 ms |
| `library_page_title_genre` | `list_top_items_in_genre.page` | 0 | 4.76 ms | 3.185 ms |
| `library_page_added` | `list_top_items_in_genre.count` | 1 | 1.51 ms | 0.443 ms |
| `library_page_added` | `list_top_items_in_genre.page` | 50 | 19.66 ms | 18.402 ms |
| `library_page_year` | `list_top_items_in_genre.count` | 1 | 1.61 ms | 0.416 ms |
| `library_page_year` | `list_top_items_in_genre.page` | 50 | 20.79 ms | 18.710 ms |
| `library_page_resolution` | `list_top_items_in_genre.count` | 1 | 1.79 ms | 0.503 ms |
| `library_page_resolution` | `list_top_items_in_genre.page` | 50 | 38.26 ms | 36.722 ms |
| `shows_page_title` | `list_top_items_in_genre.count` | 1 | 1.09 ms | 0.018 ms |
| `shows_page_title` | `list_top_items_in_genre.page` | 50 | 1.19 ms | 0.075 ms |
| `home_page_recorded` | `list_top_items_in_genre.count` | 1 | 1.05 ms | 0.038 ms |
| `home_page_recorded` | `list_top_items_in_genre.page` | 50 | 1.43 ms | 0.298 ms |
| `home_preview_pages` | `home_preview_pages` | 96 | 19.66 ms | 20.047 ms |
| `recently_added_all` | `recently_added` | 2 | 2.10 ms | 0.901 ms |
| `recently_added_shows` | `recently_added` | 2 | 2.10 ms | 0.810 ms |
| `search` | `search_items` | 10 | 1.95 ms | 0.583 ms |
| `item_by_external_id` | `item_by_external_id` | 1 | 1.20 ms | 0.026 ms |
| `files_for_item` | `files_for_item` | 2 | 1.20 ms | 0.023 ms |
| `item_media_facts` | `item_media_facts` | 50 | 1.55 ms | 0.294 ms |
| `watch_map` | `watch_map` | 5 | 1.25 ms | 0.017 ms |
| `watch_rollups` | `watch_rollups` | 24 | 4.13 ms | 2.752 ms |
| `continue_watching` | `continue_watching` | 24 | 1.38 ms | 0.103 ms |
| `next_up` | `next_up` | 0 | 10.43 ms | 8.984 ms |
| `authenticate_token` | `authenticate_token` | 1 | 1.19 ms | 0.012 ms |

Per call (sums of the statement medians above):

| Call | Statements | Cold | Warm |
|---|---:|---:|---:|
| `library_page_title` | 2 | 3.81 ms | 0.671 ms |
| `library_page_title_genre` | 2 | 9.49 ms | 6.303 ms |
| `library_page_added` | 2 | 21.17 ms | 18.846 ms |
| `library_page_year` | 2 | 22.40 ms | 19.126 ms |
| `library_page_resolution` | 2 | 40.05 ms | 37.226 ms |
| `shows_page_title` | 2 | 2.27 ms | 0.093 ms |
| `home_page_recorded` | 2 | 2.49 ms | 0.335 ms |
| `home_preview_pages` | 1 | 19.66 ms | 20.047 ms |
| `recently_added_all` | 1 | 2.10 ms | 0.901 ms |
| `recently_added_shows` | 1 | 2.10 ms | 0.810 ms |
| `search` | 1 | 1.95 ms | 0.583 ms |
| `item_by_external_id` | 1 | 1.20 ms | 0.026 ms |
| `files_for_item` | 1 | 1.20 ms | 0.023 ms |
| `item_media_facts` | 1 | 1.55 ms | 0.294 ms |
| `watch_map` | 1 | 1.25 ms | 0.017 ms |
| `watch_rollups` | 1 | 4.13 ms | 2.752 ms |
| `continue_watching` | 1 | 1.38 ms | 0.103 ms |
| `next_up` | 1 | 10.43 ms | 8.984 ms |
| `authenticate_token` | 1 | 1.19 ms | 0.012 ms |

#### `library_page_title` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_title_genre` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[1,"Drama"]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_added` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `home_page_recorded` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[3,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
      SCAN items USING INDEX idx_items_top_level_title
      USE TEMP B-TREE FOR LAST 2 TERMS OF ORDER BY
    SCAN (subquery-5)
  SCAN (subquery-4)
SCAN ranked
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_all` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) AND ($1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) ORDER BY i.added_at DESC LIMIT 1 OFFSET $2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) AND ($1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT $3
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
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) AND ($1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) ORDER BY i.added_at DESC LIMIT 1 OFFSET $2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) AND ($1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT $3
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
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE kind = $1 AND id IN ( SELECT id FROM items WHERE $2 IS NOT NULL AND tmdb_id = $2 UNION ALL SELECT id FROM items WHERE $3 IS NOT NULL AND imdb_id = $3 COLLATE NOCASE) ORDER BY ($2 IS NOT NULL AND tmdb_id = $2) DESC, id LIMIT 1
```

Bound: `["movie",1012345,null]`

```text
QUERY PLAN
SEARCH items USING INTEGER PRIMARY KEY (rowid=?)
LIST SUBQUERY 2
  COMPOUND QUERY
    LEFT-MOST SUBQUERY
      SEARCH items USING COVERING INDEX idx_items_tmdb (tmdb_id=?)
    UNION ALL
      SEARCH items USING COVERING INDEX idx_items_imdb (imdb_id=?)
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


## Appendix C — Hiqlite schema, before, with the vendor's statistics

### hiqlite — `hiqlite-stats.db` (SQLite 3.53.2, 5 runs each)

| Call | Statement | Rows | Cold median | Warm median |
|---|---|---:|---:|---:|
| `library_page_title` | `list_top_items_in_genre.count` | 1 | 4.29 ms | 2.444 ms |
| `library_page_title` | `list_top_items_in_genre.page` | 50 | 11.57 ms | 9.355 ms |
| `library_page_title_genre` | `list_top_items_in_genre.count` | 1 | 6.86 ms | 5.552 ms |
| `library_page_title_genre` | `list_top_items_in_genre.page` | 0 | 10.47 ms | 8.295 ms |
| `library_page_added` | `list_top_items_in_genre.count` | 1 | 3.14 ms | 1.594 ms |
| `library_page_added` | `list_top_items_in_genre.page` | 50 | 24.40 ms | 19.177 ms |
| `library_page_year` | `list_top_items_in_genre.count` | 1 | 3.10 ms | 1.646 ms |
| `library_page_year` | `list_top_items_in_genre.page` | 50 | 24.89 ms | 23.867 ms |
| `library_page_resolution` | `list_top_items_in_genre.count` | 1 | 3.13 ms | 1.751 ms |
| `library_page_resolution` | `list_top_items_in_genre.page` | 50 | 48.80 ms | 47.976 ms |
| `shows_page_title` | `list_top_items_in_genre.count` | 1 | 12.96 ms | 10.845 ms |
| `shows_page_title` | `list_top_items_in_genre.page` | 50 | 15.50 ms | 11.330 ms |
| `home_page_recorded` | `list_top_items_in_genre.count` | 1 | 2.30 ms | 0.501 ms |
| `home_page_recorded` | `list_top_items_in_genre.page` | 50 | 2.98 ms | 0.830 ms |
| `home_preview_pages` | `home_preview_pages` | 96 | 44.81 ms | 40.255 ms |
| `recently_added_all` | `recently_added` | 2 | 2.20 ms | 0.909 ms |
| `recently_added_shows` | `recently_added` | 2 | 2.12 ms | 0.815 ms |
| `search` | `search_items` | 10 | 2.01 ms | 0.552 ms |
| `item_by_external_id` | `item_by_external_id` | 1 | 5.06 ms | 3.933 ms |
| `files_for_item` | `files_for_item` | 2 | 1.25 ms | 0.024 ms |
| `item_media_facts` | `item_media_facts` | 50 | 1.56 ms | 0.255 ms |
| `watch_map` | `watch_map` | 5 | 1.24 ms | 0.016 ms |
| `watch_rollups` | `watch_rollups` | 24 | 9.31 ms | 7.853 ms |
| `continue_watching` | `continue_watching` | 24 | 1.37 ms | 0.093 ms |
| `next_up` | `next_up` | 0 | 10.62 ms | 9.302 ms |
| `authenticate_token` | `authenticate_token` | 1 | 1.28 ms | 0.011 ms |

Per call (sums of the statement medians above):

| Call | Statements | Cold | Warm |
|---|---:|---:|---:|
| `library_page_title` | 2 | 15.86 ms | 11.799 ms |
| `library_page_title_genre` | 2 | 17.33 ms | 13.847 ms |
| `library_page_added` | 2 | 27.54 ms | 20.771 ms |
| `library_page_year` | 2 | 27.99 ms | 25.513 ms |
| `library_page_resolution` | 2 | 51.94 ms | 49.727 ms |
| `shows_page_title` | 2 | 28.46 ms | 22.175 ms |
| `home_page_recorded` | 2 | 5.28 ms | 1.331 ms |
| `home_preview_pages` | 1 | 44.81 ms | 40.255 ms |
| `recently_added_all` | 1 | 2.20 ms | 0.909 ms |
| `recently_added_shows` | 1 | 2.12 ms | 0.815 ms |
| `search` | 1 | 2.01 ms | 0.552 ms |
| `item_by_external_id` | 1 | 5.06 ms | 3.933 ms |
| `files_for_item` | 1 | 1.25 ms | 0.024 ms |
| `item_media_facts` | 1 | 1.56 ms | 0.255 ms |
| `watch_map` | 1 | 1.24 ms | 0.016 ms |
| `watch_rollups` | 1 | 9.31 ms | 7.853 ms |
| `continue_watching` | 1 | 1.37 ms | 0.093 ms |
| `next_up` | 1 | 10.62 ms | 9.302 ms |
| `authenticate_token` | 1 | 1.28 ms | 0.011 ms |

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
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) AND ($1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) ORDER BY i.added_at DESC LIMIT 1 OFFSET $2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) AND ($1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT $3
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
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) AND ($1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) ORDER BY i.added_at DESC LIMIT 1 OFFSET $2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) AND ($1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT $3
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
SCAN i USING COVERING INDEX idx_items_library_kind
BLOOM FILTER ON t (id=?)
SEARCH t USING AUTOMATIC COVERING INDEX (id=?)
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
  SEARCH se USING INDEX idx_items_parent (parent_id=?)
  SEARCH ep USING INDEX idx_items_parent (parent_id=?)
  SEARCH w USING INDEX sqlite_autoindex_watch_state_1 (user_id=? AND item_id=?)
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


## Appendix D — Hiqlite schema, after, with the vendor's statistics

### hiqlite — `hiqlite-stats.db` (SQLite 3.53.2, 5 runs each)

| Call | Statement | Rows | Cold median | Warm median |
|---|---|---:|---:|---:|
| `library_page_title` | `list_top_items_in_genre.count` | 1 | 2.36 ms | 0.502 ms |
| `library_page_title` | `list_top_items_in_genre.page` | 50 | 1.62 ms | 0.297 ms |
| `library_page_title_genre` | `list_top_items_in_genre.count` | 1 | 4.95 ms | 3.393 ms |
| `library_page_title_genre` | `list_top_items_in_genre.page` | 0 | 5.56 ms | 3.741 ms |
| `library_page_added` | `list_top_items_in_genre.count` | 1 | 1.82 ms | 0.517 ms |
| `library_page_added` | `list_top_items_in_genre.page` | 50 | 20.35 ms | 19.117 ms |
| `library_page_year` | `list_top_items_in_genre.count` | 1 | 1.80 ms | 0.521 ms |
| `library_page_year` | `list_top_items_in_genre.page` | 50 | 22.89 ms | 19.104 ms |
| `library_page_resolution` | `list_top_items_in_genre.count` | 1 | 1.86 ms | 0.459 ms |
| `library_page_resolution` | `list_top_items_in_genre.page` | 50 | 42.85 ms | 40.848 ms |
| `shows_page_title` | `list_top_items_in_genre.count` | 1 | 1.24 ms | 0.023 ms |
| `shows_page_title` | `list_top_items_in_genre.page` | 50 | 1.35 ms | 0.076 ms |
| `home_page_recorded` | `list_top_items_in_genre.count` | 1 | 1.26 ms | 0.038 ms |
| `home_page_recorded` | `list_top_items_in_genre.page` | 50 | 1.61 ms | 0.342 ms |
| `home_preview_pages` | `home_preview_pages` | 96 | 22.48 ms | 20.674 ms |
| `recently_added_all` | `recently_added` | 2 | 2.22 ms | 0.882 ms |
| `recently_added_shows` | `recently_added` | 2 | 2.17 ms | 0.872 ms |
| `search` | `search_items` | 10 | 2.12 ms | 0.611 ms |
| `item_by_external_id` | `item_by_external_id` | 1 | 1.29 ms | 0.029 ms |
| `files_for_item` | `files_for_item` | 2 | 1.28 ms | 0.024 ms |
| `item_media_facts` | `item_media_facts` | 50 | 1.61 ms | 0.272 ms |
| `watch_map` | `watch_map` | 5 | 1.26 ms | 0.017 ms |
| `watch_rollups` | `watch_rollups` | 24 | 9.39 ms | 6.684 ms |
| `continue_watching` | `continue_watching` | 24 | 1.13 ms | 0.079 ms |
| `next_up` | `next_up` | 0 | 8.72 ms | 7.433 ms |
| `authenticate_token` | `authenticate_token` | 1 | 1.02 ms | 0.009 ms |

Per call (sums of the statement medians above):

| Call | Statements | Cold | Warm |
|---|---:|---:|---:|
| `library_page_title` | 2 | 3.97 ms | 0.799 ms |
| `library_page_title_genre` | 2 | 10.51 ms | 7.134 ms |
| `library_page_added` | 2 | 22.17 ms | 19.634 ms |
| `library_page_year` | 2 | 24.69 ms | 19.625 ms |
| `library_page_resolution` | 2 | 44.71 ms | 41.307 ms |
| `shows_page_title` | 2 | 2.59 ms | 0.099 ms |
| `home_page_recorded` | 2 | 2.87 ms | 0.380 ms |
| `home_preview_pages` | 1 | 22.48 ms | 20.674 ms |
| `recently_added_all` | 1 | 2.22 ms | 0.882 ms |
| `recently_added_shows` | 1 | 2.17 ms | 0.872 ms |
| `search` | 1 | 2.12 ms | 0.611 ms |
| `item_by_external_id` | 1 | 1.29 ms | 0.029 ms |
| `files_for_item` | 1 | 1.28 ms | 0.024 ms |
| `item_media_facts` | 1 | 1.61 ms | 0.272 ms |
| `watch_map` | 1 | 1.26 ms | 0.017 ms |
| `watch_rollups` | 1 | 9.39 ms | 6.684 ms |
| `continue_watching` | 1 | 1.13 ms | 0.079 ms |
| `next_up` | 1 | 8.72 ms | 7.433 ms |
| `authenticate_token` | 1 | 1.02 ms | 0.009 ms |

#### `library_page_title` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_title_genre` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[1,"Drama"]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `library_page_added` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[1,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
CORRELATED SCALAR SUBQUERY 1
  SCAN json_each VIRTUAL TABLE INDEX 1:
```

#### `home_page_recorded` — `list_top_items_in_genre.count`

```sql
SELECT COUNT(*) AS count FROM items WHERE library_id = $1 AND (kind IN ('movie','show','book','audiobook') OR (kind IN ('folder','video','photo') AND parent_id IS NULL)) AND ($2 IS NULL OR EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = $2 COLLATE NOCASE))
```

Bound: `[3,null]`

```text
QUERY PLAN
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
SEARCH items USING INDEX idx_items_top_level_title (library_id=?)
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
      SCAN items USING INDEX idx_items_top_level_title
      USE TEMP B-TREE FOR LAST 2 TERMS OF ORDER BY
    SCAN (subquery-5)
  SCAN (subquery-4)
SCAN ranked
SEARCH i USING INTEGER PRIMARY KEY (rowid=?)
USE TEMP B-TREE FOR ORDER BY
```

#### `recently_added_all` — `recently_added`

```sql
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) AND ($1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) ORDER BY i.added_at DESC LIMIT 1 OFFSET $2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) AND ($1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT $3
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
WITH cut AS ( SELECT i.added_at FROM items i WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) AND ($1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) ORDER BY i.added_at DESC LIMIT 1 OFFSET $2 ), ranked AS ( SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.sort_title, i.year, i.overview, i.tmdb_id, i.imdb_id, i.season_number, i.episode_number, i.air_date, i.runtime_ms, i.poster_path, i.backdrop_path, i.added_at, i.updated_at, i.recorded_at, i.tags, i.nfo_seeded_at, i.artwork_attempted_at, i.artwork_error, i.genres, i.author, i.book_work_id, i.book_edition_id, i.book_metadata_source, show.title AS rail_show_title, season.poster_path AS rail_season_poster, ROW_NUMBER() OVER (PARTITION BY CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL THEN 'show:' || show.id ELSE 'item:' || i.id END ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank FROM items i LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' LEFT JOIN items show ON show.id = season.parent_id WHERE i.kind IN ('movie','episode','video','folder','book','audiobook') AND ($1 IS NULL OR i.library_id = $1) AND ($1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings')) AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) ) SELECT r.id, r.library_id, r.kind, r.parent_id, r.title, r.sort_title, r.year, r.overview, r.tmdb_id, r.imdb_id, r.season_number, r.episode_number, r.air_date, r.runtime_ms, r.poster_path, r.backdrop_path, r.added_at, r.updated_at, r.recorded_at, r.tags, r.nfo_seeded_at, r.artwork_attempted_at, r.artwork_error, r.genres, r.author, r.book_work_id, r.book_edition_id, r.book_metadata_source, r.rail_show_title, r.rail_season_poster, EXISTS (SELECT 1 FROM cut) AS rail_window_cut FROM ranked r WHERE r.rail_rank = 1 ORDER BY r.added_at DESC, r.id DESC LIMIT $3
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
SELECT id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source FROM items WHERE kind = $1 AND id IN ( SELECT id FROM items WHERE $2 IS NOT NULL AND tmdb_id = $2 UNION ALL SELECT id FROM items WHERE $3 IS NOT NULL AND imdb_id = $3 COLLATE NOCASE) ORDER BY ($2 IS NOT NULL AND tmdb_id = $2) DESC, id LIMIT 1
```

Bound: `["movie",1012345,null]`

```text
QUERY PLAN
SEARCH items USING INTEGER PRIMARY KEY (rowid=?)
LIST SUBQUERY 2
  COMPOUND QUERY
    LEFT-MOST SUBQUERY
      SEARCH items USING COVERING INDEX idx_items_tmdb (tmdb_id=?)
    UNION ALL
      SEARCH items USING COVERING INDEX idx_items_imdb (imdb_id=?)
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
SCAN i USING COVERING INDEX idx_items_library_kind
BLOOM FILTER ON t (id=?)
SEARCH t USING AUTOMATIC COVERING INDEX (id=?)
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
  SEARCH se USING INDEX idx_items_parent (parent_id=?)
  SEARCH ep USING INDEX idx_items_parent (parent_id=?)
  SEARCH w USING INDEX sqlite_autoindex_watch_state_1 (user_id=? AND item_id=?)
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
