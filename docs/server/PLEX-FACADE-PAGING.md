# Plex façade paging — batched metadata, honest container counts, and a golden corpus to hold them

**Status:** ready for review · **Executes:** C4 / F-core-5, with C9 /
F-core-11 as the measure-first appendix, from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
(assessment rows C4, F-core-5, C9, F-core-11 in
[ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md))
· **Written:** 2026-09-20 against `main` @ `0f02b7ea`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Board id **C-07**. Read §2 first, then **§5.0 before anything else**: this
plan's priority is conditional on a real Plex-family client being in use,
and §5.0 is the census that settles it. If the census says nobody is
calling the façade, put the row back to `unclaimed` with the census result
in the Notes column and stop — the work is correct and unurgent, and the
board should say so rather than hiding it inside a merged PR. If the
census says Kodi or PlexKodiConnect *is* in use, build §5 in order: M1
(the golden corpus) first, because M2 and M3 are behaviour changes to XML
and a corpus is the only way to prove they did not move anything else.
The C9 appendix (M5–M6) is independent and may run in a parallel session;
it starts with a measurement and is not authorised to change the TTL.
One draft PR per milestone into `main` under the fast lane. Every
`file:line` is from `0f02b7ea`; re-verify by function name.

**If a step seems to require changing what `map::is_plex_visible` exposes,
returning a `<Media>`/`<Part>` element without its file and version
details, letting `size` mean anything other than the number of children in
*this* response, extending the media-session route cache's positive TTL,
or making the control path read that cache, stop and flag it.**

**Correction to the review:** two, one of them material.

1. **`files_for_items` does not exist.** C4's remedy says "Batched
   `files_for_items`/`child_counts` (helpers exist in `browse.rs:236/247`)".
   `child_counts` does exist and is exactly that
   (`store/mod.rs:2768`, called at `browse.rs:247`). There is no
   `files_for_items` anywhere in the tree — `grep -rn "fn files_for_items"
   crates/` is empty. What `browse.rs:235` calls is
   `item_media_facts(&file_backed)` (`store/mod.rs:2787`), which returns a
   `MediaFacts` **summary** per item, not the `MediaFile` rows that
   `map::video_element` needs to emit `<Media>` and `<Part>`. So M2 must
   *add* the batched files helper rather than reuse one, and the
   assessment's F-core-5 wording — "Reuse batched metadata **without
   dropping file/version details**" — is precisely a warning against
   reaching for `item_media_facts` because it is already batched. This
   plan takes that warning literally.
2. **There is no recorded Kodi/PKC traffic in the repository.**
   `crates/plurx-compat-plex/tests/` contains one file,
   `cluster_discovery.rs`; nothing under `tests/` mentions Plex; and
   `X-Plex-Container` appears nowhere in the tree except the review
   documents themselves. A plan cannot test paging "against the recorded
   fixtures" because there are none. M1 therefore *creates* the corpus, as
   golden XML generated from a seeded catalogue rather than captured from a
   client — which is the honest substitute, and §7.1 says what only a real
   client can still prove.

A third, smaller one: the review's `plex.rs:191-267` is `:214-267` at this
commit (`section_all` begins at `:214`).

---

## 1. Objective

1. `GET /library/sections/{id}/all` makes a number of store calls that does
   not grow with the number of items on the page. Today it makes one per
   item, plus one per item for children.
2. Every `<Media>`/`<Part>` attribute a client reads today is byte-for-byte
   what it was. A batched read is a different way to fetch the same rows,
   not a smaller answer.
3. The façade honours `X-Plex-Container-Start` / `X-Plex-Container-Size`
   on the three list handlers, with `totalSize`, clamped inputs, an
   ordering that does not shuffle between pages, and a correct empty page.
4. A golden XML corpus pins every one of those answers, so the next change
   to the façade has an oracle.
5. Separately and conditionally: the media-session route cache's cost is
   **measured** before anything about it is changed.

## 2. Contract today

Re-verify at build time.

### 2.1 `section_all` is an N+1, twice over

`crates/plurxd/src/http/plex.rs:214-236`:

```rust
pub async fn section_all(
    PlexUser(user): PlexUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    match state.catalogue.get_library(id).await? {
        Some(lib) if map::is_plex_visible(lib.kind) => {}
        _ => return Err(ApiError::NotFound("section")),
    }
    let page = state
        .catalogue
        .list_top_items_in_genre(id, Default::default(), 0, 5000, None)
        .await?;
    let views = views(&state, user.id, &page.items).await?;
    let mut elements = Vec::with_capacity(page.items.len());
    for item in &page.items {
        let view = views.get(&item.id).copied().unwrap_or_default();
        elements.push(element_for(&state, item, view).await?);
    }
    Ok(xml(plex::container(elements)))
}
```

`views` (`:158-170`) is **already batched** — one `watch_map` for the whole
page. `element_for` (`:191-212`) is not:

```rust
match item.kind {
    ItemKind::Movie | ItemKind::Episode => {
        let files = state.catalogue.files_for_item(item.id).await?;   // one call per item
        Ok(map::video_element(item, &files, view))
    }
    ItemKind::Show | ItemKind::Season => {
        let children = state.catalogue.get_item_children(item.id).await?;  // one call per item
        Ok(map::directory_element(item, Some(children.len() as i64), view))
    }
    …
}
```

