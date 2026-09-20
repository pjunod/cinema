# Scan identity — Sol's build contract for ownership, matching and repair

**Status:** ready for implementation; no code built or production repair run ·
**Executes:** the revised [root-cause and fix proposal](SHOW-IDENTITY-SPLIT-RCA-AND-FIX.md)
after Fable's changes-requested review · **Written:** 2026-09-19 ·
**Implementer:** Sol.

Read the RCA for evidence; this document owns the implementation order,
interfaces, bounded decisions, tests and acceptance. Work through M0–M3 in
order on one effort branch. Complete the prevention and hint changes before
repairing any catalogue. Do not copy the review prototype into the repo as
finished code: its Hiqlite lookup is an empty default, and it does not cover
movies, hints or repair.

The user's request here is a build plan. It does not itself authorize
production catalogue changes. Sol can implement and verify the maintenance
operation against fixtures; production apply requires the concrete reviewed
dry run and operator authorization. Do not interrupt implementation to seek
permission for the routine decisions specified below.

## 1. Outcome and boundaries

The completed effort must establish these invariants:

1. A changed file at the same canonical path keeps its existing item and
   file IDs. Reprobing must not orphan its episode and cascade away watch state.
2. A new episode or movie version in a recognized source directory resolves
   through existing file ownership before mutable display-title matching.
3. Existing duplicate shows do not prevent new playable files from being
   indexed. Parent selection is deterministic and duplicates are reported.
4. Provider IDs remain metadata, not placement keys. Episode TMDB IDs cannot
   overwrite show IDs, and a conflicting import hint cannot replace a known ID.
5. An explicit maintenance operation consolidates same-directory show trees,
   including overlapping episodes, with a reviewed mapping for every retiring
   item, preserved file IDs and defined per-user watch-state behavior.
6. SQLite and Hiqlite implement the same contract. No identity schema,
   backfill, persistent alias or migration is introduced.

Keep the scope finite: no general-purpose cross-directory merger, automatic
bulk cleanup, provider search during placement, movie repair, media renaming,
new metadata provider, native-client UI or playback redesign. Books/Home/DVR
placement stays unchanged except for the shared existing-file ownership fix.
Flat/release layouts and directory renames retain title/year fallback and its
limitations. Do not broaden those cases while building this effort.

## 2. M0 — establish the base, compiler and failure fixtures

### 2.1 Preserve the shared working tree

The authoring checkout was `a603b4e2`; its local `origin/main` pointer was
`1ae2c4ec` when this plan was written. Neither is a required build base.
Fable reviewed `8663d6c0`, which was not available in this checkout's object
database. Fetch current main and record the actual SHA you build.

The working tree also contains unrelated streaming RCA/implementation files
and edits to the documentation index. Preserve them. Use an isolated checkout
for implementation and bring across only this plan, the identity RCA and their
index rows. Both identity documents are initially untracked in the authoring
checkout; a source archive of its HEAD will not contain them. Commit the
intended documentation on the effort before archiving it.

Use these proposed branch names, checking for an existing effort first:

| Branch | Purpose |
|---|---|
| `effort/scan-identity` | Integration branch based on current main. |
| `codex/scan-identity-ownership` | M1, based on the current effort. |
| `codex/scan-identity-hints` | M2, based on the effort after M1. |
| `codex/scan-identity-repair` | M3, based on the effort after M2. |

The three PRs share files; do not use the independent-main-branch exception.
Do not create other user-owned Codex tasks unless asked. This handoff is for
Sol to execute in its assigned task, not an instruction to dispatch agents.

### 2.2 Establish the compile loop before writing Rust

Follow [AGENTS.md](../../AGENTS.md),
[the compile-loop runbook](../ci/AGENT-COMPILE-LOOP.md) and
[the development pipeline](../DEVELOPMENT_PIPELINE.md). Verify the compiler:

```bash
rustc --version                         # must be 1.97.1
cargo --version
rustup show active-toolchain
cargo check --locked -p plurx-core -p plurxd --all-targets
```

If this host cannot run the pin, use the documented source-only compiler
loop: commit, `git archive`, transfer source only, keep the build target warm,
and never transfer `.git` or a repository credential. Results must identify
the archived SHA. Re-archive and recheck after rebasing or merging a new base.
Do not use CI to discover whether the changes compile.

### 2.3 Read the implementation seams, not just the prototype

| Seam | Existing source to inspect |
|---|---|
| Parser and placement | [`scan/parse.rs`](../../crates/plurx-core/src/scan/parse.rs), [`scan/mod.rs`](../../crates/plurx-core/src/scan/mod.rs): `record_candidates`, `place_item`, `find_or_create_show`. |
| Storage contract | [`store/mod.rs`](../../crates/plurx-core/src/store/mod.rs): `MediaStore`, `FencedPublicationStore`, `Store`. |
| Backend queries | [SQLite media](../../crates/plurx-core/src/store/sqlite/media.rs), [Hiqlite media](../../crates/plurx-core/src/store/hiqlite_media.rs): `find_book`, show/movie lookup, `upsert_file`, reconciliation. |
| Publication | [`PublicationStore`](../../crates/plurx-core/src/store/publication.rs), [SQLite fenced operations](../../crates/plurx-core/src/store/sqlite/publication.rs), [Hiqlite fenced operations](../../crates/plurx-core/src/store/hiqlite_publication.rs). |
| Scan orchestration and hints | [`state.rs`](../../crates/plurxd/src/state.rs): request/queue/drain, `run_targeted`, `apply_ids`, library lease. |
| HTTP/auth/routing | [`http/mod.rs`](../../crates/plurxd/src/http/mod.rs), [`http/scan.rs`](../../crates/plurxd/src/http/scan.rs), [`http/libraries.rs`](../../crates/plurxd/src/http/libraries.rs), [`http/extract.rs`](../../crates/plurxd/src/http/extract.rs). |
| Replicated test harness | [`store_contract.rs`](../../crates/plurx-core/tests/store_contract.rs): `for_each_backend` and the media/watch/publication contracts. |

