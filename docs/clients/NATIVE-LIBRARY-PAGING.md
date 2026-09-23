# Native library paging — pages on demand, one merged order, filtering off the main thread

**Status:** ready for review · **Executes:** A5 / F-apple-5 and the
library half of D6 / F-android-10 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Companion to [APPLE-CLIENT-PARITY.md](APPLE-CLIENT-PARITY.md) and
[ANDROID-CLIENT-PARITY.md](ANDROID-CLIENT-PARITY.md) (what each native
client still lacks) and [../API.md](../API.md) (the endpoint this pages) —
this is *how both native library grids stop downloading the whole library*
without a title going missing, in five PRs: one server, two Apple, two
Android.

Read first: the A5 and D6 rows (§3.6, §3.7 of the review) and the
assessment's A5, D6, F-apple-5 and F-android-10 rows in
[ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md);
then §2 below with the four files open. Milestone by milestone (§5); one
draft PR each into `main` under the fast lane; shipped Swift/Kotlin changes
take the mobile version bump.

The standing instruction: **if a step seems to require a server-side
library-scoped search, a server-side watch filter, changing the 200-row
clamp, or a client comparator that differs from the server's `ORDER BY`,
stop and flag it.** The only server change in this plan is additive: a
deterministic tie-break and one DTO field.

**Correction to the review:** F-android-10 says "asking for 500 does not
produce 500-row pages". On `main` the Android client asks for **200**
(`PlurxApi.kt:98-104`, default `limit = 200`) and already advances from
returned rows (`AppViewModel.kt:676-681`: `offset += page.items.size`), as
does the Apple client (`AppModel.swift:619-632`). What stands from D6 is
the eager whole-library walk and the per-page main-thread re-sort. Two
findings from this read that the review did not make, both load-bearing
for a k-way merge (§3.1): the server's `title` order has **no unique
tie-break** (`sort_title ASC` alone, both backends), so offset paging over
equal sort titles is not stable; and the client comparators
(`localizedCaseInsensitiveCompare` on Apple, `lowercase().compareTo` on
Android) do **not** reproduce the server's BINARY-collated `sort_title`
order, so "each page is server-sorted, the client re-sorts the merge" only
looked consistent because the whole library was always downloaded.

---

## 1. Objective

Both native library grids:

1. fetch pages **on demand** as the viewer approaches the end of what is
   loaded, one server page (200 rows) at a time per library cursor;
2. show a multi-library collection in **one merged order** produced by a
   k-way merge of server-sorted cursors, using a comparator that is the
   server's `ORDER BY` — locked to it by a shared fixture and a
   server-side tie-break;
3. define what a **title query or watch filter** does while pages are
   unloaded: the loader drives to completion in the background and the
   grid says so, so no title disappears;
4. filter and merge **off the main thread**, memoised by an immutable
   snapshot and a result generation, with a 150 ms debounce on the query.

Done means: five PRs merged; `cargo test -p plurxd library_sort`, `make
apple-test` and `make android-test` green with the shared fixture; and the
before/after measurement of §6 recorded for a 6,000-title category on the
Apple TV and the Lenovo.

---

## 2. Contract today

Copied from `main` @ `88a3957a`; **re-verify at build time**.

### 2.1 The endpoint

`GET /api/v1/libraries/{id}/items?sort=&offset=&limit=[&genre=][&facts=1]`
→ `browse::list_items`
([`http/browse.rs:184-215`](../../crates/plurxd/src/http/browse.rs)):

```rust
const DEFAULT_LIMIT: i64 = 60;                       // :21
const MAX_LIMIT: i64 = 200;                          // :22
fn clamp_limit(limit: Option<i64>) -> i64 {          // :117
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}
let sort = q.sort.as_deref().and_then(ItemSort::parse).unwrap_or_default();
let offset = q.offset.unwrap_or(0).max(0);
```

`ItemSort::parse` accepts `title | added | year | resolution | recorded`
(`plurx-core/src/domain.rs:1639-1643`). The response is `ItemListResponse
{items, total}` with per-user watch state joined per page. There is **no**
watch filter parameter, and `/search` (`browse.rs:703-721`) takes only
`q` and `limit` — no library scope.