The Show/Season arm is the worse of the two: it fetches every child row in
order to call `.len()` on them. `child_counts` (`store/mod.rs:2768`) exists
for exactly that and its doc comment says so — "doing that with one query
per card would be an N+1 on every grid render."

`metadata` (`:239-250`) and `children` (`:252-265`) have the same shape;
`children` is an N+1 over one show's episodes.

### 2.2 There is no paging input

`section_all` takes only `Path(id)`. It asks for `offset 0, limit 5000`
unconditionally and builds `plex::container(elements)`
(`crates/plurx-compat-plex/src/lib.rs:64-69`):

```rust
pub fn container(children: Vec<Element>) -> Element {
    let size = children.len() as i64;
    Element::new("MediaContainer")
        .attr_i("size", size)
        .children(children)
}
```

So `size` is the child count, there is no `totalSize` and no `offset`, and
a library of 6,000 titles silently loses 1,000 of them.
[API.md](../API.md) §20 documents the endpoint as "Up to 5000 top-level
items, with per-user view state", which is accurate and is the line M3
updates.

### 2.3 The ordering has no tie-break

`list_top_items_in_genre` defaults to `ItemSort::Title`
(`domain.rs:1622-1625`), which both backends render as bare
`"sort_title ASC"` (`sqlite/media.rs:857`, `hiqlite_media.rs:936` and
`:2036`). Two items with the same `sort_title` have no defined relative
order, so `LIMIT/OFFSET` may show one of them on two pages and the other on
none. Today nothing pages, so nothing notices. **The moment M3 lands,
this becomes a correctness bug**, and M3 owns fixing it.

`ItemPage` already carries the total (`domain.rs:1650-1653`:
`items`, `total`), so `totalSize` costs nothing extra.

### 2.4 The route cache (the C9 appendix)

`crates/plurxd/src/media_sessions.rs:107-108`:

```rust
const ROUTE_CACHE_TTL: Duration = Duration::from_secs(1);
const MAX_ROUTE_CACHE_ENTRIES: usize = 4_096;
```

`CachedRoute` (`:1512-1515`) is `{ route: Option<MediaSessionRoute>,
expires_at: Instant }` — positive **and** negative answers, both for one
second. `raw_route_before` (`:1808-1858`) is the read path:

```text
  reject_pending_release  ──▶ cached_route (lock #1)  ──hit──▶ return
        │ miss
        ▼
  route_queries[hash % 32].lock()      ← single-flight, 32 shards
        │
        ├─ cached_route again (lock #2)  ──hit──▶ return
        ▼
  store.media_session_route(session_id)   ← NOT under the map lock
        │
        ▼
  cache_queried_route_result (lock #3)   ← generation-checked insert
```

Three things the finding's wording does not carry, all load-bearing:

- The map mutex is **not** held across the Store call — the assessment
  says so and the code confirms it (`:1845` is outside every `routes.lock()`
  scope).
- Repeated lookups of one capability are already single-flighted by the
  32 `route_queries` shards (`:1618-1622`), so N concurrent segment GETs for
  one session produce **one** store read, not N.
- The control path deliberately bypasses the positive cache. The comment
  at `:1860-1863` is explicit: "Unlike media GET routing this deliberately
  bypasses the positive cache: an owner epoch may advance during its
  one-second TTL."

There is a query counter already, but it is test-only:
`#[cfg(test)] route_store_queries` (`:1631, 1842-1844`).

A separate cost the finding does not name: `cache_route_result` and
`cache_queried_route_result` both run `routes.retain(|_, cached|
cached.expires_at > now)` (`:2258, 2276`) on **every** insert — an O(n)
sweep over up to 4,096 entries while holding the map lock. That is a
different cost from the lookup and is measured separately in M5.

## 3. Change

### 3.1 Batched metadata (M2)

One new trait method, beside `files_for_item` (`store/mod.rs:2764`):

```rust
/// Every item's media files, for many items at once. The list handlers
/// need the file rows themselves — `item_media_facts` is a summary and
/// cannot build a `<Media>`/`<Part>` element (assessment F-core-5).
async fn files_for_items(
    &self,
    ids: &[i64],
) -> Result<std::collections::HashMap<i64, Vec<MediaFile>>, StoreError>;
```

Implemented on both backends the way `child_counts` already is
(`sqlite/media.rs:2027`, `hiqlite_media.rs:2930`) — read those two first
and copy their shape, including how they chunk, so this method's SQL and
their SQL are recognisably the same thing. Ordering **within** an item must
match `files_for_item` exactly, because `map::video_element` renders the
list in order and the first entry is what a client picks by default; M2's
first test asserts that equality directly.

`element_for` becomes `elements_for(state, items, views)`:

```text
  items ─┬─ Movie/Episode ids ──▶ files_for_items(&ids)      (1 call)
         ├─ Show/Season ids   ──▶ child_counts(&ids)         (1 call)
         └─ views already batched at plex.rs:158             (1 call)
                     │
                     ▼
         map::video_element / map::directory_element, unchanged
```