Reconfirm that `record_candidates` is still shared by full and targeted scans,
and that production callers hold `scan:library:{id}`. Check new writers or
references added since the review before relying on its inventory.

### 2.4 Turn the review probes into retained regressions

Fable's local `_showid_review_probe_v2.rs` asserts the old defective behavior;
`_proto_patch_v1.py` inverts those assertions for its scratch implementation.
These files are supplementary evidence, not dependencies on Sol's machine.
Recreate their four scenarios with neutral fixture names and assertions for
the required fixed behavior. Do not commit user names or real library titles.

Capture the pre-fix failures for: a suffix rename followed by S5, punctuation
rename followed by a second version, and changed bytes followed by a full
scan destroying watch state. Keep the no-rename control passing. Prefer
small fake-video fixtures already used by scanner tests; use a mock probe or
existing media fixture only when verifying refreshed probe facts.

**M0 acceptance:** known build SHA, working pinned compiler, documented old
failure outputs, and no unrelated edits in the implementation checkout.

## 3. Decisions Sol can implement without further design work

| Decision | Contract |
|---|---|
| Show directory | Recognized `Show/Season N/file`, using the parser's existing season-directory grammar, strictly below a library root. |
| Movie directory | Direct parent `Title (YYYY)` below a library root; the directory/filename title-year consistency rule in §4.2 prevents collection-folder capture. Undated parents are deferred. |
| Path representation | Use the canonical strings already written to `files.path`; no database path rewrite. Derive separator-aware bounds as in §4.3. |
| Duplicate placement | Existing incoming-season owner first, then oldest `(added_at, id)`; report, continue indexing. |
| Provider hint | After placement, series ID only for episodeish requests; atomically absent-or-equal. |
| Watch collision on repair | Survivor's complete row wins; missing user rows are copied in full. Dry run must expose losing differences and apply explicitly acknowledge them. |
| Unsupported repair references | Refuse the entire group with a typed reason; do not silently delete user rules or active work. Derived empty-override classifications are allowed. |
| Repair interface | Admin-only JSON preview/apply/status endpoints; no new settings screen or native-client change. |
| Plan persistence | Bounded node-local preview cache, 15-minute lifetime; no schema or durable receipt. Restart/eviction requires a new preview. |
| Production execution | After prevention deployment and a fresh reviewed preview, separately authorized by the operator. |

The movie rule and explicit watch-collision acknowledgement refine the RCA's
remaining review points. They are this build's proposed defaults, not claims
that Fable already reviewed the exact wire contract. Include them in the
normal adversarial PR review.

## 4. M1 — existing ownership and directory lookup

### 4.1 Preserve existing-path ownership before `place_item`

Keep the size/mtime unchanged fast path and failed-probe repair behavior.
For changed files, replace unconditional new placement with:

```rust
// Illustrative shape; use the current Placement/home::Placed types.
let placement = match existing.as_ref() {
    Some(file) => Placement::Placed(home::Placed {
        id: file.item_id,
        created: false,
    }),
    None => place_item(store, library, &path).await?,
};
```

Before reuse, ensure the existing item's library matches the resolved
library and that its kind can own that file in this library. Treat broken
ancestry or cross-library ownership as a reported scan error, retaining the
existing association. Do not silently place a replacement elsewhere.

Keep `is_new` and added/updated/probe-failure report semantics correct. The
changed-file branch still stats and probes, updates file facts in place,
runs applicable NFO/book metadata work, and returns the original IDs to a
targeted caller. Do not change `upsert_file` globally to forbid reassociation;
M3 will intentionally reattach version files through its own transaction.

A full scan after a repack must not prune the original episode. Compare
complete watch rows before/after, not just the watched Boolean. Test both
size-only and mtime-only changes, plus an unsuccessful re-probe. Preserve
byte-dependent cache/index invalidation already driven by source facts.

### 4.2 Carry directory context from the parser

Add `source_directory: Option<PathBuf>` to `ParsedEpisode` and `ParsedMovie`
or to a shared placement-context wrapper that the parser returns. Do not add
this field to public item DTOs. Update every struct literal and both anime
parse paths. Existing title/year/numbering extraction must remain unchanged.

Show recognition uses the same `SEASON_DIR` grammar as title selection.
For the initial implementation, accept the direct layout only. A release
folder between the season and file has no directory identity in this lane;
it falls back to current matching. The candidate source must be canonical,
inside a configured root and unequal to every applicable root. Test a library
root itself called a show name: `root/Season 1/file` must not identify root.

