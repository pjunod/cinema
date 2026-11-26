# Custom tags — native client contract (Apple, Android)

**Status:** open · **Updated:** 2026-11-15 · **Owner:** Claude Opus 5.5 (session_01BCWLA1Vd3M7YPaN8tUssxh)
**Parent plan:** [CUSTOM-TAGS-PLAN.md](CUSTOM-TAGS-PLAN.md) §5. The server and web parts are built on `effort/custom-tags`.

This page is for the agent that builds the Apple or Android part. It says exactly
what the server answers and what each client must draw. Phones and TVs **render**
the reader's Home order and custom rows, and let a person toggle tags on a title.
Customizing Home, bulk tagging and the suggestions queue stay web-only in v1.

## 1. Home order and custom rows

`GET /api/v1/home/rows` (authenticated, same token as `/hubs`). Fetch it **in
parallel** with the requests Home already makes; never serialize it behind them.

```json
{
  "layout": [
    {"key": "row:12", "kind": "row", "row_id": 12},
    {"key": "continue_watching", "kind": "builtin", "row_id": null},
    {"key": "recently_added", "kind": "builtin", "row_id": null}
  ],
  "rows": {
    "12": {"title": "Gay-themed movies & shows", "total": 38, "items": [ /* ItemDto, same shape as /hubs items */ ]}
  }
}
```

- `layout` lists only **visible** entries, in the reader's order. Built-in keys are
  `continue_watching`, `next_up`, `watchlist`, `favorites`, `recently_added`,
  `coming_soon`, `libraries` (the per-library preview rows). A client draws each
  built-in it already has, in this order, and **leaves out** any built-in that is
  not in the list (the reader hid it). Ignore a key the client does not know.
- A `row:N` entry is drawn as a poster rail titled `rows["N"].title` from
  `rows["N"].items`; selecting a poster opens the normal detail screen. When
  `total > items.length`, the rail ends with a "See all" tile that opens
  `GET /api/v1/rows/N/items?offset=0&limit=60` (`{items,total,offset,limit}`) as a grid.
- Server older than this feature (404) or a request error: draw Home exactly as
  today. A slow `/home/rows` must not delay the first paint of the built-ins;
  apply the order when it lands, without blanking what is already shown.
- tvOS: `TvHomeLayout` keeps its presentation policies; only the **order and
  membership** of sections come from `layout`.

## 2. Tags on the detail screen

Movies and shows only (not seasons or episodes).

- `GET /api/v1/items/{id}/tags` → `{"tags":[Tag]}`: the tags on this title the
  reader can see. Show them as small chips under the title/badges when non-empty.
- `GET /api/v1/tags` → `{"tags":[Tag],"can_manage_household":bool,"colors":[...]}`.
- `Tag = {"id","name","scope":"household"|"private","color","suggest_terms":[],"archived":bool,"item_count","editable":bool}`.
  Colour names map to: violet `#a78bfa`, blue `#60a5fa`, teal `#2dd4bf`, green `#4ade80`,
  lime `#a3e635`, amber `#fbbf24`, orange `#fb923c`, red `#f87171`, pink `#f472b6`, slate `#94a3b8`.
- A **Tags…** action (button on phone/tablet; a focusable button on TV, and on TV
  also the long-press / Menu action on a poster in a Home rail if the client
  already has a poster context menu) opens a checklist of the tags with
  `editable && !archived`, ticked where the title has them. Private tags show a
  lock glyph. Toggling sends
  `PUT /api/v1/items/{id}/tags` `{"tag_ids":[...]}` with **every ticked editable
  tag** (the server leaves tags the reader cannot edit untouched) and redraws from
  the response `{"tags":[Tag]}`.
- Phone/tablet only: a "New tag" field in the checklist creates a private tag
  (`POST /api/v1/tags` `{"name","scope":"private"}` → `201` Tag; `409` means the
  name exists — select that tag instead) and ticks it.
- `403` from the PUT: show "You can't change that tag" and redraw.

## 3. Rules for the build

- Work only under your client's folder (`clients/apple/**` or `clients/android/**`).
  Bump the client's version/build as `make paperwork` / `tests/validation/test_mobile_versions.py` requires.
- Unit-test the decoding of `/home/rows` (unknown keys ignored, hidden built-ins
  left out, missing endpoint falls back) and the PUT body (only editable ticked ids).
- Compile and run your client's unit tests on its build host before handing back.
- No feature flags: the feature is on.
