# Custom tags and tag-driven Home rows — plan and build contract

**Status:** open — building on `effort/custom-tags` · **Updated:** 2026-11-15 · **Owner:** Claude Opus 5.5 (session_01BCWLA1Vd3M7YPaN8tUssxh)
**Verified against:** `origin/main @ 29b35e713`. Line numbers drift; re-anchor with `scripts/ripwire` and `rg`.

## 0. What and why

The owner wants to put his own tags on movies and shows (example: tag every
gay-themed title), sort, filter and search by them, and put a row on Home built
from tags, the way Continue Watching and Recently Added are rows today.

The owner made three rulings on 2026-11-15:

1. Tagging is a **grantable per-user permission**. Admins can always do everything.
2. **Manual tagging and the suggestions review queue both ship** in v1.
3. Watchlist and Favorites become **movable Home rows** like every other row.

Renderings: the owner's "Custom tags & Home rows" design canvas (six screens).

## 1. Why the existing pieces are not reused as storage

- `items.tags` is provider/NFO input for home-video items, and the classifier
  reads it (`SOURCE` in `store/classification.rs`; the
  `classification_source_changed` trigger). User tags stored there would be
  rewritten by NFO seeding and would invalidate classifications on every edit.
  User tags therefore get their own tables, keyed by item id.
- `items.genres` belongs to the provider, and enrichment replaces it.
- The classifier keeps TMDB keywords in `media_classifications.terms`, in both
  stores. That makes it the **suggestion source**, not the tag store.

Home-video NFO tags stay where they are in v1. Moving them into the new tables
is listed in §9.

## 2. Data model

The new tables are added as two **independent schema extensions**, built the
same way as `library_annotations`: an `.sql` file, a schema wrapper, SQLite
registration, hiqlite install and migration, the hiqlite import list, the
consistent-read census and the contract-test fixture marker. Every read and
write goes through the store traits and their SQLite and hiqlite twins, so
nothing goes around the cluster layer.

### 2.1 `user_permissions` (grantable permissions)

```
user_permissions(user_id → users ON DELETE CASCADE, permission TEXT CHECK(permission IN ('manage_tags')),
                 granted_by INTEGER, granted_at_ms INTEGER, PRIMARY KEY(user_id, permission))
```

This is generic on purpose: the next grantable right is a new `CHECK` value,
not another table. `User` gains no field; permissions are looked up per request
(`PermissionStore::user_permissions`). Admins implicitly hold every permission
(`effective_permissions`).

### 2.2 `library_curation` (tags, suggestions, rows, Home layout)

```
library_curation_tags(id PK, name, name_key, owner_user_id NULL=household → users CASCADE,
                      color TEXT, suggest_terms TEXT json-array, archived_at_ms NULL,
                      created_by, created_at_ms)
  UNIQUE INDEX (COALESCE(owner_user_id,0), name_key)
library_curation_item_tags(tag_id → tags CASCADE, item_id → items CASCADE, added_by, added_at_ms,
                           source CHECK IN('manual','suggestion'), PRIMARY KEY(tag_id,item_id))
  INDEX (item_id)
library_curation_rejections(tag_id → tags CASCADE, item_id → items CASCADE, rejected_by, at_ms,
                            PRIMARY KEY(tag_id,item_id))
library_curation_rows(id PK, owner_user_id NULL=household → users CASCADE, title, spec TEXT json,
                      created_by, created_at_ms, updated_at_ms)
library_curation_home(user_id PK → users CASCADE, layout TEXT json, updated_at_ms)
```

- `name_key` = NFKC + Unicode lowercase + trim + collapsed whitespace
  (`plurx_core::curation::tag_key`). The display name is kept as typed.
- **Shows carry tags.** Rows, grids and search list top-level titles (movies,
  shows), so tagging a show covers it everywhere a show appears. Tagging
  episodes and seasons directly is deferred (§9).