Movie recognition accepts a direct parent ending in a parenthesized four-digit
year parsed by the existing year rule. Parse the filename's title/year as
well as the parent's. If the filename supplies a nonempty title or year that
contradicts the cleaned parent title or explicit parent year, return no
directory identity. Ignore only release cruft and the existing case policy;
do not erase region qualifiers or punctuation to force agreement. This
bounded rule covers normal `Title (Year)/Title.Year.quality.mkv` variants,
including filenames that themselves carry a year, while preventing a broad
collection parent from grouping differently named films. Root-level movies,
undated parents and ambiguous collection layouts use current matching.

A source directory is not an inode identity or a persistent alias. Moving
or renaming it can still create a new item if title fallback misses.

### 4.3 Backend lookup and path contract

Add required methods to `MediaStore`, with concrete implementations on both
backends and test doubles updated at compile time:

```rust
async fn find_shows_by_directory(
    &self, library_id: i64, directory: &str,
) -> Result<Vec<Item>, StoreError>;

async fn find_movies_by_directory(
    &self, library_id: i64, directory: &str,
) -> Result<Vec<Item>, StoreError>;
```

These signatures are the proposed contract. There is no empty default on the
trait. Return distinct candidates sorted by `(added_at ASC, id ASC)`.
For shows join `files` → episode → season → show, checking each kind/library;
for movies join directly to the movie. Never call the unscoped
`item_by_external_id` from placement.

Use the existing unique index on `files.path`. For stored POSIX paths:

```sql
-- Bind directory without a trailing separator.
-- lower = directory || '/', upper = directory || '0'
SELECT DISTINCT sh.id
FROM files f
JOIN items e ON e.id = f.item_id AND e.kind = 'episode'
JOIN items s ON s.id = e.parent_id AND s.kind = 'season'
JOIN items sh ON sh.id = s.parent_id AND sh.kind = 'show'
WHERE f.path >= :lower AND f.path < :upper
  AND e.library_id = :library_id
  AND s.library_id = :library_id
  AND sh.library_id = :library_id;
```

Expand the projection/order using the existing item row decoder. This query
is a range prefilter, not proof that every deeper descendant belongs to the
exact recognized directory. Reject evidence under a nested second show or
movie directory: its parser-derived directory must equal the requested one.
Implement that exact-directory filter in SQL or in a shared parser helper
on the range's evidence rows; keep it identical across backends. Do not use
one arbitrary `MIN(path)` as proof that all required evidence was considered.
Deduplicate after exact-directory filtering. Querying a directory must not
perform a full-library file scan.

For native Windows paths, derive the prefix using the existing stored path
separator. A binary prefix range is `[prefix, successor(prefix))`; for a
trailing backslash the successor character is `]`, not `0`. Keep path case
and existing canonical representation unchanged, including extended/UNC
prefixes. Factor/test the lexical bounds helper with POSIX and Windows
strings on any host; test actual canonicalization on the Windows compile/test
surface available in the repo. Do not rewrite legacy separators in SQL.
Reject a malformed/empty prefix rather than scanning all files.

Test literal `%`/`_`, non-ASCII directory names, case-distinct names, sibling
prefixes, trailing separators, nested content and two libraries. Capture
`EXPLAIN QUERY PLAN` evidence on a representative SQLite fixture showing an
indexed path range; Hiqlite executes the same SQL semantics. Parameterize
paths and IDs. No new index or schema version should be needed.

### 4.4 Deterministic selection and bounded duplicate reporting

For a new episode with directory candidates:

1. Choose the first `(added_at, id)` candidate that owns the incoming season.
2. If none owns it, choose the first candidate overall.
3. Run existing `find_season` / `find_episode` beneath that show; create only
   absent season/episode rows. A second version attaches to the existing
   episode, even if metadata renamed the show.
4. With no directory candidate, use existing title/year lookup then insert.

Avoid unbounded per-candidate round trips: batch season ownership or add a
small internal candidate projection with `has_season`. If doing so, record
that refinement of the trait shape and retain the same ordering contract.
Movies choose the oldest directory candidate, then current title/year fallback.

Return selection diagnostics to `record_candidates`; do not make the store
mutate `ScanReport`. Maintain a per-scan set of directories already reported,
call the existing bounded `report.note`, and include candidate/chosen IDs.
It is a problem note, not a skipped-file count. Respect existing report limits;
a large duplicate library must not produce one warning per episode. Runtime
logs may contain local paths under current policy; committed evidence uses
neutral paths. Provider lookup and repair do not run during selection.

### 4.5 M1 tests, ownership and acceptance

Primary edits: parser/scanner, `MediaStore`, two media backends and tests.
Use the existing library lease and statement-fenced insertion; no new
placement transaction abstraction. Leave generic movie/show title lookup
callers unchanged outside this scanner path.

Add the `scan_identity_` prefix to new scanner tests, and this backend-neutral
contract test in the existing `for_each_backend` harness:

```text
scan_identity_directory_contract
```