Three store calls per page, whatever the page holds. `element_for` stays
as a thin wrapper for the single-item `metadata` handler so that handler's
shape does not change.

`children` (`:252-265`) keeps `get_item_children` — it *needs* the rows —
but feeds their ids to the two batched calls rather than looping.

The mapping functions (`map::video_element`, `map::directory_element`) are
not touched. That is the guarantee that no attribute moves.

### 3.2 Container paging (M3)

**Inputs.** Plex clients send the container window as headers and, in some
versions, as query parameters of the same names. Both are read, header
first, and both spellings are accepted:

```rust
struct ContainerWindow { start: i64, size: Option<i64> }

const MAX_CONTAINER_SIZE: i64 = 5_000;   // today's implicit ceiling, kept

fn container_window(headers: &HeaderMap, query: &HashMap<String, String>) -> ContainerWindow
```

Clamping, each with its reason:

- `start` below zero, or not a number → `0`. A negative offset is a client
  bug and answering `400` would break a client that works today.
- `start` beyond `total` → kept as given; the answer is a correct empty
  page (see below), not a 404. A client that asks for page 9 of a library
  that shrank must be told "there is nothing here", not "this section does
  not exist".
- `size` absent → `MAX_CONTAINER_SIZE`, which is exactly today's
  behaviour, so a client that sends no window sees no change.
- `size` ≤ 0 → `0`, which is a legal request for a count only; Plex
  clients use it to discover `totalSize` before paging.
- `size` above `MAX_CONTAINER_SIZE` → `MAX_CONTAINER_SIZE`.

**Outputs.** `plex::container` gains a sibling rather than a changed
signature, so the handlers that have no window keep emitting exactly what
they emit now:

```rust
pub fn paged_container(children: Vec<Element>, offset: i64, total: i64) -> Element {
    let size = children.len() as i64;
    Element::new("MediaContainer")
        .attr_i("size", size)          // children in THIS response — unchanged meaning
        .attr_i("totalSize", total)    // the whole collection
        .attr_i("offset", offset)
        .children(children)
}
```

`size` keeps its current meaning. That is not a detail: Plex's own
containers use `size` for the returned count and `totalSize` for the
collection, and a client that computes "have I reached the end?" from
`offset + size >= totalSize` breaks if `size` becomes the total.

An **empty page** is `size="0"` with the real `totalSize` and the
requested `offset`, no children, HTTP 200, same content type. Not a 404 and
not an error: the collection exists and the window is past its end.

**Ordering.** `ItemSort::Title` gains an explicit tie-break in both
backends: `"sort_title ASC, id ASC"`. This is the one change outside the
façade, and it is required by paging (§2.3). `id ASC` rather than any
other column because `id` is unique, monotone and already indexed as the
primary key, so it is total and free. The other four sorts already end in
a discriminating column or are out of the façade's reach; M3 adds the
tie-break to `Title` only and states that in the PR, rather than touching
sorts nothing here pages.

**Which handlers.** `section_all`, `children`, and the search handler
(`/search` · `/hubs/search`, capped at 50 per [API.md](../API.md) §20 —
window it too so a client that asks for a second page gets an honest empty
one rather than the same 50 again). `metadata` returns one element and
takes no window.

### 3.3 The golden corpus (M1)

`crates/plurx-compat-plex/tests/golden/` with one `.xml` file per case,
plus a Rust test that renders from a seeded in-memory catalogue and
compares byte-for-byte. Seed: one Movies library with 12 titles (two
sharing a `sort_title`, one with two files, one with none), one Shows
library with 2 shows × 2 seasons × 3 episodes, one Books library (invisible
to the façade), and one title with a Dolby Vision file so the `<Media>`
attributes exercise the interesting branch.

Cases, before any behaviour change:

```text
  root.xml                 GET /
  identity.xml             GET /identity
  sections.xml             GET /library/sections
  section_all.xml          GET /library/sections/1/all          (no window)
  metadata_movie.xml       GET /library/metadata/<movie>
  metadata_show.xml        GET /library/metadata/<show>
  children_show.xml        GET /library/metadata/<show>/children
  children_season.xml      GET /library/metadata/<season>/children
  search.xml               GET /search?query=harbor
```

M1 lands these against **today's** output. M2 must not change a single
byte of any of them — that is M2's acceptance. M3 adds the windowed cases
and changes only `section_all.xml`, `children_*.xml` and `search.xml` by
the two new attributes, which the PR diff then shows exactly.

Fixture titles use the public-mirror names (Harbor Lights, Night Tide) per
the repository's naming rule.

### 3.4 Metrics

| Metric | Type | Labels | Why |
|---|---|---|---|
| `plurx_plex_requests_total` | counter | `handler=` one of the fourteen route handlers registered at `http/mod.rs:452-465` plus `root` (`root_dispatch`, `:475`) — `root`, `identity`, `library_root`, `sections`, `section_all`, `metadata`, `children`, `image`, `part`, `photo_transcode`, `timeline`, `scrobble`, `unscrobble`, `search`; `outcome="ok\|not_found\|error"` | The census in §5.0 answers "is anyone using this" once; this answers it continuously, which is what decides whether the façade gets future work |
| `plurx_plex_container_items` | histogram | none; buckets 1,10,50,200,1000,5000,+Inf | Whether clients actually page, and how big a page they ask for |