The order, identical on both backends
([`hiqlite_media.rs:935-942`](../../crates/plurx-core/src/store/hiqlite_media.rs),
[`sqlite/media.rs:856-865`](../../crates/plurx-core/src/store/sqlite/media.rs)):

```rust
ItemSort::Title      => "sort_title ASC",
ItemSort::Added      => "added_at DESC, id DESC",
ItemSort::Year       => "year IS NULL, year DESC, sort_title ASC",
ItemSort::Resolution => "COALESCE((SELECT MAX(f.height) FROM files f WHERE f.item_id = items.id), -1) DESC, sort_title ASC",
ItemSort::Recorded   => "(recorded_at IS NULL), recorded_at DESC, sort_title ASC",
```

`sort_title` is `domain::sort_title_for(title)` (`domain.rs:369-379`):
`to_lowercase()`, then strip a leading `the ` / `a ` / `an ` if something
remains. It is stored, not exposed: `ItemDto` (`http/dto.rs:99-117`) has
`id`, `title`, `year`, `added_at`, … and no `sort_title`. Only `Added`
carries an `id` tie-break; the other four end in `sort_title ASC`, which is
not unique.

### 2.2 Apple

[`AppModel.swift:606-644`](../../clients/apple/Sources/AppModel.swift)
`libraryItems(_:sort:publish:)`: for each library in
`collection.libraries`, `limit = 200`, loop; `merged.append(contentsOf:
batch)`; `offset += batch.count`; after **every** batch
`publish(merged.sorted { Self.compare($0, $1, sort: sort) })` — a sort of
the whole merged array on the main actor per page; terminates on empty
batch, `offset >= total`, or `batch.count < limit`.

`compare` (`:985-1005`): `.title` →
`sortTitle(lhs.title).localizedCaseInsensitiveCompare(sortTitle(rhs.title))`;
`.added`/`.year`/`.resolution` → `compareDescending` then a title
fallback; `.recorded` → string compare of `recordedAt`. `sortTitle`
(`:1012`) strips the same three articles. No `id` tie-break.

[`LibraryView.swift`](../../clients/apple/Sources/LibraryView.swift):
`@State items`, `sort`, `filter`, `query`; `visibleItems` (`:14-16`) is a
computed property `items.filter { AppModel.matches($0, filter: filter) &&
(query.isEmpty || $0.title.localizedCaseInsensitiveContains(query)) }`,
read by `summary` (count), `stateContent` (empty check) and the
`LazyVGrid`'s `ForEach` — three evaluations per body. `.task(id: loadKey)`
where `loadKey = "\(collection.id):\(sort.rawValue)"` → `load()` → the
whole walk.

### 2.3 Android

[`AppViewModel.kt:674-681`](../../clients/android/app/src/main/java/tv/plurx/app/ui/AppViewModel.kt):

```kotlin
suspend fun libraryPages(id: Long, sort: String = "title", onPage: (List<Item>) -> Unit) {
    var offset = 0
    do {
        val page = api().libraryItems(id, limit = 200, offset = offset, sort = sort)
        if (page.items.isNotEmpty()) onPage(page.items)
        offset += page.items.size
    } while (page.items.isNotEmpty() && offset < page.total)
}
```

[`LibraryScreen.kt:69-100`](../../clients/android/app/src/main/java/tv/plurx/app/ui/LibraryScreen.kt):
`produceState(initialValue = null, libraryIds)` walks every library with
`vm.libraryPages(id)` — **always the default `sort = "title"`** — appending
to `loaded` and publishing `LibraryLoad(loaded.toList())` per page; then
`val shown = remember(load, sort, filter) { sortMerged(load?.items.orEmpty(),
sort).filter { matchesFilter(it, filter) } }` — a full re-sort and filter on
the composition thread per page and per sort/filter change. The comment
records the trade: "keying it on `sort` too meant every sort change
re-fetched the whole collection". `sortMerged` (`:212-222`):
`sortedBy { sortableTitle(it.title) }` (lowercase, default `String`
ordering) with `thenBy { sortableTitle }` on the other keys; no `id`
tie-break. There is no title query on the Android grid today.

### 2.4 Fixture conventions