- **Row spec** (`curation::RowSpec`, validated server-side):
  `{all_of:[Term], any_of:[Term], none_of:[Term], libraries:[id], kinds:["movie"|"show"], sort, unwatched:"any"|"first"|"only", limit}`
  where `Term = {"tag": id} | {"genre": "Drama"}`. `sort` ∈
  `recently_tagged | added | released | title | shuffle_daily`. Limit 1–50,
  default 20. At most 16 terms per group.
- **Home layout** (`curation::HomeLayout`): an ordered list of
  `{key, hidden}`. A key is a built-in (`continue_watching`, `next_up`,
  `recently_added`, `watchlist`, `favorites`, `coming_soon`, `libraries`) or
  `row:<id>`. A user with no stored layout gets the **household default**
  (settings key `home.default_layout`, admin-set). When there is none of
  either, the built-in default reproduces today's Home exactly. Unknown or
  deleted keys are dropped on read. Rows visible to the user that the layout
  doesn't list are appended (household rows at their default position), so a
  newly published household row shows up without anyone editing their layout.

### 2.3 Visibility (enforced in the store queries, never in clients)

A user sees household tags and their own private tags, and household rows and
their own rows. Every tag-bearing query takes the viewer's `user_id`. Private
tags never appear in another user's results, facets, search, rows or detail.

**Never leaves the server:** shared-library federation (`shared_library.rs`
`shared_item`), Jellyfin (`jellyfin.rs` `item_dto`) and Plex (`plex.rs`
`metadata`) build their own DTOs and do not carry curation tags. A test pins
each of them (§7).

## 3. Permissions

| Action | Any user | `manage_tags` | Admin |
|---|---|---|---|
| Create, rename or delete **own private** tags; apply them | ✓ | ✓ | ✓ |
| Create, rename, merge, archive or delete **household** tags | | ✓ | ✓ |
| Apply or remove household tags on items; review suggestions | | ✓ | ✓ |
| Create own rows; edit own Home layout | ✓ | ✓ | ✓ |
| Publish **household** rows | | ✓ | ✓ |
| Set the household default Home layout | | | ✓ |
| Grant or revoke `manage_tags` | | | ✓ |

The table needs a new extractor that checks a permission:
`RequirePermission<ManageTags>`, which accepts an admin or a holder.

## 4. HTTP API (`/api/v1`, documented in `docs/API.md`)

| Method · path | Purpose |
|---|---|
| `GET /tags` | The tags visible to the caller, with counts and `scope` (`household`/`private`) |
| `POST /tags` | Create `{name, color?, scope}`; household needs `manage_tags` |
| `PATCH /tags/{id}` | Rename, recolour, archive or unarchive, set `suggest_terms` |
| `DELETE /tags/{id}` | Delete; the reply names the rows that used it |
| `POST /tags/{id}/merge` | Merge `{into}`: memberships move, the source is deleted |
| `GET /items/{id}/tags` | The visible tags on one item |
| `PUT /items/{id}/tags` | Set the caller-editable subset to exactly `{tag_ids}` |
| `POST /tags/{id}/items` | Bulk apply/remove `{add:[item], remove:[item]}`, ≤ 5000 per call |
| `GET /tags/{id}/suggestions` | The review queue: items whose classifier terms match `suggest_terms`, not tagged, not rejected, with the matched terms |
| `POST /tags/{id}/suggestions` | `{accept:[item], reject:[item]}` |
| `GET /rows` · `POST /rows` · `PATCH /rows/{id}` · `DELETE /rows/{id}` | Saved rows |
| `GET /rows/{id}/items?offset&limit` | A row's full result ("See all") |
| `GET /home/layout` · `PUT /home/layout` | The caller's ordered entries |
| `PUT /home/default-layout` | Admin: the household default |
| `GET /users/{id}/permissions` · `PUT /users/{id}/permissions` | Admin: grant/revoke |

Changes to existing routes:

- `GET /libraries/{id}/items` gains `tag=<id>` (repeatable), `tag_mode=any|all`
  and `not_tag=<id>` (repeatable), plus `sort=recently_tagged`.