The cases must cover new seasons after rename, second versions, unchanged
legacy files supplying lineage immediately, changed-file watch preservation,
flat/release/anime fallbacks, movie directory exclusions, duplicate season
preference/ties, prefix boundaries and cross-library isolation. Exercise both
`scan_library` and `scan_path` rather than assuming their shared helper proves
caller/report behavior. Add a daemon queued/full-scan ownership regression
with the existing publication lease fixture.

**M1 acceptance:** all old failure probes invert; one logical item owns two
versions; changed-path identity is unchanged; Hiqlite is exercised through
three voters; no schema or provider-placement code was added. Update
[Features](../FEATURES.md), [Operations](../OPERATIONS.md) and the RCA's build
status only to describe behavior actually delivered.

## 5. M2 — guarded series metadata hints

### 5.1 Dedicated conditional store operation

Add the following proposed contract to the store and publication wrapper,
with unfenced and fenced implementations:

```rust
enum SeriesHintOutcome {
    Applied,
    AlreadyEqual,
    Conflict { current_tmdb_id: i64 },
    MissingOrWrongKind,
}

async fn apply_series_tmdb_hint(
    &self, library_id: i64, show_id: i64, tmdb_id: i64,
) -> Result<SeriesHintOutcome, StoreError>;
```

Reject nonpositive IDs. The mutation is limited to a show in the supplied
library and to `tmdb_id IS NULL`. Equal IDs return success without rewriting
metadata or timestamps. A different known ID never changes. Determine the
outcome using a transaction/consistent write result, not a stale read followed
by an unconditional `apply_metadata`. Fenced variants accept the existing
`Lease` and replacement token through `PublicationStore::fenced_call`.
Do not weaken the absent/equal guard when a metadata worker races it.

Leave normal `apply_metadata` semantics alone. This guard protects externally
supplied import hints; it is not a global ban on metadata correction.

### 5.2 Apply after placement and surface conflicts

In `apply_ids`:

- For `episodeish`, use only `series_tmdb`, resolve the placed episode's
  show root, and call the dedicated guard. Ignore an item-level episode
  `ids.tmdb` for show metadata; do not reinterpret it as a legacy series ID.
- Preserve non-episode/movie routing and current IMDb behavior.
- Repeated files in one request should not cause redundant writes to the
  same show. Equal repeats are still idempotent at the store boundary.
- First valid series hint on an unassigned root wins. Coalesced conflicting
  requests are handled in their established order; later conflicts produce
  a diagnostic instead of overwriting the first ID.

Return bounded notes into the targeted scan report/request record and log
correlation ID, show ID and both provider IDs. A conflict leaves the file
indexed/playable and the request's scan result available. Do not turn the
entire import into a failure merely because a hint disagrees. An episode-only
ID without `series_tmdb` needs no alarming error; it is simply not a show hint.

**M2 tests:** add `scan_identity_series_hint_contract` to the backend-neutral
harness and daemon tests prefixed `scan_identity_hint_`. Cover null/equal/
conflict, wrong kind/library, invalid ID, series-only request, episode-only
request, mixed hints, coalesced order, stale lease and concurrent metadata
write. Assert playable placement and report contents on disagreement.

**M2 acceptance:** no episode-ID-to-show fallback remains; the absent/equal
condition is atomic on both backends; hints remain after placement. Update
[API](../API.md) and [Integration](../INTEGRATION.md) to state the behavior.

## 6. M3 — bounded repair planner and admin API

### 6.1 Implementation layout

Put repair-specific types and pure mapping logic in a focused core module,
for example `store/scan_identity_repair.rs`, re-exported from the store.
Add backend implementations beside existing media/publication operations,
and a focused daemon HTTP module such as `http/scan_identity.rs`. These are
new implementation files to create, not paths that already exist.

Keep the planner deterministic and separate from database mutation. Its
input is a consistent, bounded snapshot; its output is a complete mapping
and typed blockers. The HTTP layer authenticates and caches previews, the
job coordinator owns the library lease, and the store verifies/applies the
plan atomically. No shell SQL, arbitrary SQL endpoint or direct state-machine
file write is part of this feature.

### 6.2 Proposed admin wire contract

All endpoints below require `AdminUser` and current write-capable store
routing. API keys scoped only to scan imports do not authorize repair.
Use the existing API error and routing conventions; test reader/worker roles
and maintenance mode instead of accidentally permitting a local side write.

| Endpoint | Request / result |
|---|---|
| `POST /api/v1/libraries/{id}/identity-repairs/preview` | `{ "show_ids": ["669", "6420086538814441635"] }`; produces a bounded report and opaque plan ID when applicable. |
| `GET /api/v1/libraries/{id}/identity-repairs/{plan_id}` | Returns cached preview/apply state or reports that the preview expired. |
| `POST /api/v1/libraries/{id}/identity-repairs/{plan_id}/apply` | `{ "fingerprint": "…", "accept_watch_conflicts": false }`; applies only the cached server-generated plan. |

All catalogue/user/file IDs in new JSON fields are decimal strings. Several
live IDs exceed JavaScript's exact integer range; never round-trip them as
JSON numbers. Parse strict positive i64 IDs, reject duplicates, and sort them
server-side. Do not accept client-authored file reparenting maps or SQL.

Preview response fields:

