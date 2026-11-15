# Pipeline contract: Offload and Get it back

**Status:** built · **Parent:** [PIPELINE-CONTRACT.md](PIPELINE-CONTRACT.md) ·
**Wire:** [INTEGRATION.md](../INTEGRATION.md) §1.11, [API.md](../API.md)

Curator can delete a title's files to free disk and keep the title. This is the
cinema side of the amendment (built; Curator side open). It builds on the
probe-only scan of [PIPELINE-CONTRACT-REMOVED-PATHS.md](PIPELINE-CONTRACT-REMOVED-PATHS.md).

## Offload (Curator to cinema)

The same probe-only `POST /api/v1/scan` as a Re-match's moved sub-deliveries,
with the deleted paths in `"offloaded_paths"` (at most 64, each under `path`,
the limits of `removed_paths`). Cinema removes each path's file records and
keeps the item, its metadata and its watch history. It sets `offloaded_at`
**only on items left with no files** (an item with another copy elsewhere stays
on disk), in the same transaction as the removal, so no prune can see the item
fileless and unmarked. Each path is reported in `files` as
`offloaded{item_id,item_ids,offloaded_items}` (`offloaded_items` counts items
now flagged), or as `gone`, `unknown`, `present`, `retained` as for removed
paths. Kinds cinema does not keep (books, audiobooks, photos, folders) are
removed as for `removed_paths` and reported `gone`; an `offloaded` report never
names an item that no longer exists.

A folder emptied by the offload that sits on its parent's filesystem is
`retained{item_id, code:"root_empty", retryable:false}`: cinema keeps the record
until a file lands or Curator re-acquires. (A full scan would prune it, so none
is admitted.)

The sidecar stores `{"offloaded_path": ...}`, which older binaries refuse, so
**every cinema node before Curator sends `offloaded_paths`**. The connection
test lists `"offload"` in `features`; Curator sends `offloaded_paths` only then
(and always the `offload` event with them) and otherwise sends plain
`removed_paths`, after the dialog said cinema will drop the title and its watch
history (advice, never a gate). An older node's full scan prunes offloaded
items.

No prune deletes an offloaded item: every item-deleting statement uses one
shared predicate, and the store clears `offloaded_at` whenever a file attaches.

## Files return

A normal import scan. For a curator-notified scan whose request names ids,
placement first asks for an offloaded **movie or episode** with those ids
(episodes by the show's TMDB id, season and episode), scoped to the request's
own expected files, so a returning file lands on the kept item even when the
title differs (a TMDB title like "Tar" against the folder's). Books, audiobooks
and photos are not placed by ids. Only then do the usual lookups run.

## Get it back (cinema to Curator)

An admin presses Get it back; cinema makes one synchronous call to Curator's
`POST /api/v1/webhooks/plurx` (`monarr.url`, `monarr.api_key`, `X-Api-Key`):

```json
{"event":"reacquire","kind":"movie|show|season|episode","tmdb":603,"imdb":"tt0133093",
 "season":1,"episode":2,"user":"paul"}
```

Ids are the watched event's: a movie's own; otherwise the **show's** TMDB/IMDb id
plus `season` and `episode`. Curator matches by ids only and answers
`{matched, mediaItemId, state, message?}`. `state` is `searching`,
`downloading`, `importing`, `on_disk`, `not_offloaded`, `not_found`,
`offloading` (Curator is still deleting the files: "try again shortly") or
`offloaded` (re-acquire did not start; Curator's `message` is shown). Any other
state is shown as "Curator answered <state>", never as success. No ids is
`409 no_external_ids` before Curator is called; an answer without `state` is
`502 curator_too_old`; an unreachable Curator is `502 curator_unreachable`.
Asking twice is harmless.

## Progress

`GET /api/v1/items/{id}/offload-status` sends `event:"reacquire_status"` and
returns Curator's state untouched, persisting nothing. A `409 not_offloaded`
means the item came back: clients reload it. Unreachable reads "Offloaded -
curator unreachable". Clients poll every 15 s while the page is visible.