Fourteen handlers × three outcomes is 42 fixed series; no ids, no titles,
no tokens. (`/search` and `/hubs/search` share one handler and therefore
one label, which is correct — they are the same code path per
[API.md](../API.md) §20.) Rendered from atomics in the house pattern
(`store/hiqlite.rs:629-737`), so the existing
`prometheus_scrape_has_no_store_operation` assertion
(`http/system.rs:5483`) still holds.

### 3.5 The C9 appendix — measure, then decide (M5–M6)

**M5 is instrumentation only. It changes no constant and no behaviour.**

1. Promote the test-only counter (`media_sessions.rs:1631`) to a shipped
   `plurx_media_session_route_lookups_total{result="cache_hit\|
   single_flight_hit\|store"}` — three series. `single_flight_hit` is the
   second `cached_route` at `:1831`, which is the one that proves the
   shards are already absorbing concurrent segment GETs.
2. `plurx_media_session_route_lock_seconds` (histogram, buckets
   0.000_01, 0.000_1, 0.001, 0.01, 0.1, +Inf) measured around each of the
   three `routes.lock()` sites, and
   `plurx_media_session_route_prune_entries` (histogram) around the
   `retain` sweep at `:2258, 2276` — because F-core-11 says lock sharding
   and pruning are *separate* measured changes, and this separates them at
   the measurement, not just in prose.
3. `plurx_media_session_route_cache_entries` (gauge) against
   `MAX_ROUTE_CACHE_ENTRIES = 4_096`.

**M6 is conditional on what M5 measures, and is written as a decision, not
as a change.** The PR body must contain the p50/p95/p99 of lock wait and
prune size, the lookup breakdown, and the store-read rate per active HLS
session, taken on `media1` during four concurrent playbacks. Then, and only
then, one of:

- **Nothing to fix.** If store lookups per session per second are already
  well below 1 (the single-flight shards making the "≥1/s" in C9 wrong in
  practice), the appendix closes with the measurement recorded and the TTL
  untouched. This is the outcome the code shape predicts, and the plan says
  so up front so nobody treats a null result as a failure.
- **Shard the map.** If lock wait is the cost, shard `routes` the way
  `route_queries` is already sharded and prune per shard. This changes no
  authority semantics at all — it is the safe change, and it is the one
  F-core-11 explicitly permits as separate work.
- **A longer positive TTL.** Only with proof against the three events that
  change authority immediately: **release** (the durable end, published
  through `cache_terminal_route`, `:2233-2242`), **replacement** (a
  prepared successor committing), and **fencing** (an owner epoch
  advancing). The assessment is blunt that lease TTL does not bound these.
  Any proposal here must show, as tests, that each of the three invalidates
  the cache within one request rather than within the TTL — and note that
  the negative-cache half has a different argument from the positive half
  and must be reasoned about separately. **This plan does not authorise
  that change**; it authorises writing the proof.

## 4. Guardrails (non-goals)

- **Batching must not drop file or version details** (F-core-5). §3.1 adds
  a helper that returns the same `MediaFile` rows in the same order and
  leaves `map::video_element` untouched; M1's corpus is landed *before* M2
  so "unchanged" is a byte comparison, not a claim.
- **`totalSize`, offset/limit clamping, stable ordering, child pagination**
  (F-core-5, verbatim). §3.2 specifies each, including the ordering
  tie-break that paging makes necessary and that nothing needs today.