```text
plan_id, node_id, library_id, expires_at, fingerprint,
status = ready | blocked,
source_directory, survivor_show_id, input_show_ids,
item_moves[], file_moves[], retired_item_ids[],
watch_copies[], watch_conflicts[],
reference_summary, blockers[], before_counts, expected_after_counts,
file_availability = not_checked
```

Each watch conflict carries user ID, source/destination episode IDs and both
complete state values, plus `resolution: survivor_wins`. File moves retain
file IDs and include expected old/new owners. Item moves retain IDs and
include expected old/new parents. A blocked preview contains reasons and no
applicable plan. These are admin diagnostics, not a new product workflow.

Planning is catalogue-only and never claims to have checked disk availability.
It does not need media reads to preserve known IDs and ownership. Operational
verification may check files through a correctly mounted daemon after preview;
never use an arbitrary host namespace to conclude that a file is missing.

### 6.3 Bounded preview cache and status

Use a node-local cache of at most eight plans, maximum 2 MiB serialized
snapshot/plan per entry, and 15 minutes from creation; no background database
job or persistent receipt table. Bound each request to 2–32 show IDs, at most
512 seasons, 4,096 episodes, 8,192 files and 16,384 watch rows. The byte bound
also applies, so large metadata can hit it before the row limits. These are
explicit operational limits, not silent truncation thresholds. Refuse an
over-limit group with `repair_too_large`; smaller independent groups can be
previewed separately, but do not split one atomic repair internally.

Associate plans with the authenticated admin's user ID and origin node. A
plan ID is not authority; apply reauthenticates. Prefer non-expired completed
entries for eviction over in-flight entries; never evict a transaction while
its result is unresolved. If all slots are pinned, return `repair_busy`.
The origin node ID is explicit: a request reaching another node cannot apply
an unknown cached plan and must return a typed routing/expiry response,
not silently generate another plan. Reuse current cluster node discovery for
routing; do not construct arbitrary redirects from client input.

Statuses are `ready`, `applying`, `applied`, `outcome_unknown`, `stale`, and
`blocked`. Cached applied responses are replayed without reapplying. A server
restart or cache expiry loses the preview; return `repair_plan_expired` and
require a fresh preview of current rows. This is the stated limit of avoiding
a durable receipt, not a promise of replay across restart.

HTTP status/error contract:

| Status | Cases |
|---|---|
| 200 | Preview (ready or blocked), status read, applied/already-applied result. |
| 400 | Malformed IDs/fingerprint, invalid body, invalid operation shape. |
| 401 / 403 | Existing unauthenticated/non-admin semantics. |
| 404 | Library or requested show absent during a new preview. |
| 409 | `repair_busy`, `repair_stale`, `repair_blocked`, `watch_conflicts_unaccepted`, `repair_wrong_node`. |
| 410 | `repair_plan_expired`. |
| 413 | `repair_too_large`. |
| 503 | Authority unavailable or `repair_outcome_unknown`; include retry/status guidance, never an assertion that nothing committed. |

Use `ApiError::Typed`/`TypedDetail` rather than changing legacy error bodies.
No preview can be applied with unacknowledged watch conflicts. The Boolean
acknowledgement applies to the fingerprinted conflict list, not later changes.

### 6.4 Snapshot, fingerprint and preconditions

Validate all requested rows are shows in one Shows library, with valid
season/episode ancestry. Derive a single recognized source directory from
all their file paths; require every file to belong to it and every member
to have source evidence. Reject mixed directories, malformed/duplicate
numbering within a single parent, missing season/episode numbers and a
newly discovered directory owner omitted from the requested set. List that
omitted owner so a new complete preview is possible. Do not select a partial
subset that leaves another known duplicate behind.

Different non-null show TMDB IDs block repair; equal or absent IDs do not
prove identity by themselves. Keep survivor metadata as-is rather than
merging genres, titles, annotations or provider IDs. The directory/numbering
contract is the evidence for logical overlap. No file-content deduplication
or remote episode-provider request is required.

Read a consistent snapshot, not a series of unrelated catalogue reads. Use
one aggregate SQL snapshot or an actual backend read transaction. Include:

- library ID/kind/root configuration, every affected item row and ancestry;
- complete file identity/ownership facts and source path for the group;
- per-user watch rows, including absence of destination rows that will be
  copied into;
- classification rows/overrides and every dependency class in §7.3;
- the complete set of other directory owners and season/episode membership.

Sort every collection by stable keys and version the canonical encoding.
Hash that server-generated snapshot and deterministic plan with SHA-256.
Do not use `updated_at` alone: its granularity is not a compare-and-swap
version, and it does not represent missing/new references.

Apply must test equivalence of the relevant database rows and absence sets
inside its write transaction. The client fingerprint only selects what the
admin saw; it is not itself a database guard. A new episode, file, watch
update, rule reference or metadata change after preview makes the plan stale.

**M3 planning acceptance:** deterministic previews; complete reference
inventory; strict decimal IDs; limits/expiry/routing/auth covered; no mutation
from preview beyond normal authentication bookkeeping.

## 7. M3 — mapping, dependencies and atomic apply

### 7.1 Deterministic show/season/episode mapping

Choose the surviving show by `(added_at ASC, id ASC)`, not minimum ID alone.
For each season number, prefer the surviving show's existing season; if
absent, take the oldest losing season and reparent it intact. For each
remaining overlapping season, process episodes by number. Prefer an episode
already under the retained season; otherwise move the oldest matching
losing episode intact. Use `(added_at, id)` for all ties.