Shared fixtures live under `tests/contracts/` and `tests/playback/`; both
native test targets read them by relative path (`clients/apple/project.yml`
`buildPhase: resources`; `clients/android/app/build.gradle.kts` adds
`../../../tests/contracts` to the `test` source set). Rust tests read them
with `include_str!`.

---

## 3. Change

### 3.1 Server — deterministic order, exposed key (one PR)

- Append `, id ASC` to the `Title`, `Year`, `Resolution` and `Recorded`
  orders on both backends (`Added` already has `id DESC`; leave it).
  Reason: without a unique final key, two items with equal `sort_title`
  may swap sides of a page boundary between requests, and an
  offset-paged client then shows one twice and the other never.
- Add `sort_title: String` to `ItemDto` (additive; `API.md` row). Reason:
  the clients must merge with the server's key, and re-deriving it
  client-side means three implementations of `sort_title_for` in three
  languages with three lowercasing rules.
- New shared fixture `tests/contracts/library-sort-cases.json`: ~30 items
  (ids, titles including articles, mixed case, accented and non-Latin
  titles, equal sort titles, null years, equal `added_at`, no files) and
  the expected `id` order for each of the five sorts. Rust test
  `library_sort_fixture_matches_order` loads the items into a fresh store
  on **both** backends and asserts `list_top_items_in_genre` returns the
  fixture order for every sort, across a page boundary (limit 7).
- `API.md`: the "items" row gains `sort_title` and the sentence "ordering
  is total: every sort ends in `id`".

### 3.2 The merge — server order, client-side, exactly

Both clients implement the same pure `LibraryMerge`:

```
 cursor per library: {libraryId, sort, offset, total?, buffer: [Item], exhausted}
                 │
                 ▼
 next(): among cursors with a non-empty buffer, pop the head that is
         smallest under KEY(sort); if any non-exhausted cursor has an
         EMPTY buffer, the merge cannot decide → request that cursor's
         next page first (a head we have not seen may precede every head
         we have)
                 │
                 ▼
 KEY(sort) — the server's ORDER BY, byte for byte:
   title:      (sort_title bytes ASC, id ASC)
   added:      (added_at DESC, id DESC)
   year:       (year IS NULL, year DESC, sort_title bytes ASC, id ASC)
   resolution: (resolution ?? -1 DESC, sort_title bytes ASC, id ASC)
   recorded:   (recorded_at IS NULL, recorded_at DESC, sort_title bytes ASC, id ASC)
```

"sort_title bytes" means the UTF-8 bytes of the DTO's `sort_title`
compared lexicographically (`Array(s.utf8).lexicographicallyPrecedes` in
Swift; `toByteArray(Charsets.UTF_8)` unsigned compare in Kotlin) — SQLite's
BINARY collation, not a locale compare. The fixture from §3.1 is loaded by
both native test targets and each `LibraryMerge` must reproduce the
expected order from k = 1, 2 and 3 shuffled cursors with page size 7.

Page requests: `limit = 200` (the clamp; asking for more changes
nothing), `offset += returned.count`, exhausted when `returned.isEmpty ||
offset >= total || returned.count < 200` — the termination the clients
already use, now per cursor.

**Demand trigger:** the grid asks `merge.ensure(visibleThrough: index +
prefetch)` where `prefetch` = two rows of the current column count (Apple:
from `columns`/poster width; Android: `LazyGridState.layoutInfo`). The
merge fetches until it has `index + prefetch` decided items or every
cursor is exhausted. First paint after one request per library (as today);
a single-library grid is one request until the viewer scrolls.

**Sort change** re-creates the cursors with the new server `sort`. Android
loses its "never re-fetch on sort" property (§2.3) deliberately: with
demand paging a sort change costs one page per library, not the whole
collection, and the alternative — merging cursors the server sorted by a
*different* key — is not a merge.

### 3.3 Query and watch filter over unloaded titles

Neither is server-side (§2.1), and this plan does not add them (§4). So:

- While `query` is empty and `filter` is `all`, the grid shows the merge's
  decided prefix and pages on scroll.