- `GET /search` understands `tag:name`, `tag:"two words"` and `-tag:name`
  tokens. They are stripped from the text query, resolved against the caller's
  visible tags and applied as a membership filter.
- `GET /hubs` gains `layout` (the resolved, ordered entries) and `rows`
  (`{id: {title, items, total}}` for the visible custom rows in the layout).
  Home stays at its current three requests, and old clients ignore the new
  fields.

## 5. Clients

- **Web**
  - Home renders `hubs.layout` in all four layouts (classic, catalog,
    theater, meridian).
  - A **Customize Home** panel to reorder, hide and add or edit rows, with a
    rule editor and a live count.
  - Detail page: a tag chip row with an add-tag typeahead (create on Enter,
    Just me / Household).
  - Library grid: a Tags filter with chips and Any/All, Select mode with a
    tri-state **Tag…** popover and **Select all N matching**, and the
    recently-tagged sort.
  - Tag pages: Tagged / Suggested / Rejected / Settings.
  - Settings → Users: the `manage_tags` grant.
  - Search hints `tag:`.
- **Apple (iOS, iPadOS, tvOS):** Home renders `hubs.layout` and `rows`;
  `TvHomeLayout` keeps its two presentation policies but takes its order from
  the layout. Detail: Tags… checklist (toggle existing tags; create a private
  tag on iOS).
- **Android (phone, Google TV):** the same as Apple.
- Customizing Home, bulk tagging and the review queue are web-only in v1 (§9).
  Phones and TVs render the result.

## 6. Build order and file ownership (one effort branch, one PR)

| Task | Owner | Files |
|---|---|---|
| T1 Core: extensions, `curation` domain types, `PermissionStore` + `CurationStore` traits, SQLite + hiqlite twins, contract tests | Opus (this session) | `crates/plurx-core/src/{curation.rs, store/user_permissions*.{sql,rs}, store/library_curation*.{sql,rs}, store/sqlite/curation.rs, store/hiqlite_curation.rs}` + registrations |
| T2 HTTP: handlers, extractor, browse/search/hubs changes, API.md | Opus | `crates/plurxd/src/http/{curation.rs, extract.rs, browse.rs, users.rs, mod.rs}`, `docs/API.md` |
| T3 Web | Opus | `crates/plurxd/src/web/**` (new `curation/*.js` registered in `WEB_ASSETS`) |
| T4 Apple | Sonnet sub-agent, Mac runner | `clients/apple/**` + version bump |
| T5 Android | Sonnet sub-agent | `clients/android/**` + version bump |
| T6 Review, fast lane, merge | Opus + Fable review | — |

## 7. Tests that must reject the wrong code

- **Contract** (both stores, `--features hiqlite-contract-tests`):
  - Visibility: user B never sees A's private tag in tags, item tags, filters,
    suggestions or rows.
  - Name-key uniqueness per scope.
  - Merge moves memberships.
  - Cascades on item, tag and user delete.
  - Rejections stick: suggestions exclude rejected and already-tagged items.
  - Row evaluation for all-of, any-of, none-of, kinds, libraries and unwatched.
- **HTTP:**
  - Permission matrix (§3) per route.
  - The `tag:` search parse.
  - `hubs.layout` with no stored layout equals today's order.
  - Jellyfin, Plex and federation DTOs carry no curation tag (asserted against
    a tagged fixture item).
- **Prove the tests:** reinstate a wrong implementation once (for example,
  drop the owner filter) and record which tests fail.

## 8. Execution log

| When | Who (model · session) | What |
|---|---|---|
| 2026-11-15 | Claude Opus 5.5 · 01BCWLA1Vd3M7YPaN8tUssxh | Proposal and renderings; owner rulings; plan written; T1 started |

## 9. Deferred (not v1)

Customizing Home on TV and phone · bulk tagging and the review queue on phone ·
hiding a tag from specific profiles · tags shown to Jellyfin/Plex apps
(per-tag opt-in, read-only) · a channel from a row · grouping by tag ·
rule-backed automatic tags · tagging episodes and seasons directly · moving
home-video NFO tags into curation tags.