When multiple episodes share the same number across losing seasons, reattach
all files from retiring episodes to the retained episode. File rows, IDs,
paths, size/mtime, probe facts and file-keyed associations do not change
except for `item_id`. Never delete a version because its bytes or quality
look redundant. Season/show metadata and retained episode metadata remain
those of the selected survivor. Non-overlapping moved rows retain metadata.

Build every move and retirement before writing. The postcondition is one
season per number and one episode per number under the surviving show;
retired episodes have no files, retired seasons no children, retired shows
no children. If those invariants cannot be established from the snapshot,
block the group rather than infer a missing relationship.

### 7.2 Watch-state semantics

For each `(user, retained episode)`:

1. If a row already exists, preserve the complete row unchanged.
2. Otherwise copy the complete row from the first losing episode under the
   deterministic source order. If further losing rows for that user differ,
   show them as conflicts with that selected row.
3. Copy before deleting the losing episode. Do not call user watch APIs that
   enqueue provider/outbox events or rewrite timestamps.

The preview exposes all differing collisions, including one watched row
losing to an unwatched survivor and a newer resume losing to an older one.
`accept_watch_conflicts = true` is required to apply those exact differences.
Equal duplicate rows need no special acknowledgement, but count reduction
must still be reported. No user represented only on a losing episode may
lose their only mapped state.

For disjoint season moves, episode IDs and every watch row remain unchanged.
Do not advertise “all IDs unchanged” for overlapping episode consolidation:
losing episode IDs intentionally retire. Late requests against retired IDs
must use the existing missing-item response rather than recreate rows or
surface an unhandled FK error; add a focused regression for that boundary.

### 7.3 Reference policy for the first release

Implement a declarative dependency inventory shared by preview and in-write
precondition checks. Review the current schema and logical JSON references;
foreign keys alone are insufficient. Apply this bounded policy:

| Dependency on an item being retired | First-release policy |
|---|---|
| `items.parent_id`, `files.item_id` | Transfer exactly as planned. |
| Episode `watch_state` | Copy/resolve as §7.2, then delete retiring row by cascade. |
| Show/season watch state | Block as unsupported, since there is no approved semantic mapping. |
| `reading_state`, reconciliation work rows | Block while present. Do not erase them because the table cascades. |
| Classifications with empty include/exclude overrides | Permit derived-row deletion and normal rebuild. |
| Nonempty classification overrides | Block; a transfer policy is outside this release. |
| Item FTS/classification FTS | Existing triggers; verify removal of retired rows and survival of retained results. |
| Active/pending artwork repair rows | Block. Do not cancel another lease owner's work from this repair. |
| Channel entries referencing an item or show ID being retired | Block; require existing rebuild/retirement of that generation first. |
| Recipe JSON include/exclude item IDs or show IDs | Block; do not silently change user selections. |
| Offline/cache/source records keyed only by retained file IDs | Preserve unchanged; verify no logical retiring-item reference is embedded. |
| Active playback/work containing retiring item IDs | Block or wait for normal completion; no forced stop. |
| Outbox/snapshot JSON with retiring item IDs | Block unless current source proves it is independent of those IDs. |
| Artwork on disk for retired IDs | Harmless orphan after DB commit; clean only through an existing proven orphan collector, or report retained orphan IDs for later maintenance. |
| Client bookmarks / compatibility rating keys | Stale links accepted; no redirect table. Refreshed browsing uses retained IDs. |

Inspect all four recipe fields: `include_item_ids`, `exclude_item_ids`,
`include_show_ids`, `exclude_show_ids`, plus both channel-entry ID columns.
Do not reject all classification rows: the live S census contains derived
show classifications, which are explicitly permitted to retire.

For non-transactional/node-local active dependencies, a one-time preview
check is not exclusion. Prove that existing admission/lease mechanisms prevent
new references during apply, or refuse that active case and document the
required quiet condition. Do not claim the library scan lease also excludes
playback, watch writes or classification work. Durable concurrent changes
must be caught by the transaction predicates; audit races creating new
non-FK references to a retired row. If an existing writer lacks a target-
existence guard, add the narrow guard and test it rather than creating a
cluster-wide generic maintenance subsystem.

### 7.4 Store/publication interfaces and transaction structure

Add a backend-neutral snapshot/planner API and an apply API with types such
as `IdentityRepairSnapshot`, `IdentityRepairPlan`, `IdentityRepairOutcome`.
Add the apply method to the fenced store family and expose it only through
the job coordinator for production. The standalone counterpart supports
SQLite and contract fixtures, not a remote bypass of the scan lease.

Acquire `scan:library:{id}` for apply; if busy, return a conflict and leave
the plan ready for a later attempt. No queued destructive repair should run
unexpectedly after its preview expires. Keep the lease through the result
and refresh the cached outcome. Cancellation after submission is outcome-
ambiguous until a consistent read determines the state.

Within one transaction:

```text
validate exact lease and complete expected preimage
  → create transaction-local authorization output
  → move intact seasons/episodes
  → reattach version files
  → copy allowed watch rows without overwriting retained rows
  → delete reviewed, now-empty episodes/seasons/shows in that order
  → assert expected final ownership/count/reference predicates
  → commit
```