- The moment `query` is non-empty **or** `filter != all`, the loader
  switches to **drive-to-completion**: it keeps requesting pages from
  every cursor in the background (same 200-row pages, sequential per
  cursor, all cursors interleaved) until all are exhausted or the view
  leaves. The filter applies to everything loaded so far; the summary
  line reads "12 of 6,000 loaded · 3 match" until complete; the empty
  state while incomplete says "Still loading — N of M titles checked",
  never "No matching titles". Reason (F-apple-5, F-android-10): "so
  unloaded titles do not disappear" — a title the viewer is searching for
  must not be absent merely because its page had not been fetched.
- Clearing the query and filter stops the drive; what was loaded stays.
- Bound: a 6,000-title collection is 30 requests of 200; at the observed
  ~100 ms per page on the LAN that is ~3 s to completion, during which the
  match count grows visibly. This is the accepted cost of not adding a
  server search scope in this plan.

### 3.4 Off the main thread, memoised, generation-guarded

Apple (`LibraryView`):

- `items` becomes the merge's decided array (immutable `[Item]` value,
  replaced whole per page — COW makes the snapshot free).
- A `@State visible: [Item]` **stored** result replaces the computed
  `visibleItems`; it is produced by one `Task.detached(priority:
  .userInitiated)` that receives `(snapshot: items, filter, query,
  generation)`, filters, and hands back `(result, generation)`; the main
  actor assigns only if `generation == current`. `query` edits are
  debounced 150 ms via `.task(id: query)` + `Task.sleep`; `filter` and
  page arrivals are not debounced (they are rare).
- `summary`, `stateContent` and the grid read `visible` — one evaluation,
  three readers.
- The merge's `next()` loop runs in the same detached task; the main actor
  receives arrays, never sorts.
