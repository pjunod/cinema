# Scan identity — prevent split items and preserve watch state

**Status:** M1 prevention merged to the effort; M2 hints built and
fixture-verified; M3 repair remains unbuilt ·
**Written:** 2026-09-19 · **Revised:** 2026-09-19 ·
**Incident:** reference show S, season 5.

Companion to [Integration](../INTEGRATION.md) and the
[targeted-scan API](../API.md#63-the-targeted-scan-seam). This document owns
the root causes and revised fix proposal. It incorporates Fable's supplied
review, replacing the first draft's source-claims schema and backfill with
lookup through existing file ownership. M1 now preserves existing-path
ownership and performs bounded directory lookup on both stores; the guarded
hint and repair stages have not run, and no production repair is authorized.
Read §4–§7 as the staged contract and §8 as review disposition.

[Sol's implementation plan](SCAN-IDENTITY-IMPLEMENTATION.md) owns the detailed
task order, parser rules, store/API interfaces, test commands and release
acceptance. Its explicit build decisions refine this proposal; use this
document for the incident evidence and review provenance.

## 1. Verdict — display-title matching causes splits and can erase watch state

For newly placed shows and movies, the scanner matches parsed titles against
mutable display metadata. A provider rename can break that match. The
observed `(US)` suffix is one instance; punctuation changes and reworded
provider titles produce the same class of failure.

There are two distinct paths:

```text
new path / second version              changed bytes at an existing path
           │                                         │
           └─────────────▶ title-based placement ◀───┘
                                  │
                       enriched title does not match
                                  │
                         new item / new show tree
                                  │
             ┌────────────────────┴───────────────────┐
             ▼                                        ▼
       separate library card              existing file changes item owner
                                                      │
                                          original episode has no files
                                                      │
                                          full scan prunes original item
                                                      │
                                          watch_state cascades away
```

The first repair priority is preserving an existing file's item association
when its bytes change. For new files in recognized show/movie directories,
resolve identity through existing `files.path` ownership before falling
back to title/year. This needs no new identity table or legacy backfill.

The watch-state loss is execution-proven in Fable's scratch reproduction and
supported by the inspected production code path. It is not a claim that a
specific user's historical watch rows have been proved lost in production.

## 2. Evidence — distinguish original observations from review measurements

### 2.1 Revisions and test provenance

The initial read-only inspection on 2026-09-19 observed container revision
`a2d9c2fb7b26e142a39b70da8cc0e57bbbdf878a`, with local checkout
`a603b4e26647ab0aa71564e7f4a5d624f9a87ff0`. Fable reports a subsequent
redeploy to `8663d6c0`, build `v0.3.0-2846-g8663d6c0`, during review.
These are dated observations, not a claim about the fleet's present revision.

Fable verified the cited parser, scanner, metadata, media-store, publication,
scan coordinator and HTTP scan files as byte-identical across those revisions.
This revision re-inspected their local source behavior; `8663d6c0` is not
available in this local object database, so its equality/deployment evidence
remains attributed to Fable. Reverify the intended build base before coding.

Fable reports four executed probes on a scratch clone: suffix rename,
punctuation rename with a second episode version, changed-file watch loss,
and a control without metadata rename. The prototype's inverted assertions
and 220 `scan::` / `store::sqlite::` unit tests reportedly passed. The local
review artifacts `_showid_review_probe_v2.rs` and `_proto_patch_v1.py` were
read during this revision; they were not executed here or copied into source.
They are review evidence, not merge-ready tests. The prototype leaves the
Hiqlite directory method returning an empty default and does not implement
the movie arm, hint correction or catalogue repair. Its green result does
not qualify those missing pieces.

The original import request and historical worker were not recovered. Both
scan entry points share `record_candidates`, so the failure mechanism does
not depend on recovering that event. No media or production rows were changed
by this documentation revision. Evidence uses neutral titles and root paths.

### 2.2 Original incident snapshot

Reference show S's two rows belong to library `2` and have the same stored
provider title, year `2024`, TMDB ID `261507`, and no IMDb ID.

| Field | Original show | Split show |
|---|---|---|
| Item ID | `669` | `6420086538814441635` |
| Seasons | 1, 2, 3, 4 | 5 |
| Episode rows | 10, 10, 9, 10 respectively | 1 |
| Added at, UTC | 2026-07-22 02:29:18 | 2026-09-17 23:40:22 |

Season 5 is item `4011036022662973033`. Both file generations use the same
show directory; these are neutralized representations of the observed paths:

```text
<tv-root>/<show-dir>/Season 1/Reference Show S (US) - S01E01 - Episode WEBDL-1080p.mkv
<tv-root>/<show-dir>/Season 5/Reference Show S - S05E01 - Episode [WEB-DL 1080p].mkv
```

The actual directory retains `(US)` while the provider title omits it.
The parser prefers that directory above `Season N` for both files; both
produce the same parsed title with `(US)` and unknown year. The filename
suffix difference is therefore not the trigger. The null incoming year
passes the existing year predicate; it does not cause this miss.

Fable reconfirmed those rows and found one watch row on the season 5 episode.
That row must survive the disjoint-season repair unchanged.

### 2.3 Fleet census supplied by Fable

| Measurement | Review observation | Meaning |
|---|---|---|
| Shows with recognized season-directory layout | 159 | Existing file lineage covers nearly all of the 160 shows inspected. |
| Directory/display-title mismatches | 25/159, approximately 16% | Reported latent split candidates: 17 punctuation changes, four suffix/year forms and four other rewordings. |
| Movie folder/display-title mismatches | 130/438, approximately 30% | The same identity weakness warrants movie coverage; no split movie was reported, and seven movies had two files. |
| Equal-provider-ID show groups | Four | Three same-directory duplicate groups; one intentional cross-directory pair. |

These are Fable's snapshot counts, not measurements rerun here. A raw
folder/title inequality is a candidate census, not by itself proof that
every row fails the parser's normalized title/year predicate. Punctuation
and suffix failures were reproduced. Directory naming conventions replace
colons in the reported examples; no universal filesystem ban is assumed.

| Neutral reference | Item IDs / provider | Required disposition |
|---|---|---|
| Show S | `669`, `6420086538814441635`; TMDB `261507` | Reparent disjoint season 5. |
| Show T | `1061`, `5932`; TMDB `103516` | Reparent disjoint season 4 into seasons 1–3. |
| Show U | `279`, `6280`, `6814090868372358311`, `6644432004754260538`, `4443586102535344127`; TMDB `131927` | Consolidate five rows from one directory, including overlapping season/episode numbers and second file versions. |
| Shows V/W | `3823`, `3833`; TMDB `79063` | Keep separate: different directories, both with S01Exx numbering. |

Thus two duplicate groups permit a disjoint-season repair, one needs episode
consolidation, and the fourth group must remain separate. The review's prose
about “three of four” groups refusing the old repair does not match its own
inventory; the table above governs scope.

**Count discrepancy:** the review reconfirms the 40-episode snapshot for S
but later requests 41 episodes in acceptance. It also mentions 17 watch rows
without a complete per-group mapping. Do not fabricate a new episode or use
those totals as fixed assertions. A fresh dry run must enumerate exact IDs,
counts and watch rows, explain changes since the snapshot, and become the
repair's conservation baseline.

### 2.4 Read-only incident queries

Use the authoritative catalogue, not a leftover pre-Hiqlite database:

```sql
SELECT id, library_id, kind, year, tmdb_id, imdb_id, added_at
FROM items WHERE id IN (669, 6420086538814441635);

SELECT sh.id AS show_id, s.id AS season_id, s.season_number,
       COUNT(DISTINCT e.id) AS episodes, COUNT(DISTINCT f.id) AS files
FROM items sh
JOIN items s ON s.parent_id = sh.id
LEFT JOIN items e ON e.parent_id = s.id
LEFT JOIN files f ON f.item_id = e.id
WHERE sh.id IN (669, 6420086538814441635)
GROUP BY sh.id, s.id ORDER BY sh.id, s.season_number;
```

Equal provider IDs are corroborating metadata, not a placement or merge key.
The V/W pair is the live counterexample.

## 3. Source trace — new placement and changed-file ownership share the defect

| Stage | Current behavior and source |
|---|---|
| Parse | [`parse_episode_inner`](../../crates/plurx-core/src/scan/parse.rs) prefers the directory above `Season N`; title cleanup preserves `(US)`. |
| Show match | [`find_or_create_show`](../../crates/plurx-core/src/scan/mod.rs) uses `find_show`, comparing mutable `items.title` with `COLLATE NOCASE`, then inserts on a miss. |
| Movie match | `find_movie` in [SQLite](../../crates/plurx-core/src/store/sqlite/media.rs) and [Hiqlite](../../crates/plurx-core/src/store/hiqlite_media.rs) likewise compares title and year. |
| Rename | [`metadata`](../../crates/plurx-core/src/metadata/mod.rs) replaces display title through `MetadataPatch`. |
| Unchanged file | [`record_candidates`](../../crates/plurx-core/src/scan/mod.rs) retains the existing item when size and mtime are both unchanged, including probe-repair retries. |
| Changed file | The same loop falls through to `place_item` for changed bytes at an existing path, potentially choosing a new item. |
| Reassociate | `upsert_file` in the media stores updates `item_id` on a path conflict; file-row identity alone does not protect episode identity. |
| Prune | `reconcile_library` deletes fileless playable items, empty seasons and empty shows. `watch_state.item_id` cascades on deletion. A full scan can perform this immediately; a targeted scan leaves cleanup to a later full scan. |
| Hints | [`run_targeted` / `apply_ids`](../../crates/plurxd/src/state.rs) place first, apply IDs second, enrich third. Keep this ordering. |

The scanner's own probe-repair comment explains why an existing file should
not be re-placed after a display rename. Extend that ownership invariant to
changed-file processing instead of limiting it to failed-probe repair.

## 4. Prevention — preserve ownership, then query existing directory lineage

### 4.1 Existing path retains its item before any new placement

In `record_candidates`, select placement as follows:

```text
existing catalogue file at canonical path?
    yes → existing item_id, created = false; probe/update this file in place
    no  → normal placement, with directory lookup before title lookup
```

Keep existing file and item IDs, watch state and associations. Continue to
refresh size, mtime and probe facts and invalidate byte-dependent caches
through existing mechanisms; retaining ownership does not mean retaining
stale media analysis. Test the shared branch for movies, shows and existing
Home/Books behavior. Verify the existing owner belongs to the resolved
library before reuse; do not silently transfer a file across libraries.

Replacing unrelated content under exactly the same path will retain the
existing identity too. That is the bounded ownership contract; explicit
re-identification or moving content is separate work.

### 4.2 Recognize a source directory without adding schema

Carry an optional recognized directory on `ParsedEpisode` and `ParsedMovie`
(or an equivalent parser result) and validate it against canonical library
roots before lookup. Reuse the parser's decision rather than an independent
second interpretation of the path.

For shows, initially recognize `Show/Season N/file`. The directory above
`Season N` must be strictly below a configured library root. Flat files,
release folders and a season folder directly under the library root retain
the existing title/year fallback. Preserve anime numbering and fallback
behavior; do not treat a whole anime library as one show.

For movies, implement the equivalent lookup for a positively recognized
single-movie directory. The initial bounded rule is a direct movie parent
below the library root with a title/year folder shape already understood by
`parse_movie`; exclude library roots and collection/group directories. Do
not treat an arbitrary parent as one movie. Directory shapes outside that
rule retain title/year matching and remain a documented limitation. Test
recognition separately from title extraction, including filenames that
already carry a year, before widening the rule to undated movie folders.

Enrichment does not rewrite `files.path`. Every legacy show/movie with
catalogued files already supplies the ownership needed for the query; no
claims table, scan-title column, backfill marker or startup barrier is needed.
Empty items have no such evidence and fall through to current behavior.

### 4.3 Lookup candidates from files, not metadata IDs

Add required store methods on both backends, conceptually:

```rust
find_shows_by_directory(library_id, directory) -> Result<Vec<Item>, StoreError>
find_movies_by_directory(library_id, directory) -> Result<Vec<Item>, StoreError>
```

These are proposed method shapes, not existing APIs. Do not ship the
prototype's default implementation returning an empty vector for Hiqlite.
The book precedent is `find_book` in the two media stores: it already derives
identity from owned file paths. Its current prefix check uses `substr`, not
an unescaped `LIKE`; neither wildcards nor substring siblings are identity.

For shows, join files → episode → season → show. For movies, join files
directly to their movie. Restrict kind and library throughout the ancestry,
return distinct candidates, and use the existing path index. Proposed POSIX
range predicate under the existing binary path collation:

```sql
-- directory has no trailing slash and is not a library root.
-- Bind lower = directory || '/', upper = directory || '0'.
WHERE f.path >= :lower AND f.path < :upper
```

`'0'` is the successor of `'/'` in this comparison: every descendant prefix
falls inside the range and `<directory>-other` does not. Test `%`, `_`,
Unicode, case and sibling boundaries, and inspect the query plan. Preserve
the established path representation on other supported hosts; Windows
separator/canonicalization needs an explicit fixture or an adapted bound,
not a POSIX string trick silently applied to native backslashes.

Resolve show candidates in this order:

1. Candidates already holding the incoming season, sorted by
   `(added_at ASC, id ASC)`.
2. Otherwise all directory candidates sorted by `(added_at ASC, id ASC)`.
3. If no directory candidate exists, current title/year lookup, then insert.

The ID tie-break is only for equal timestamps; minimum ID alone is not
chronology. Movie directory candidates use `(added_at, id)`. Continue normal
season/episode lookup under the chosen show so second versions attach as
multiple files to one episode.

Pre-existing duplicates within a recognized single-show directory must not
make new playable media unavailable. Pick deterministically, index the file,
and report one bounded problem per directory naming the candidate IDs and
chosen owner. Reuse scan problems and request status; no new unresolved-file
surface is needed. This is placement continuity, not automatic consolidation
or a declaration that every metadata field is correct.

Provider IDs never participate in placement. Source directories for V/W
remain distinct even though metadata assigns the same TMDB ID. Repair has
stronger preconditions than selecting a parent for the next import (§6).

### 4.4 Preserve the current publication boundary

Production placement remains under the existing `scan:library:{id}` lease
and statement-fenced inserts through
[`PublicationStore`](../../crates/plurx-core/src/store/publication.rs) and
[Hiqlite publication](../../crates/plurx-core/src/store/hiqlite_publication.rs).
No resolve/create/bind transaction or schema compatibility gate is added.
Test takeover and queued targeted/full scans through that boundary. Direct
unfenced test helpers do not establish a new concurrent-production contract.

Prevention needs no schema migration, import or digest change. Old binaries
can still exhibit the old bug during deployment: finish upgrading scan
writers before the catalogue repair. Normal release qualification still
applies; absence of a schema change is not evidence that the fleet is fixed.

## 5. Hint correction — preserve placement and reject unsafe metadata writes

Make this a separate task PR. In `apply_ids`, an `episodeish` request may
apply only `series_tmdb` to its show root. An episode's `ids.tmdb` must never
be used as a show ID. Leave valid movie/non-episode routing unchanged.

Apply a supplied series ID only when the show's existing ID is absent or
equal. A different known ID produces a bounded diagnostic while retaining
the playable file and prior ID. Use a conditional store update at the write
boundary, including its fenced counterpart; a read followed by an
unconditional `COALESCE` update can race a metadata writer. Do not change
all metadata refresh behavior to implement this one import-specific guard.

Queued same-path requests retain their current processing order. The first
valid hint that fills an empty show ID wins; equal repeats are idempotent;
later conflicting hints are reported and cannot overwrite it. Hints remain
after placement. Test episode-only IDs, valid series IDs, an existing
different ID, coalesced conflicts and a concurrent metadata update.

## 6. Repair — consolidate same-directory seasons and episode versions

### 6.1 Dry run selects a bounded group and a stable survivor

Add an explicit admin dry-run/apply maintenance operation through the
store. Group by recognized source directory and library, never by provider
ID. Every member must have valid ancestry in that library, and its files
must belong to the reviewed source. Refuse cross-directory/cross-library
sets and conflicting non-null show TMDB IDs. A shared ID alone is not proof.

Choose the survivor by `(added_at, id)` and enumerate the full show, season,
episode and file mapping. Proposed survivors are S=`669`, T=`1061`, U=`279`.
V/W=`3823`/`3833` are an explicit exclusion, not failed repair candidates.

Bind the dry run to a fingerprint of affected rows, references and watch
state. Recheck within apply under the library's existing exclusion/fence;
concurrent changes invalidate the plan rather than being overwritten.
Filesystem checks must run in the daemon's actual library mount namespace
on a node that can resolve its roots. A host path failing `exists()` is not
proof that a container-visible media file disappeared. Catalogue-only dry
runs must identify file availability as unverified.

### 6.2 Merge hierarchy while retaining files and user state

| Situation | Proposed action |
|---|---|
| Losing season number absent on survivor | Reparent that season intact. Season, episode and file IDs remain unchanged. |
| Same season number exists | Use the survivor's season; move non-overlapping episodes intact into it. |
| Same episode number exists | Keep the survivor's episode, reattach every losing file to it without changing file ID/path, then handle per-user state before deleting the losing episode. |
| Multiple losing candidates for an absent season/episode | Process by `(added_at, id)` for a reproducible mapping. |

For overlapping episodes, copy each losing `watch_state` row to the surviving
episode only when that user has no surviving row. Where both exist, retain
the survivor's whole row, including watched flag, position, duration and
timestamp. This is Fable's proposed survivor-wins rule, not a newest-progress
merge. A newer losing resume or watched flag can be discarded: enumerate
both values and the chosen result in the dry run so that approval covers
that concrete tradeoff. Do not describe this as preserving every raw watch
row. An absent destination preserves the complete incoming row; no user
with only losing state becomes unwatched through deletion.

For S/T's disjoint moves, all episode/watch IDs remain unchanged. For U,
losing episode IDs necessarily retire; file IDs stay stable and per-user
watch state follows the explicit mapping. Do not recreate media bytes or
trigger watched outbox events merely because catalogue ownership changed.

### 6.3 Reference handling must precede deletion

Fable's S-tree census reports one episode watch row, zero reading rows,
42 classification rows (two show-level), no reconciliation rows, channel
entries or artwork repairs, and no recipe naming the show IDs. This is
useful evidence for S, not an exemption from a fresh audit of U's overlapping
episodes or changes since review.

| Reference class | Required handling |
|---|---|
| `items.parent_id`, `files.item_id` | Move intended descendants/files before any cascade can delete them. |
| `watch_state.item_id` | Preserve unchanged for reparenting; use §6.2 for overlapping episodes. |
| `reading_state.item_id`, `scan_reconcile_items.item_id` | Unexpected references require refusal or an explicit tested transfer rule. |
| `media_classifications.item_id` | Derived rows with empty overrides may be deleted/rebuilt; non-empty user overrides require a transfer rule or refusal. Do not reject derived rows just because they exist. |
| `items_fts`, classification FTS | Use existing delete/update triggers and verify surviving search results. |
| `cluster_artwork_repairs.item_id` | Retire or invalidate affected work under its existing guard before item deletion. |
| `library_channel_entries.item_id` and `.show_id` | Inspect both fields; rebuild affected derived generations under channel publication rules or refuse active references. |
| Recipe JSON | Inspect `include_item_ids`, `exclude_item_ids`, `include_show_ids` and `exclude_show_ids`; never drop an explicit selection silently. |
| Offline packages, cache/source associations, sessions and outboxes | Stable file IDs preserve many associations, but inspect snapshots/logical references to retiring episode IDs. Refuse unsupported active dependencies. |
| Artwork files named by item ID | Delete only orphan artwork for retired items through existing maintenance; no media deletion. |
| Client bookmarks / compatibility `ratingKey` | Retired item links can become stale. No redirect is proposed; refreshed library browsing exposes survivors. |

The FK census is not the whole dependency contract: JSON, non-FK rows and
active work matter more when episode IDs retire than for a show-only move.
Recheck schema and writers on the build base. Derived classification is an
explicit permitted-delete class; classification overrides are not derived.

Reconcile already removes empty seasons/shows. Prefer explicit deletion of
reviewed empties in the repair transaction, using those existing emptiness
rules, so the promised one-card result is immediate and does not depend on
a later scan. Reuse audited rules without invoking a whole-library prune.
Do not leave empties for automatic reconcile if an unsupported reference
made deletion unsafe.

### 6.4 Atomic apply and observable acceptance

Apply the mapping, guarded watch-state copies and permitted deletions in
one bounded store transaction, fenced on the library lease. A stale plan,
unsupported reference or failed guard aborts the whole group. The new repair
operation needs transactional semantics even though ordinary placement
needs no new primitive. Reuse existing store transaction/publication support.

Return before/after IDs, counts and the applied mapping. After an uncertain
response, re-read the expected final state before retrying: recognize the
same completed mapping without applying watch-state merges twice; partial
or divergent state is an error. No durable receipt table is proposed. State
recognition and idempotent retry require backend contract tests.

Acceptance is relative to the fresh dry-run inventory:

- S: one show `669`, seasons 1–5, original season 5 watch row unchanged.
- T: one show `1061`, seasons 1–4, unchanged episode/file/watch identities.
- U: one show `279`, S01E01–E10, all file versions retained. Fable reports
  20 files; verify that count and the exact file IDs before applying.
- V/W: both shows and their season/episode trees remain untouched.
- All users' mapped watch state matches §6.2. Explain rather than conceal
  source-row count reduction where two rows become one.
- Full scan, new import and metadata refresh do not recreate duplicates;
  both backends and all voters converge, and refreshed clients show survivors.

## 7. Delivery and validation — one effort, three dependent task PRs

Use one effort branch under [the development pipeline](../DEVELOPMENT_PIPELINE.md).
These tasks share scan/store files, so the disjoint-ownership exception does
not apply. Task PRs target the effort; promotion to main requires the final
qualification receipt. Establish Rust 1.97.1 with the
[agent compile loop](../ci/AGENT-COMPILE-LOOP.md) before writing Rust; verify
check, Clippy, formatting and focused tests on the exact intended source
before pushing. This documentation revision makes no Rust test claim.

| Task | Scope | Acceptance |
|---|---|---|
| 1. Ownership and directory lookup | Changed-file preservation first; parser directory context; show/movie methods on SQLite and Hiqlite; deterministic duplicate selection and bounded diagnostics. | Inverted failure probes; movie version/rename coverage; both backend store contracts. |
| 2. Hint correction | Episodeish uses series ID only; conditional absent/equal update through fenced storage; conflicts diagnosed after placement. | Episode/series routing, queued repeats/conflicts and concurrent-ID write tests. |
| 3. Catalogue repair | Dry run/apply; season moves, episode/file/watch mappings; reference checks; immediate audited empty deletion. | Conservation, stale-plan refusal, concurrent activity, atomic rollback and idempotent retry on both backends; reviewed live inventory before production apply. |

Primary source ownership spans
[`scan/parse.rs`](../../crates/plurx-core/src/scan/parse.rs),
[`scan/mod.rs`](../../crates/plurx-core/src/scan/mod.rs),
[`store/mod.rs`](../../crates/plurx-core/src/store/mod.rs),
[SQLite media](../../crates/plurx-core/src/store/sqlite/media.rs),
[Hiqlite media](../../crates/plurx-core/src/store/hiqlite_media.rs),
[publication](../../crates/plurx-core/src/store/publication.rs), and
[`state.rs`](../../crates/plurxd/src/state.rs), with corresponding fenced
store implementations and an admin maintenance surface for task 3. The
build handoff must name actual tests, commands and API interfaces rather
than treating the review prototype as a complete patch.

| Regression | Required result |
|---|---|
| New season after suffix/punctuation/provider rename | Original show reused in targeted and full scans. |
| Second version of one episode or movie | One logical item, multiple file rows. |
| Changed bytes at the same path after rename | Same item/file IDs, refreshed facts, watch state survives, no identity-induced pruning. |
| Legacy same-directory duplicates | Season preference then `(added_at, id)`; playable import and one bounded diagnostic. |
| Flat/release layouts, anime, root-level season folders | No false directory binding; existing fallback/numbering preserved. |
| Movie collection folders and root-level movies | No broad parent match that combines different movies. |
| Separate directories sharing TMDB ID; separate libraries | Provider identity never forces placement or repair across them. |
| Prefix siblings, special characters, supported host paths | Indexed directory bounds have exact component semantics. |
| Lease loss, concurrent queued scans, replicated backend | Existing fences reject stale writes; no empty-default Hiqlite implementation. |
| Repair overlap with only losing watch state / both states | Copy complete row when absent; survivor wins when present, disclosed in dry run. |
| Repair classification, JSON rules, active dependencies | Derived rows handled; user intent/references transferred explicitly or whole group refused. |
| Repair stale fingerprint, rollback, ambiguous response | No partial group, no overwritten concurrent watch progress, safe retry. |

**Accepted limits:** directory renames/moves have no durable alias and may
create a new item if title fallback misses; unchanged-title fallback may
still find the old item. Flat layouts remain vulnerable to metadata/title
drift for new paths. There is no automatic cross-directory merge, provider-ID
placement or bulk movie repair. Books/Home/DVR grouping is unchanged; their
existing-file branch still needs regression coverage because it is shared.
The source-claims table, scan-title migration and provider-before-placement
workstream from the first draft are withdrawn.

## 8. Review disposition — resolved design changes and remaining checks

| Fable finding | Disposition |
|---|---|
| 1. Deterministic cause | Accepted; retained original rows, neutralized titles, explicitly protected season 5 watch state. |
| 2. Wider defect including movies | Accepted; added attributed fleet census and movie prevention, distinguishing candidate counts from normalized-match proof. |
| 3. Changed-file watch loss | Accepted; promoted to primary root cause and first implementation change. |
| 4. Derive identity from existing paths | Accepted; removed table/backfill/migration/import/digest work; required real Hiqlite/movie implementations beyond the prototype. |
| 5. Keep IDs out of placement | Accepted; retained post-placement hints with an atomic import-specific guard; V/W are explicit exclusions. |
| 6. Continue indexing duplicates | Accepted for recognized same-directory candidates; deterministic selection and existing bounded problem reporting. |
| 7. Existing scan lease/fencing | Accepted for placement; a repair still needs one atomic multi-row mutation. |
| 8. Broader repair and census | Accepted with watch-state collision semantics made explicit; added all four recipe selector fields, episode-level dependency checks and fresh inventory requirements. |
| 9. Neutral naming | Applied to title, prose, paths, SQL and index row. |
| 10. Provenance/prototype limits | Updated; reported review tests are not presented as tests run here or a production-complete implementation. |

Before implementation, confirm the bounded movie-directory recognizer and
host-specific prefix behavior, and review the survivor-wins watch-state
tradeoff. Before production repair, resolve the S episode-count discrepancy
and repeat the full reference census for every retiring episode, season and
show. Do not copy the original review's unverified final totals into a
repair command.