SQLite: use the current fenced connection transaction, compare the snapshot
inside that transaction and return an error before commit on any mismatch.
Do not open nested transactions under `with_fenced_conn`; factor helpers
accepting its connection as existing code does.

Hiqlite: reuse `atomic_publication`, whose statement zero renews the lease
and whose mutations bind authority to that statement's returned row.
The proposed preimage guard is a no-op update of the surviving item guarded
by all expected-row/absence predicates, returning its ID. Every later
statement must consume that returned output in addition to the existing
lease authority. An absent guard output must cause transaction rollback,
not a sequence of zero-row mutations that appears successful.

The vendored transaction API supports statement-output parameters. Verify
and test exact statement indices: the lease renewal shifts application
statements by one. Do not rewrite the vendor or general transaction layer.
Equivalent existing guard machinery is acceptable if it proves the same
atomicity. A postcondition assertion must fail inside the transaction too;
checking affected-row counts only after commit cannot undo a partial result.

The guarded snapshot SQL must compare actual values and exact membership,
including new rows/references, not rely on a custom SHA function being
available in SQLite. Bind the deterministic expected data, or compare stable
SQL JSON projections; the external SHA fingerprint is for preview identity.
Keep parameters and statements within the plan limits and the actual backend
limits. A group too large for one safe transaction is refused, not chunked.

### 7.5 Timeouts, retries and concurrent work

Hiqlite's client may retry after a leader change. Therefore the store method
must recognize the plan's already-applied mapping after an ambiguous result
or guard rejection, even if the preceding attempt retired source IDs.
Return `AlreadyApplied` only when all retired IDs are absent, mapped files
still point to retained episodes, intact moves have their expected parents,
and the structural/reference postconditions hold. Never reapply watch copies
on that path: subsequent legitimate watch progress must remain untouched.

If current state equals the original preimage, a retry may apply normally.
If it matches neither original nor expected completed structure, return
`Stale`/`OutcomeUnknown` with diagnostics; do not guess or regenerate a plan
under the old approval. Cache a successful response until expiry. Without a
durable receipt this proves that the intended structure exists, not which
actor performed it; state that limitation in the API documentation.

Test failure after each write step, stale lease, lost response after commit,
retry through a different leader, concurrent watch update before the guard,
and new rule/reference insertion at the commit boundary. Every failure
before commit must preserve the complete preimage. No cleanup file deletion
belongs inside a database transaction or an uncertain retry.

## 8. Exact test lanes and meaningful evidence

The new test names below are required implementation deliverables, not tests
already present. Prefix scanner/unit/HTTP cases with `scan_identity_` and
add these three top-level backend contracts:

```text
scan_identity_directory_contract
scan_identity_series_hint_contract
scan_identity_repair_contract
```

Use the existing `for_each_backend` harness so contracts run on SQLite and,
with the feature enabled, three real Hiqlite voters. Include stale lease
and transactional rollback cases there, not only SQL unit simulations.

Run focused commands after the relevant task creates these tests:

```bash
# New parser/scanner/repair helpers.
cargo test --locked -p plurx-core --lib scan_identity_
# New daemon import and admin API cases.
cargo test --locked -p plurxd --bin plurxd scan_identity_
# SQLite modes of the shared contracts.
cargo test --locked -p plurx-core --test store_contract scan_identity_
# The same scenarios with the real replicated backend.
cargo test --locked -p plurx-core --features hiqlite-contract-tests \
  --test store_contract scan_identity_ -- --test-threads=1
```

Confirm each command ran the intended nonzero case set. Record names/counts,
SHA, compiler version and outcome in the task PR. A filter matching zero tests
is not evidence. Run additional existing regressions for shared behavior:

```bash
cargo test --locked -p plurx-core --lib scan::
cargo test --locked -p plurx-core --lib store::sqlite::
cargo test --locked -p plurx-core --test store_contract media_contract_runs_through_dyn_store
cargo test --locked -p plurx-core --test store_contract watch_contract_runs_through_dyn_store
cargo check --locked -p plurx-core -p plurxd --all-targets
cargo clippy --locked -p plurx-core -p plurxd --all-targets -- -D warnings
cargo fmt --all --check
python3 -m unittest discover -s tests/operations -p test_docs_index.py
git diff --check
```

Enable the shipping Hiqlite feature explicitly for core-only compile/lint
checks if dependency feature unification does not already do so. Run the
repository's placeholder census for new Hiqlite SQL and affected API/static
checks selected by validation. Windows path logic requires its platform
fixture/compile evidence, not just POSIX tests with a backslash string.
Full affected qualification belongs to the frozen final tree, not every
small edit; rerun focused checks when their source/base changes.