- `ForEach(visible)` keeps `Item.id` as identity; refresh (`.refreshable`)
  rebuilds cursors and keeps the current `items` until page 1 of the new
  walk arrives (today's "never blank" rule, `LibraryView.swift:139-143`).

Android (`LibraryScreen`):

- `produceState` is replaced by a `LibraryPager` held in `AppViewModel`
  (survives rotation like the `rememberSaveable` sort/filter already do),
  exposing `StateFlow<LibraryGridState>` = `{decided: List<Item>,
  loadedCount, total, complete, error}`.
- Filtering: `snapshotFlow { query to filter }.debounce(150)` combined
  with the pager's flow via `combine` + `mapLatest { withContext(Default)
  { merge/filter } }` — `mapLatest` is the generation guard (a newer input
  cancels the older computation).
- `sortMerged` is deleted; `LibraryMerge.KEY` replaces it. `key = { it.id }`
  on the `LazyVerticalGrid` items.
- The TV D-pad focus rule at `:96-98` (`RequestInitialFocus(backFocus)`)
  is unchanged; a page arriving must not move focus — assert in the
  existing Compose UI test.

---

## 4. Guardrails (non-goals)

- **Preserve multi-library sorting** (assessment A5: "Sorting is needed
  across multiple libraries even if each page is server-sorted"). The
  k-way merge is the sort; there is no client re-sort of a concatenation
  anywhere after this plan.
- **Per-page server ordering is not proof of merged order** (F-apple-5).
  Hence the tie-break, the exposed key, and the fixture that pins
  comparator = SQL on both backends and both clients.
- **The server clamps to 200; advance from returned rows; preserve
  termination** (F-android-10). §3.2 states the loop; the clients already
  do this and the plan keeps it per cursor.
- **Define search/filter behaviour before removing the client merge**
  (F-android-10, F-apple-5). §3.3 defines it: drive to completion, say so,
  never show "No matching titles" while incomplete.
- **No server-side library-scoped search or watch filter in this plan.**
  Either is an API addition with its own contract and clients; this plan
  works without them and says what it costs (§3.3).
- **Immutable snapshots and result generations, not debounce alone**
  (F-apple-5). §3.4: the detached task receives a value copy and a
  generation; `mapLatest` on Android.
- **Stable identities and refresh behaviour preserved** (F-apple-5).
  `Item.id` keys; the never-blank refresh rule.
- **Measure before and after** (assessment A5: "profile a representative
  library"). §6 names the measurement and the two devices; the numbers go
  in the PR bodies.
- **No change to the 200 clamp, `DEFAULT_LIMIT`, or the `genre` filter.**
- **No settings key, no metric.** The endpoint's existing request count
  is the observable; `plurx_http_requests_total` does not exist yet
  (review C10) and this plan does not add it.

---

## 5. Milestones

### 5.1 Server: total order and `sort_title` in the DTO

§3.1. `cargo test -p plurxd library_sort` and the store tests on both
backends; `API.md` updated in the same PR (the docs test requires it).

**Acceptance:** `cargo test -p plurxd library_sort_fixture_matches_order`
passes on `--features` for both backends as the workspace runs them;
`curl -s -H "authorization: Bearer $T" 'http://media1:8080/api/v1/libraries/1/items?limit=1' | jq '.items[0].sort_title'`
prints a string.

### 5.2 Apple: `LibraryMerge` and demand paging

`LibraryMerge` (pure, `nonisolated`), cursors in `AppModel`, the
`ensure(visibleThrough:)` trigger from `LibraryView`'s grid via
`.onAppear` on the card at `index`, drive-to-completion for
query/filter, summary text. Tests: fixture order for k = 1/2/3 cursors at
page size 7; `ensure` issues exactly one request per library for a
first paint of 40 items; a query switches to drive-to-completion and the
summary reports `loaded/total`.

**Acceptance:** `make apple-test` green; on the Apple TV, opening a
6,000-title category shows the first screen after **one round trip per
library** (Charles/`journalctl` request count on `media1`: one
`/items?offset=0` per library, no `offset=200` until scrolling).

### 5.3 Apple: filtering off the main actor

§3.4 Apple bullets. Tests: a stale generation's result is discarded; the
150 ms debounce coalesces five keystrokes into one filter pass.

**Acceptance:** `make apple-test` green; Instruments Time Profiler on the
Apple TV while typing a five-letter query into a fully loaded 6,000-title
category: main-thread time per keystroke under 16 ms (one frame), against
the before-number recorded in 5.2's PR body.

### 5.4 Android: `LibraryPager` with `LibraryMerge`

§3.2/§3.3 on Android; `sortMerged` deleted; `libraryPages` keeps its
signature but is called per cursor with the grid's `sort`. JVM tests
mirror 5.2 against the same fixture.

**Acceptance:** `make android-test` green; on the Lenovo, the same
one-request-per-library first paint as 5.2, observed on `media1`.

### 5.5 Android: filtering off the composition thread, debounced

§3.4 Android bullets; a title query field is **not** added to the Android
grid in this plan (parity item, separate); the watch filter goes through
the same `mapLatest` pipeline so the query can later join it.

**Acceptance:** `make android-test` green; Perfetto trace on the Lenovo
while cycling the watch filter four times on a fully loaded 6,000-title
category: no main-thread `sortMerged`/filter frame over 16 ms; the
existing Compose focus test still passes.

---

## 6. Verification and rollout

Fast lane per PR: `make unit` is unaffected except 5.1 (`cargo test -p
plurxd library_sort`); `make apple-test` (5.2, 5.3); `make android-test`
(5.4, 5.5). The shared fixture is the cross-platform gate: a change to any
`ORDER BY` fails three test suites.

The measurement (assessment: "Device lag remains unmeasured"): before
5.2/5.4, on the Apple TV and on the Lenovo, open the largest category on
`media1` (record its item count), and capture (a) the request count and
wall time to first paint from `journalctl -u plurxd` on `media1`, (b) an
Instruments / Perfetto main-thread trace while scrolling to the end and
while changing the watch filter. Record the same after. The PR bodies
carry the numbers; APPLE-CLIENT-PARITY.md and ANDROID-CLIENT-PARITY.md get
one dated line each.

GPT prompt for the device measurement:

```
On the Apple TV and on the Lenovo tablet, sign in to http://10.42.0.10:8080
and open the category "All films" (report how many items it says). With a
stopwatch: time from tap to first posters; scroll to the very end and
report whether the grid ever showed an empty gap or a duplicate poster;
change the watch filter to Unwatched and report how long the grid took to
update and whether the count line changed as it loaded. Then search (Apple
only) for a title you know is at the END of the alphabet, "Zero Harbor",
and report whether it appeared, and whether the screen said "Still
loading" first. On media1 run
`journalctl -u plurxd --since -5min | grep -c '/items?'` and report the
number.
```

Rollout: 5.1 deploys with the server first (additive; old clients ignore
`sort_title`). 5.2-5.5 ship through the normal client builds
([CLIENT-DEPLOY-PROMPT.md](CLIENT-DEPLOY-PROMPT.md)); each requires the
5.1 server (a client that merges on a missing `sort_title` would merge on
empty strings), so the clients check `sort_title != nil` and fall back to
today's whole-walk-and-resort path against an older server — that
fallback is deleted one release later.

---

## 7. Open questions

1. **Should `/libraries/{id}/items` gain `?watch=unwatched|in_progress|watched`
   and `/search` gain `?library=`?** Both would make §3.3's
   drive-to-completion unnecessary. They are API work with a per-user
   join on the hot path (S7 territory); this plan says what it costs
   without them and leaves the decision to the S7 index work.
2. **`Added` order tie-break is `id DESC` while the others gain `id ASC`.**
   Keeping the existing `Added` clause avoids reordering a page viewers
   see today; the fixture pins both. If uniformity is preferred, change it
   in 5.1 and say so in `API.md`.
3. **Lowercasing divergence** is moot once clients use the DTO's
   `sort_title`, but the fixture still includes accented and non-Latin
   titles so a future client-side derivation would fail loudly.
4. **Prefetch depth** of two rows is a guess for a LAN; a Wi-Fi tablet
   scrolling fast may see the loading footer. The trace in §6 says whether
   four rows is needed; the constant lives in `LibraryMerge` and the
   PR can move it with the number attached.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | 5.1 | [PR #465](http://192.168.4.7:3000/noirr/plurx/pulls/465) | Server-side total order and the exposed key. The three copies of the library `ORDER BY` — `sqlite/media.rs` and both Hiqlite paths — are one `store::item_sort_order_by`, and `Title`, `Year`, `Resolution` and `Recorded` gained `id ASC` (`Added` keeps its existing `id DESC`; §7 question 2 is answered that way, and `API.md` says so). `ItemDto` carries `sort_title`, the stored value the SQL sorts on, not a re-derivation. `tests/contracts/library-sort-cases.json` holds 30 movie items — articles, mixed case, three titles that reduce to one sort title, accented, Greek, Cyrillic and CJK titles, the `"The "` edge case the domain unit test already pins, null years, equal capture dates, items with no file — and the expected id order for all five sorts. `library_sort_fixture_matches_order` in `crates/plurx-core/tests/store_contract.rs` replays it on **both** SQLite backends (and on the three-voter Hiqlite backend when `hiqlite-contract-tests` is on), once whole and once walked at the fixture's page size of 7, so a single-request order that is total while the paged one is not still fails. **Plan correction:** §5.1 names `cargo test -p plurxd library_sort`; the test that can reach both backends lives in `plurx-core`, so the command is `cargo test -p plurx-core --test store_contract library_sort_fixture_matches_order`. **A correction this session made against its own first attempt.** The fixture replay was first offered as the proof of the `, id` tie-break, and it is not one: removing `id ASC` from all four clauses leaves it passing, because SQLite returns this table's tied rows in rowid order and rowid order is exactly what the fixture expects. That it currently does is the whole reason the clause exists — nothing promises it, and the day a query plan, an index or a backend changes it, an offset-paging client reads two adjacent pages of two different orderings. The proof is therefore `store::item_sort_order_tests::every_sort_ends_in_a_unique_key`, which requires the clause rather than observing the behaviour: removing the tie-break fails it (`Title ends in "sort_title ASC", which has no unique final key`), and re-spelling the order inside a media store fails its companion. The replay is not vacuous either — ordering `Title` on the raw `title` instead of `sort_title` fails it on the first article. **Not done and not claimed:** the §5.1 `curl … | jq '.items[0].sort_title'` acceptance is a deployed-server observation and no deploy was made from here. Milestones 5.2-5.5 are **not started**: this session had no Swift toolchain, no Xcode and no Android SDK, so `LibraryMerge`, demand paging, drive-to-completion and the off-main-thread filtering could be written but not compiled, run or measured — and the plan's whole point for them is a measurement. The clients' `sort_title == nil` fallback against an older server is likewise unwritten. |