- **Empty pages are 200, not 404** (C4's "test … empty pages"). §3.2 and a
  corpus case.
- **Priority is conditional on real façade use** (C4: "Only matters if
  Kodi/PKC is in use"; F-core-5: "Measured Kodi refresh latency remains
  outstanding"). §5.0 is the census, it runs first, and its result is
  recorded on the board whichever way it goes.
- **Measured Kodi refresh latency stays outstanding, and is named as such.**
  §5.0 step 4 and §7.1: this plan can make the server answer fewer
  queries; only a Kodi box can say whether a library refresh got faster.
- **C9 is measure-first** (C9 verdict "Measure"; F-core-11 "Amend"). M5
  changes nothing; M6 is a decision with three named outcomes, one of them
  "nothing to fix".
- **The map lock is not proof of serial Store I/O** (F-core-11). §2.4
  states that the lock is not held across the Store call, that the query
  shards already single-flight, and that the control path bypasses the
  positive cache. The instrumentation separates lock wait from store time
  so the measurement can distinguish them.
- **Sharding and pruning are separate measured changes** (F-core-11).
  Separate metrics in M5, separate outcomes in M6, and never bundled with a
  TTL change.
- **No `now + TTL/2` expression.** The assessment flagged that the first
  draft's TTL arithmetic had no unambiguous definition. If M6 lands a TTL
  change at all, the TTL is defined once, in one place, as an absolute
  `expires_at` — which is what `CachedRoute` already stores (`:1514`) — and
  no derived fraction of it appears anywhere.
- **No feature gate, no new setting.** `MAX_CONTAINER_SIZE` is today's
  implicit ceiling made explicit.

## 5. Milestones

### 5.0 M0 — is anyone using the façade? (no PR)

Run the GPT prompt in §6.3 first. Record the answer in the board row's
Notes column either way. If no Plex-family client has called the façade in
the retained access-log window, stop here and return the row to
`unclaimed` with `deferred: no façade traffic <date>` — do not build M1–M3
on speculation. If a client is in use, name it (Kodi, PlexKodiConnect,
Infuse, other) in the Notes, because which one it is changes which
attributes M1's corpus must cover.

### 5.1 M1 — the golden corpus (`core/plex-golden-corpus`)

1. `crates/plurx-compat-plex/tests/golden/` and the seeded-catalogue
   renderer; the nine cases in §3.3, captured from **current** behaviour.
2. `plurx_plex_requests_total` and `plurx_plex_container_items` (§3.4).
3. A test that fails with a readable diff, not `assert_eq!` on two
   3,000-character strings — split on `\n` and compare line by line.

Acceptance: `cargo test -p plurx-compat-plex` and `cargo test -p plurxd
plex` green; `ls crates/plurx-compat-plex/tests/golden/*.xml | wc -l`
prints 9; re-running the test twice in a row produces no diff (the
renderer is deterministic, which a `HashMap` iteration in the seed would
break).

### 5.2 M2 — batched metadata (`core/plex-batched-metadata`)

1. `files_for_items` on the trait and both backends; `elements_for`;
   `section_all`, `children` and the search handler using it;
   `child_counts` replacing `get_item_children(...).len()`.
2. Tests:
   `files_for_items_matches_files_for_item_per_item` (order included);
   `files_for_items_omits_items_with_no_files` (the map has no entry, and
   the element renders exactly as today);
   `section_all_makes_three_store_calls_for_a_hundred_items` (counting
   store);
   `a_show_section_no_longer_reads_child_rows`.

Acceptance: `cargo test -p plurx-compat-plex` green **with every golden
file unchanged** — `git diff --stat crates/plurx-compat-plex/tests/golden/`
prints nothing; `cargo test -p plurxd plex` green;
`grep -n "files_for_item(" crates/plurxd/src/http/plex.rs` finds only the
single-item `element_for` wrapper.

### 5.3 M3 — container paging (`core/plex-container-paging`)

1. `ContainerWindow`, `container_window`, `paged_container`; the three
   handlers; the `sort_title ASC, id ASC` tie-break in both backends;
   [API.md](../API.md) §20's `section_all` row updated to describe the
   window and `totalSize`.
2. New corpus cases: `section_all_window_0_5.xml`,
   `section_all_window_10_5.xml` (past the end, empty),
   `section_all_window_size_0.xml` (count only),
   `section_all_window_oversize.xml` (clamped),
   `children_show_window_0_2.xml`.
3. Tests:
   `two_pages_cover_every_item_exactly_once` (the tie-break's real
   purpose: 12 items including a duplicate `sort_title`, pages of 5, assert
   the union is the whole library with no repeats);
   `a_window_past_the_end_is_an_empty_200_with_the_real_total`;
   `size_is_the_returned_count_and_total_size_is_the_collection`;
   `a_negative_start_is_clamped_not_refused`;
   `no_window_produces_exactly_the_pre_paging_bytes` (the golden file from
   M1, unchanged apart from the two new attributes).

Acceptance: `cargo test -p plurx-compat-plex` and `cargo test -p plurxd
plex` green; `make unit` green once before un-WIP;
`python3 -m pytest tests/operations/ -k api_doc_routes` green (API.md
changed).

### 5.4 M5 — route-cache instrumentation, no behaviour change (`core/route-cache-metrics`)

1. The three metric families in §3.5.
2. Tests: `a_cache_hit_and_a_store_read_are_counted_separately`;
   `concurrent_lookups_of_one_session_produce_one_store_read` (pins the
   single-flight claim in §2.4, which the measurement's interpretation
   depends on).

Acceptance: `cargo test -p plurxd media_sessions` green;
`git diff --stat crates/plurxd/src/media_sessions.rs` shows no change to
`ROUTE_CACHE_TTL`, `MAX_ROUTE_CACHE_ENTRIES` or any `lock()` ordering;
`curl -s $HOST/metrics | grep plurx_media_session_route_` shows the new
families.

### 5.5 M6 — the decision (`core/route-cache-decision`)

No code until §6.4's measurement is in the PR body. Then one of the three
outcomes in §3.5, each with its own tests. If the outcome is "nothing to
fix", the PR is a docs-only change: this file's §7 records the numbers and
the board row closes.

Acceptance: whichever outcome, the PR body contains the measurement table,
and the three authority-change tests (release, replacement, fencing) exist
and pass if and only if a TTL change is proposed.

## 6. Verification and rollout

### 6.1 Lanes

Per milestone the focused `cargo test` above, then `make unit` once before
un-WIP. `make validate-staged` before every push. M3 edits
[API.md](../API.md), so the operations doc-route test must pass. The
golden corpus is a Rust test in `crates/plurx-compat-plex/tests/`, so it
runs under `make unit` from M1 onward.

### 6.2 Rollout

M1 to the fleet with no user-visible change. M2 to `lab1` first, with one
Kodi library refresh watched if a Kodi box exists (§5.0), then the fleet.
M3 to `lab1` and held there for a full Kodi refresh before the fleet,
because it is the only change here a client can see. Nothing alters recipe
identity, argv fingerprints or cache digests. The ordering tie-break
changes no stored data; it changes which row comes first among equals in
the web grid too, which is a cosmetic difference worth naming in the PR.
Rollback is the previous `sha-` image.

### 6.3 M0 — the census, and what only the fleet can answer — GPT prompt

```text
On media1, the node clients actually connect to:
1. Does anything call the Plex façade at all? Read the access log for the
   last 30 days (journalctl -u plurxd --since "30 days ago", or the
   reverse-proxy log if one is in front) and count requests whose path
   starts with /library/, /identity, /:/timeline or /hubs/search. Report
   the count, the distinct paths, and the busiest day.
2. For each such request, report the User-Agent and any X-Plex-Product /
   X-Plex-Client-Identifier header value, aggregated — I want to know
   WHICH client, not who. If the log strips headers, say so.
3. curl -s http://media1:32400/metrics | grep plurx_plex_ and paste it
   (this family exists only once PR <M1 number> is deployed; if the grep
   is empty on the current build, say so).
4. If and only if a Kodi or PlexKodiConnect box exists on the LAN: time a
   full library refresh from it against media1 — start it, note the wall
   clock at start and at "refresh complete", and report both, plus the
   number of items in the library. Do this three times. This is the
   before-measurement for C4 and there is no substitute for it.
5. Say plainly, in one sentence, whether any Plex-family client is in
   regular use.
Report exact values. Do not restart anything.
```

### 6.4 The C9 measurement protocol (F-core-11's "measure first")

On `media1`, on the build carrying M5, with nothing else playing:

1. Record the three `plurx_media_session_route_*` families as a baseline.
2. Start four concurrent playbacks — two web, one Apple TV, one Android —
   of four different titles, two of them rolling HLS (so the segment GET
   rate is real).
3. Let them run ten minutes. Seek twice on each. Stop one and start a
   fifth, so a release and a fresh activation are both in the window.
4. Record the three families again, and compute: store reads per active
   session per second; the lock-wait p50/p95/p99; the prune-sweep size
   p50/p95; and cache entries against the 4,096 ceiling.
5. Put that table in M6's PR body. The claim M6 may make is the one the
   table supports — including "no claim", which closes the appendix.

## 7. Open questions

1. **Kodi refresh latency is still unmeasured, and this plan cannot
   measure it.** §6.3 step 4 is the only source. Batching three store
   calls instead of 5,000 will reduce server time; whether a Kodi refresh
   is dominated by server time, by Kodi's own XML parsing, or by artwork
   fetches is unknown. M2's PR may claim the query-count reduction and
   nothing more.
2. **Which client, and therefore which attributes matter.** The corpus in
   §3.3 covers what the mapper emits today. PlexKodiConnect reads more
   attributes than Kodi's own Plex add-on. If §5.0 names PKC, M1 should
   grow cases for the attributes PKC reads — which needs one capture from
   a real PKC install, and that is a §5.0 follow-up, not a guess.
3. **Tier 2 is still deferred.** [API.md](../API.md) §20 records that
   plex.tv emulation — what Infuse, VidHub, Symfonium and the official
   Plex apps would need — is not built. Nothing here changes that, and
   paging does not bring it closer.
4. **The negative cache.** §3.5 notes that the one-second cache holds
   `None` as well as `Some`. A random-UUID prober benefits from the
   negative half and an activating session is harmed by it (the
   miss-before-activation race that `cache_queried_route_result`'s
   generation check exists to close, `:2264-2290`). M6's measurement
   should report the two halves separately; if it cannot, say so rather
   than reasoning about a combined number.
5. **`MAX_CONTAINER_SIZE = 5_000`.** Inherited from the current literal.
   Whether a client ever asks for more, and what the p99 of
   `plurx_plex_container_items` turns out to be, decides if it should
   shrink. No change without that number.

---

## 8. M0 — the census, run 2026-09-23, and what it decided

**Verdict: do not build M1–M3.** Not "the census said no" — the census cannot
be completed on the instrumentation this repository has today, and every datum
that does exist points at no Plex-family client. What this session built is the
one instrument that makes the question answerable, and nothing else.

### 8.1 Why §6.3's prompt cannot be run as written

§6.3 step 1 says to read the access log. **There is no access log.** That is
C-08's finding (`docs/server/OBSERVABILITY-BASELINE.md` §2.1: the one
`TraceLayer` has a `make_span_with` and nothing else, so the span's default
`on_response` logs at DEBUG and the default filter is `info`), and it was
confirmed on a real node: `docker logs plurxd` on nuc4 returns 243,801 lines
for the 14 h since that process started and **not one** is a request line.

Nor can a metric answer it. `http_route_group` folds
`/library/sections/{id}/all`, `/library` and `/library/sections` into
`route_group="library"` beside the native `/api/v1/libraries*` routes, and
`/library/parts/{file_id}/{mtime}/{name}` into `route_group="playback"` beside
the native media routes. There is no series anywhere that separates a façade
request from a native one.

And the façade writes no playback telemetry at all: `plex::part` resolves the
file and calls `stream::serve_file_range` directly, with no session and no
`playback_events` row. **A Kodi or PlexKodiConnect box could have streamed
every night for a month and left no trace in any surface this repository
exposes.** That is the finding, and it is why §6.3 step 3 — "curl /metrics |
grep plurx_plex_" — is the step that actually settles this, and why it says
the family exists only once the census counter is deployed. The plan's M0 was
circular: it gated M1 on a measurement only M1 could produce.

### 8.2 What the available evidence does say

All of it from nuc4 (192.168.4.8), a voter running the owner's real library,
read-only, 2026-09-23. It is the only fleet node this session was authorised to
read, and that bound is part of the result.

| Source | Value |
|---|---|
| `plurx_http_route_seconds_count`, all six roles, for `auth`, `home`, `library`, `item`, `search`, `playback` and `other` | **0**, every cell, over the 14 h 07 m since the process started at 2026-09-22T20:22:15Z. Only `settings` (7,670) and `cluster` (66,551) moved. |
| `plurx_ttff_ms_count{method}` | **0** for `direct_play`, `remux`, `transcode` and `unknown`. (Read before the family gained its `client` label in C-08 M5; the same reading on a current build is `sum by (method) (plurx_ttff_ms_count)`, since each method is now seven series.) |
| `playback_events` in the node-local sidecar, 2026-08-25T01:48Z → 2026-09-23T11:12Z (29 days), 41,048 rows | Every row that carries a client class names a first-party plurx client: `Chrome` 196 (last 2026-09-04), `Android Media3` 62 (last 2026-09-11), `Apple AVPlayer` 38 (last 2026-09-14), `Safari` 4 (last 2026-09-02). 40,748 rows carry no class. **No Plex-family client appears at all.** |
| `X-Plex-Container` anywhere in the tree | Absent outside the review documents — the plan's own correction 2, re-verified. |
| `fn files_for_items` anywhere in the tree | Absent — the plan's correction 1, re-verified. |
| Façade routes registered in `router()` | 14, exactly as §3.4 says. |

Two honest limits on that table, both load-bearing:

- The 29-day playback census is **silent about the façade**, for the reason in
  §8.1. It is strong evidence about which clients play through the native API
  and no evidence at all about which clients browse or stream through
  `/library/`.
- nuc4 served no user traffic whatsoever in the 14 h window, and its most
  recent playback event is 2026-09-14. Its zeros are the zeros of an idle
  node, not of a fleet. Another node may be the ingress.

### 8.3 The recommendation

**Do not build M1 (the golden corpus), M2 (batched metadata) or M3 (container
paging) now.** C4's own priority note says the work "only matters if Kodi/PKC
is in use", nothing in this repository can currently show that it is, and the
repository's own client story has moved since REQ-PLEX-1 was written:
`docs/CLIENTS.md` §3 describes Kodi-family clients as what covers living rooms
"in the meantime", and `clients/android` and `clients/apple` now exist and ship
to physical devices. Building a paging layer, a batched store helper on both
backends and a nine-case golden corpus for a façade with no demonstrated
caller is the speculative work §5.0 exists to prevent.

The N+1 in `section_all` is real and the plan's description of it is accurate.
It is not urgent, and this document remains the ready plan for the day the
counter says it is.

### 8.4 What was built instead, and its boundary

One measure-only instrument, `plurx_plex_requests_total{handler,outcome}`:
fourteen bounded handler labels by four bounded outcomes, 56 fixed series, no
ids, no titles, no tokens. A layer over the façade sub-router only, so no
native request can reach it, plus one recording call inside `root_dispatch`'s
Plex branch because `/` serves both the web app and the capabilities container
from one handler and counting it by path would count every page load as Plex
traffic. **No handler was changed and no response byte moved.**

Two deliberate departures from §3.4:

1. **A fourth outcome, `unauthorized`, beside `ok`, `not_found` and `error`.**
   Every façade route but `/` and `/identity` requires a plurx token presented
   as `X-Plex-Token`, so a Kodi or PlexKodiConnect box that is configured but
   not yet paired produces 401s and nothing else. For a census "somebody tried"
   is the single most interesting thing that can happen, and folding it into
   `error` would hide it.
2. **`plurx_plex_container_items` was not built.** It answers "do clients page,
   and how big a page do they ask for", which is a question about clients that
   exist. It costs nothing to add on the day one does.

### 8.5 How to close this row

`plurx_plex_requests_total` is an in-process counter. It starts at zero every
time `plurxd` starts, it lives on one node, and nothing in this repository
scrapes or persists it. **One read is evidence about one node since that
node's last restart, and about nothing else.** The rule below is built around
that. The rule this PR first shipped — leave it a week on "the node clients
actually connect to", then `curl` once — was not, and the adversarial review
of PR #462 showed the failure: a Kodi box that browsed on day 2 closes C-07 as
`abandoned` if the node restarted on day 5, and on this campaign's deploy
cadence a week-old process is unlikely. nuc4 itself restarted between the
census read and the review.

**Read every node, with its uptime.** A Plex client can be pointed at any
node's HTTP port (32400 unless `PLURX_HTTP_PORT` moved it), and §8.2 shows
nobody knows which node clients use, so there is no single node to read.

```sh
for host in <every plurxd node in the fleet>; do
  printf '%s %s ' "$host" "$(date -u +%FT%TZ)"
  curl -s "http://$host:32400/metrics" \
    | grep -E '^(plurx_uptime_seconds|plurx_plex_requests_total)' \
    | grep -v '^plurx_plex_requests_total.* 0$' | tr '\n' ' '
  echo
done
```

Record every line in the execution log. A read at time *t* of a node reporting
`plurx_uptime_seconds` *u* covers that node over [*t − u*, *t*], and nothing
before *t − u*.

**What each outcome means.**

- Any non-zero cell, on any node, in any read, is a caller — whatever the
  window. The three outcomes below say what kind.
- Zeros close the row **only with coverage**: for every node, the union of its
  reads' intervals must cover the same seven consecutive days with no gap. A
  node that restarts between two reads leaves a gap from the earlier read to
  the restart — whatever it counted in that stretch is gone. A gap makes the
  week **inconclusive, not zero**: extend the window until seven gap-free days
  exist, never close on a partial one. In practice this means reading every
  node daily and immediately before any deploy or restart of it; a deploy
  that does not read first opens a gap on every node it touches.
- A node the census cannot see through is a gap too. `cluster_capacity_gate`
  is layered outside the whole router and answers 503 before routing, so the
  census never sees its refusals: every façade request on a node in
  maintenance or fenced, and on a learner every façade route but `/`,
  `/identity` and `/library` (the only ones `learner_route_eligible` admits).
  Only `mutable_media_serving_gate`'s refusals are inside the census and
  counted. Any stretch a node spent in maintenance, fenced, or as a learner is
  a gap for that node.
- With seven gap-free days of zeros on every node → the façade has no caller.
  Close C-07 as `abandoned: no façade traffic`, keep this plan, and revisit if
  that ever changes.
- `outcome="unauthorized"` moving → somebody is trying and cannot get in. That
  is a pairing problem, not a paging one, and it is the more urgent finding.
- `handler="section_all"` or `handler="children"` moving with `outcome="ok"` →
  a client is browsing. Re-open C-07 at M1 and build the plan as written; the
  §6.3 step 4 Kodi refresh timing is then both possible and required, because
  it is the before-measurement M2 may not claim without.

A persisted or scraped counter would make gaps impossible, and was
considered. It was not built: it puts a file or store write on the façade's
request path, or a scrape target on every node, for a measure-only instrument
whose question a daily read can answer. If the reads prove impractical in
practice, persisting the 56 cells is the next step — not closing the row on a
partial window.

### 8.6 The C9 appendix (M5, M6) was not opened

M5 is measure-only and independent of this census, so it was available. It was
not taken because its measurement, §6.4, requires four concurrent real
playbacks on the ingress node for ten minutes with seeks, a release and a fresh
activation — an operation this session could not stage and could not ask for
cheaply. Landing M5's three metric families without that protocol produces
instrumentation nobody is scheduled to read, and §3.5's own framing is that the
measurement, not the metric, is the deliverable. The appendix is unclaimed and
unblocked.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M0 — the census | [PR #462](http://192.168.4.7:3000/noirr/plurx/pulls/462) | Run against nuc4, read-only. **Could not be completed as §6.3 specifies**: there is no access log, no metric separates the façade from the native API, and `plex::part` writes no playback telemetry, so façade traffic is invisible by construction. Every datum that does exist — 29 days of node-local playback events naming only Chrome, Safari, Android Media3 and Apple AVPlayer, and 14 h of zero in every user-facing route group — points at no Plex-family client. Numbers and limits in §8.2. Verdict: **do not build M1–M3**. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M0 — the census instrument | [PR #462](http://192.168.4.7:3000/noirr/plurx/pulls/462) | `plurx_plex_requests_total{handler,outcome}` from §3.4, plus a fourth outcome `unauthorized` (§8.4). Measure-only: one layer over the façade sub-router, one call in `root_dispatch`'s Plex branch, no handler touched. Five tests, one of which reads the façade's route registrations out of `router()`'s own source so a new façade route without a label fails the build. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M1, M2, M3 | — | **Deliberately not built.** §8.3. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M5, M6 (C9 appendix) | — | **Not opened.** Unclaimed and unblocked; reason in §8.6. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M0 — review of PR #462 | [PR #462](http://192.168.4.7:3000/noirr/plurx/pulls/462) | Two findings, both fixed. (1) The census counter was a process-wide `static`, so three census tests asserting exact deltas raced every other test sending façade traffic (the reviewer measured 34 failing runs in 400). It is now a `PlexCensus` held on `AppState`, so each router counts only its own requests; `each_router_counts_only_its_own_facade_requests` pins it. (2) §8.5's closing rule read one node once after a week, which a restart or an unread node silently turns into a false "no caller". It now requires every node, with `plurx_uptime_seconds` on every read, seven gap-free days, and treats a restart between reads, maintenance, fencing and learner time as gaps; the layer comment that claimed refused requests are counted now names the refusals it cannot see. Merged main (`99d4abf8c`); `process-capable-launch-method` re-measured on the merged tree. |
| | | | | | `needs:` seven gap-free days of `plurx_plex_requests_total` read from every node with its uptime, then §8.5. |