| Boundary | Required assertions |
|---|---|
| Changed existing file | Same episode/movie/file IDs, exact watch rows, new facts, no identity-induced prune. |
| Directory resolution | No provider calls; exact source directory; deterministic duplicates; two versions remain two files. |
| Hint routing | Series-only permitted; episode-only never stamps root; conflicting existing ID unchanged; conflict visible. |
| Preview | Admin isolation, no catalogue mutation, consistent snapshot, decimal ID round-trip, blockers, bounds, expiry, wrong node. |
| Disjoint repair | Original season/episode/file/watch IDs unchanged; only parent move and reviewed empty deletion. |
| Overlap repair | Every file ID conserved; explicit retired episode mapping; absent-row copy and acknowledged survivor-wins collisions. |
| References | Derived classifications allowed; overrides and four JSON selector types block; new references invalidate apply. |
| Failure/retry | Stale plan/lease aborts all; lost response recognized; no repeated state merge; later watch progress retained. |
| Separate directories | Shared-provider V/W fixtures untouched; cross-library and incomplete owner sets refused. |

Register corrective commits with the current validation catalog and
regression ledger. Follow its actual tooling/schema; do not insert a guessed
commit SHA before the commit exists. Use neutral synthetic IDs/titles in
fixtures and capture only sanitized live acceptance evidence.

## 9. Documentation, review and release

### 9.1 Per-task completion

Each task PR contains the concrete before/after behavior, changed surfaces,
focused test commands/results and accepted limits. Maintain this checklist
on the effort; add exact SHA/PR/evidence links as they exist:

| Milestone | State at handoff | Required evidence |
|---|---|---|
| M0 compiler/base/failures | not started | SHA, Rust 1.97.1, old failures and passing control. |
| M1 ownership/directory lookup | not started | Scanner plus SQLite/Hiqlite contracts, movie/path cases. |
| M2 guarded hints | not started | Both backend CAS behavior and queued import tests. |
| M3 repair | not started | API/transaction/reference/retry tests and sanitized fixture preview. |
| Final review/qualification | not started | Adversarial review disposition and exact-candidate gates/receipt. |
| Production deployment/repair | not authorized by this document | Deployment receipt and separately reviewed live preview/apply result. |

Update [API](../API.md), [Operations](../OPERATIONS.md),
[Integration](../INTEGRATION.md) and [Features](../FEATURES.md) in the task
that changes their behavior. Document stale bookmark behavior, preview expiry,
file-availability status, watch collision resolution and flat-layout limits.
Keep the [RCA](SHOW-IDENTITY-SPLIT-RCA-AND-FIX.md) as the evidence/design record;
link to this plan rather than maintaining two competing API contracts.
Every new document belongs in its subject folder and must be indexed in
[the documentation map](../README.md) in the same commit.

### 9.2 Gates and review

Follow the current workflow correction at the top of the development pipeline:
main-bound PRs start draft, receive the required adversarial review and become
ready only after findings are addressed. Do not resurrect the removed
`fast-lane` label or assume that opening/merging a PR deploys anything.
Effort checks may need manual dispatch; missing evidence is not a pass.

Treat the `Effort development gate` as blocking for task integration as
required by AGENTS.md. After all three task PRs integrate, freeze task merges,
merge current main into the effort, and qualify that exact tree. Require
`Main promotion gate` and the qualification receipt before main promotion.
A base/tree move invalidates prior qualification. Use current workflow
commands rather than copying stale automatic-CI assumptions from old docs.

The adversarial review must attack at least: broad directory capture, Windows
prefix handling, skipped Hiqlite implementation, duplicate diagnostics,
episode-ID hint fallback, transaction preimage completeness, non-FK writer
races, hidden watch-state loss, replay after leader retry and JSON ID precision.
Address concrete findings before declaring the effort build-complete.

### 9.3 Production acceptance, prepared but not silently executed

After the approved release is deployed to every scan writer, take fresh
admin previews for the neutral incident groups from the RCA:

| Group | Intended survivor | Expected structural result |
|---|---|---|
| S: `669`, `6420086538814441635` | `669` | Seasons 1–5; season 5 watch row unchanged. |
| T: `1061`, `5932` | `1061` | Seasons 1–4, same episode/file/watch IDs. |
| U: five same-directory rows listed in the RCA | `279` | S01E01–E10, every version file retained, explicit watch mapping. |
| V/W: `3823`, `3833` | neither | No changes; separate directories remain separate shows. |

Compare preview counts with the source rows. S was observed with 40 episodes,
while the review later requested 41; U was reported with 20 files and the
review mentioned 17 watch rows without a complete mapping. Fresh enumerated
IDs and state are the baseline, never those inconsistent totals.

Prepare the reviewable preview, collision list, reference blockers, deployment
revision and recovery evidence before requesting authorization to apply. Use
the supported admin operation only. Do not edit a local Raft state-machine
file, run a broad rescan as a substitute for the repair, or repair against an
old prevention binary. If blocked by user state/references, report the actual
blocker and leave the group untouched.

After authorized apply, verify store convergence on all voters, fresh browse
results, each preserved/mapped watch row and file ID, and one subsequent
import plus full scan/metadata refresh. Do not download media just to test the
fix: use an already intended import or a disposable fixture library. Capture
actual deployment/repair evidence separately from implementation test results.

## 10. Sol's completion report

Report the effort and task PRs, final qualified SHA, exact tests run and the
review disposition. State which behavior is deployed, whether production
repair was authorized/applied, any groups blocked by reference policy, and
the remaining flat-layout/directory-move limits. A built maintenance endpoint
is not evidence that live duplicates were repaired; a passing SQLite probe
is not evidence that Hiqlite or Windows works.
