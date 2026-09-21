# Detail reads and storage availability — a badge that never unpacks an index, and an availability answer that carries its age

**Status:** ready for review · **Executes:** C14 (§3.3.3) from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `0f02b7ea`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Board id **C-05**. Read §2 first: it quotes the detail loop, the index
read it calls, and the two rows in the sidecar that decide the badge, and
it states exactly which of `get`'s five refusal conditions a cheap
projection has to reproduce. Then build §5 in order — M1 (the validation
marker) must land before M2 (the projection), because the projection's
whole safety argument is that marker; M3 (availability) is independent of
both and may run in a parallel session. One draft PR per milestone into
`main` under the fast lane. Every `file:line` is from `0f02b7ea`;
re-verify by function name.

**If a step seems to require changing invalidation-by-mismatch (a stale
row answering `None` rather than being deleted), making the detail page
authoritative for whether a file can be opened, deleting a
`fragment_indexes` row that fails validation, or letting an unverified
legacy row render as `indexed`, stop and flag it.** Those four are the
invariants this plan is built on; everything else here is mechanism.

**Correction to the review:** three, none fatal to C14.

1. C14 has **no row in the assessment.** It arrived with Astra's review in
   revision 3 (§0, "Added from Astra's review"), and the assessment
   (`ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md`) was written against the
   first draft. The governing dispositions are therefore §3.3.3's three
   acceptance clauses and nothing else; this plan treats them as the
   required-disposition list and says in §4 how each is honoured. Do not
   look for an F-core row for this item — there is not one.
2. The review says the cost is a Store read. On the **hiqlite** backend it
   is not a leader round trip: `fragment_index` and
   `fragment_index_outcome` route through `HiqliteAuthStore::telemetry`
   (`hiqlite.rs:3096-3102, 3166-3175`), the node-local sidecar, whose every
   call is `spawn_blocking` behind **one** `Mutex<Connection>`
   (`store/telemetry.rs:526-540`). That connection is also what the
   fragment indexer writes through. So the real cost of a detail render on
   a clustered voter is contention with index *writes* on a single mutex,
   plus a `spawn_blocking` hop per identity — which is worse than a
   consensus read would be to diagnose, because nothing in `/metrics`
   currently separates them. §3.4 adds the separation.
3. The handler reads more than the review names: `get_file_probe_json(f.id)`
   at `browse.rs:395` pulls the **raw catalogue probe JSON** for every file
   on the page, before any badge work. That is a second unbounded blob per
   file, it is a replicated read on hiqlite, and it is used for two things
   (`video_identities`' DV decision and `chapters_from_probe_json`). It is
   in scope here because it is on the same loop and dominates the same
   page; M2 keeps it but stops fetching it twice over.

---

## 1. Objective

1. An item-detail render never materialises a fragment index. The index
   badge is decided from a projection that reads row metadata only — never
   the `rows_packed` blob — for every `(file, identity)` pair on the page,
   in a bounded number of store calls rather than one call per pair.
2. A row whose packed bytes or promotion blob would not survive
   `fragment_index`'s own checks can never render as `indexed`. That is
   guaranteed by a marker written at publication, not by re-deriving the
   checks from cheaper columns.
3. Filesystem availability is a bounded, cached *observation* with three
   states — `available`, `unavailable`, `unknown` — and a timestamp. A
   detail render with an unmounted share answers within its budget instead
   of holding metadata the database already has.
4. Outstanding filesystem work is capped **and** deduplicated, so a caller
   that gave up does not leave a stat behind and ten callers asking about
   one path produce one stat.
5. The authoritative open stays at playback. Nothing on the detail path
   authorises opening a source; a stale observation can only change a badge.

## 2. Contract today

Re-verify at build time.

### 2.1 The detail loop

`crates/plurxd/src/http/browse.rs:389-465`, inside `item_detail`
(`:304`):

```rust
let playback_prefs = state.transcode.lang_prefs().await;
let mut file_dtos: Vec<FileDto> = Vec::with_capacity(files.len());
let mut part_offset_ms = 0_i64;
for f in files {
    let path = f.path.clone();
    let available = tokio::fs::metadata(&path).await.is_ok();      // :394
    let raw_probe = state.catalogue.get_file_probe_json(f.id).await?; // :395
    // …
    let videos = crate::fragindex::video_identities(
        &f, raw_probe.as_deref(),
        crate::ffmpeg::has_dovi_rpu().await,
        state.transcode.dv_convert_enabled().await,
    );
    let mut present = 0_usize;
    let mut refusals = Vec::new();
    for video in &videos {
        let identity = crate::fragindex::identity_for(&f, *video);
        if state.store.fragment_index(f.id, &identity).await?.is_some() {  // :432
            present += 1;
        } else if let Some(outcome) =
            state.store.fragment_index_outcome(f.id, &identity).await?     // :435
        {
            refusals.push(outcome);
        }
    }
    // … "indexed" / "partial" / "refused" / "pending"
}
```

Per file: one `stat`, one probe-JSON read, and up to two store calls per
identity. `video_identities` (`fragindex.rs:1240-1267`) returns one
identity for an ordinary file, two for a Dolby Vision file, three when the
node converts Profile 7. An audiobook's `files` is every part, and the
parts are sequential (`browse.rs:377-383`), so a 300-part audiobook is 300
stats and 300 probe reads on one request — sequentially, each `await`ed
before the next begins.

### 2.2 What `fragment_index` costs, and the five ways it says `None`

`crates/plurx-core/src/store/fragindex.rs:374-442`:

```rust
let row = conn.query_row(
    "SELECT source_size, source_mtime, argv_fingerprint, segplan_version,
            timescale, init_sha256, rows_packed, promotion,
            parameter_sets_constant
       FROM fragment_indexes
      WHERE file_id = ?1 AND argv_fingerprint = ?2", …)?.optional()?;
let Some((…)) = row else { return Ok(None) };                 // (a) absent
if version != i64::from(SEGPLAN_VERSION) { return Ok(None); }  // (b) old plan
let stored = SourceIdentity::new(size.max(0) as u64, mtime, fingerprint);
if !stored.matches(identity) { return Ok(None); }              // (c) file changed
let rows = unpack(&packed)?;                                   // errors on bad length
if rows.is_empty() { return Ok(None); }                        // (d) empty
index.promotion = match serde_json::from_str(&promotion) {
    Ok(inputs) => inputs,
    Err(_) => return Ok(None),                                 // (e) unparsable
};
```

`unpack` (`:221-238`) allocates one `IndexRow` per 24 bytes
(`ROW_BYTES = 24`, `:202`). A 4,100-fragment index is 98,400 packed bytes
and 4,100 heap rows, built and dropped to answer `is_some()`.

The badge needs (a), (b), (c) and *a truthful answer for* (d) and (e).
(a)–(c) are decidable from columns. (d) and (e) are not: `unpack` only
checks that the blob length is a multiple of 24, so a corrupt blob of the
right length passes it, and `serde_json` must actually run to know whether
the promotion blob parses. **This is why the plan writes a marker at
publication instead of guessing from `fragments`** — see §3.1.

`put` (`:283-360`) already stores `fragments = index.rows.len()`
(`:314, 337`), and nothing reads it back. A projection that trusted
`fragments > 0` would report `indexed` for a row whose blob is garbage,
which is the exact failure §3.3.3 forbids.

### 2.3 The outcome side

`fragment_index_outcome` → `fragindex::outcome` (`:613-700`) is already a
metadata-only point read: fourteen scalar columns, an identity match, no
blob. It is the badge's `refused`/`pending` input, it is cheap, and this
plan does not change its semantics — only how many round trips it takes.

### 2.4 Where the rows live

`FRAGMENT_INDEXES_SCHEMA` (`fragindex.rs:25-37`), reached by both backends
through append-only migration lists: the standalone `MIGRATIONS` array
(`sqlite/mod.rs:821, 832, 878`; `SQLITE_SCHEMA_VERSION = MIGRATIONS.len()`,
`:1135`) and the sidecar's guarded batch (`store/telemetry.rs:418-513`,
`SIDECAR_SCHEMA_VERSION = 9`, `:90`). The sidecar's comments at `:462-478`
and `:498-508` state the rule this plan must follow: a create constant is
frozen at its historical shape, additions arrive as their own constant, and
the guard is `creating_indexes || !column_exists(…)` so a fresh database
and an upgraded one reach one table by one route.

### 2.5 Availability

There is no availability cache anywhere in `crates/plurxd`. `browse.rs:394`
is the only place the detail path touches the filesystem, and its result
goes to `FileDto.available` (`:455`) and, for admins, `missing_path`
(`:462-464`). The comment at `:380-388` records the intent: "so the client
can refuse to 'play' a file that's missing … instead of opening a dead
player", and "one stat per file — cheap for the handful a movie/episode
has". That reasoning is correct for a movie and wrong for an audiobook and
for a dead mount, which is the finding.

Prior art for bounded filesystem work is in the image path:
`images.rs:32` `MATERIALIZE_CONCURRENCY = 8` with `try_acquire_owned`
(`:87-89`) and a per-filename `lock_owned` single-flight (`:76-85`). M3
follows that shape and fixes its one flaw for this use — `try_acquire`
turns contention into a refusal, which here must be `unknown`, not an error.

## 3. Change

### 3.1 The validation marker (M1)

One new column, one new constant, appended to both migration lists:

```rust
/// Written at publication, read by the projection. The marker says the
/// row's packed bytes unpacked to exactly the rows that were stored and
/// its promotion blob round-tripped — the two facts `get` establishes by
/// doing the work, and the only two a metadata projection cannot derive.
pub(crate) const FRAGMENT_INDEXES_VALIDATION_COLUMN: &str = "
ALTER TABLE fragment_indexes ADD COLUMN validated_revision INTEGER NOT NULL DEFAULT 0;";
```

`VALIDATION_REVISION` is a `u32` constant beside `SEGPLAN_VERSION`
(`segplan.rs:49`, currently `3`). It starts at `1` and is bumped whenever
the definition of a valid row changes — a new promotion field, a change to
`ROW_BYTES`, a change to what `get` refuses. A bump does not delete
anything: an older marker stops matching, which is the same
invalidation-by-mismatch discipline `get` already documents at `:363-372`.

`put` gains, immediately before the insert:

```rust
let packed = pack(&index.rows);
let promotion = serde_json::to_string(&index.promotion)
    .map_err(|error| StoreError::Migration(error.to_string()))?;
// Prove, once, what the projection will later be trusted to assume.
let validated = !index.rows.is_empty()
    && unpack(&packed)? == index.rows
    && serde_json::from_str::<crate::fmp4::PromotionInputs>(&promotion).is_ok();
let validated_revision = if validated { i64::from(VALIDATION_REVISION) } else { 0 };
```

A row that fails validation is still written, with marker `0`. It is not
an error and not a deletion: the indexer's own retry and outcome machinery
owns that decision (`record_typed_outcome`, `:453+`), and deleting here
would make a publication failure indistinguishable from a file that was
never indexed. What the `0` buys is that the row can never be read as
`indexed`.

**Legacy rows.** Every row already on disk gets `0` from the column
default. `0` means *unverified*, not *invalid*: the projection reports
`unverified`, the badge renders `pending`, and the handler's
`vod_index_refusal` stays empty. This is fail-closed and it is visible —
`pending` is the state the badge already uses for "the indexer has not
answered yet". A bounded background revalidation pass (M1 step 3) reads
`file_id, argv_fingerprint` for rows with `validated_revision <>
VALIDATION_REVISION`, in pages of `VALIDATION_PAGE = 64`, at most one page
per `VALIDATION_INTERVAL = 30 s`, calls the existing `get` for each (which
does the real unpack) and writes the marker. A library converges in
`rows / 64` half-minutes — a 20,000-row node in under three hours —
without a rebuild and without a startup stall.

### 3.2 The batched projection (M2)

New trait method on `FragmentIndexStore` (`store/mod.rs:4703+`), beside
`fragment_index`:

```rust
/// Badge-shaped status for many (file, identity) pairs at once. Never
/// reads `rows_packed`: the row's own validation marker is what says the
/// blob is sound, because a length check is not that proof (§2.2).
async fn fragment_index_status(
    &self,
    wanted: &[(i64, crate::segplan::SourceIdentity)],
) -> Result<Vec<FragmentIndexStatus>, StoreError>;
```

```rust
pub struct FragmentIndexStatus {
    pub file_id: i64,
    pub argv_fingerprint: String,
    pub presence: IndexPresence,                 // Ready | Unverified | Absent
    pub fragments: u32,                          // 0 unless Ready
    pub outcome: Option<FragmentIndexOutcome>,   // the existing type
}
```

The SQL names every column it needs and `rows_packed` only under
`length()`, so no blob leaves SQLite:

```sql
SELECT file_id, argv_fingerprint, source_size, source_mtime,
       segplan_version, fragments, validated_revision,
       length(rows_packed) AS packed_len
  FROM fragment_indexes
 WHERE file_id = ?1 AND argv_fingerprint = ?2
```

`presence` is `Ready` only when **all** of: `segplan_version ==
SEGPLAN_VERSION`; the stored `(size, mtime, fingerprint)` matches the
asked-for identity through the existing `SourceIdentity::matches`;
`validated_revision == VALIDATION_REVISION`; `fragments > 0`; and
`packed_len == fragments * ROW_BYTES`. The last is redundant against the
marker and is kept as a cheap consistency assert — a mismatch means the row
was edited outside `put`, and it demotes to `Unverified`, never to `Ready`.
Everything else is `Unverified` (row present, marker stale) or `Absent`.

**Shape of the call.** Both backends iterate the wanted list with one
prepared statement per call — the same choice `surviving_file_ids`
(`hiqlite.rs:3186-3199`) already made and documented: fixed-arity SQL, no
dynamic placeholder building. The win is not fewer SQL statements; it is
**one** `with_conn` / `spawn_blocking` / mutex acquisition for the whole
page instead of one per identity, which is the cost §2 identified. The
outcome read joins the same call: the projection also runs the existing
`outcome` point query for every pair whose presence is not `Ready`, inside
the same connection lease, so the handler makes exactly one store call for
the whole page.

`wanted` is chunked at `STATUS_CHUNK = 256` pairs by the caller so a
300-part audiobook is two calls, not one unbounded one.

**The handler.** `item_detail` becomes two passes:

```text
  files ──▶ pass 1: for each file, read probe JSON once,
  │                  derive identities, collect (file_id, identity)
  │                                             │
  │                                             ▼
  │                        one fragment_index_status(&wanted) per chunk
  │                                             │
  └──────▶ pass 2: build FileDto from the projection + the probe JSON
                    already in hand + the availability answer (§3.3)
```

`has_dovi_rpu()` and `dv_convert_enabled()` are hoisted out of the loop —
they are node-wide answers and are currently re-awaited per file.
`get_file_probe_json` is read once per file in pass 1 and reused in pass 2
for `chapters_from_probe_json`; today it is fetched once and used twice,
which is already correct, and the change only moves it.

The badge rules at `:441-452` are unchanged in meaning. `Ready` counts
toward `present`; `Unverified` and `Absent` do not; the refusal summary is
built from the same `index_refusal_summary` (`:32-67`) over the same
outcomes. A file whose every identity is `Unverified` renders `pending`,
not `refused`, because no refusal was recorded.

### 3.3 Availability as a bounded cached observation (M3)

A new node-local module `crates/plurxd/src/availability.rs`:

```rust
pub enum Availability { Available, Unavailable, Unknown }

pub struct Observation {
    pub state: Availability,
    pub observed_at_ms: Option<i64>,   // None only for Unknown-never-observed
}

const TTL: Duration = Duration::from_secs(15);
const MAX_AGE: Duration = Duration::from_secs(120);
const PROBES: usize = 8;              // concurrent stats, node-wide
const MAX_INFLIGHT: usize = 1_024;    // distinct paths with a probe pending
const ENTRIES: usize = 16_384;        // cached observations
const REQUEST_BUDGET: Duration = Duration::from_millis(250);
```

- **Read.** A cached entry younger than `TTL` answers immediately. Older
  than `TTL` but younger than `MAX_AGE`: the cached state is returned and a
  refresh is scheduled. Older than `MAX_AGE`, or absent: `Unknown`, and a
  probe is scheduled. An observation past `MAX_AGE` is never returned —
  a two-minute-old "available" is not evidence about a share that was
  unmounted ninety seconds ago, and returning it would be the stale
  authorisation §3.3.3 forbids.
- **The probe is detached from its caller.** Scheduling inserts the path
  into an in-flight map (bounded at `MAX_INFLIGHT`; over that, the request
  answers `Unknown` and schedules nothing) and spawns one task that
  acquires a permit from a `Semaphore::new(PROBES)` with `acquire_owned`
  — *not* `try_acquire`, because a refusal here must be a wait, not an
  `unknown` — runs `tokio::fs::metadata`, writes the observation, and
  removes the in-flight entry. **A caller that times out does not cancel
  the probe and does not create another**: this is the "cap outstanding fs
  work even after callers time out" clause, and it is why the work lives in
  the map rather than in the caller's future.
- **The request budget.** `item_detail` asks for every file's availability
  at once, waits up to `REQUEST_BUDGET` for the scheduled probes as a
  group (`tokio::time::timeout` around a `join_all` of per-path
  notifications), and takes whatever has answered. The rest are `Unknown`.
  250 ms is chosen against the page's other work: the projection is one
  store call, and a detail render that already costs ~30–60 ms should not
  quadruple for a mount that is not going to answer. A dead NFS mount
  blocks in the kernel for its own `timeo`, which no userspace deadline
  shortens — the budget bounds *this request*, not the syscall, and §4
  says so.
- **Eviction** is by observation age, then by insertion order, evaluated
  when the map exceeds `ENTRIES`. Sixteen thousand entries × (path +
  16 bytes) is single-digit megabytes on the largest library in the fleet.

**The DTO.** `FileDto` gains `availability: "available"|"unavailable"|
"unknown"` and `availability_observed_at_ms: Option<i64>`. The existing
`available: bool` stays and is defined as **`availability != "unavailable"`**
— so `unknown` reads as available to a client that has not been updated.
That direction is deliberate: `available: false` is what makes a client
refuse to start playback (`:380-388`), and an unobserved mount must not
refuse a play that the authoritative open at playback would have allowed.
The cost of the other direction is a dead player; the cost of this one is
the error the playback path already produces, in the place that is entitled
to produce it. `missing_path` (admins only) is set on `unavailable` and
never on `unknown`, because a path is only "the thing to fix" once
something observed it missing.

`GET /api/v1/items/{id}` gains those two fields in
[API.md](../API.md) §6 as part of M3's PR; the clients read `available`
unchanged and may adopt the tri-state later.

### 3.4 Metrics

Fixed-cardinality atomics rendered as text, the house pattern
(`store/hiqlite.rs:629-737`, `telemetry.rs:28-58`), so a `/metrics` scrape
still does no store work — pinned by the existing
`prometheus_scrape_has_no_store_operation` test (`http/system.rs:5483`).

| Metric | Type | Labels (bounded) | Why |
|---|---|---|---|
| `plurx_index_status_projections_total` | counter | `presence="ready\|unverified\|absent"` | The badge's own answer, so an operator can see a library stuck on `unverified` |
| `plurx_index_status_pairs` | histogram | none; buckets 1,2,4,8,32,128,512,+Inf | Pairs per detail render — the audiobook case as a number |
| `plurx_index_validation_backfill_total` | counter | `result="validated\|refused\|gone"` | M1's convergence |
| `plurx_storage_availability_total` | counter | `result="available\|unavailable\|unknown"` | The three states, at the rate they are answered |
| `plurx_storage_availability_probes_inflight` | gauge | none | The cap in §3.3, observable |
| `plurx_storage_availability_probe_seconds` | histogram | none; buckets 0.001,0.01,0.05,0.25,1,5,+Inf | What a slow mount actually costs |

No content identifiers, no paths, no file ids in any label.

### 3.5 Settings

None. Every bound here is a constant with a reason in the code beside it.
An operator who needs a different availability TTL is describing a
different problem (a mount that flaps), and the answer to that is the
`unknown` state, not a knob. This plan adds no replicated setting and no
Settings → Developer entry.

## 4. Guardrails (non-goals)

- **No bulk index payload for a badge** (§3.3.3). The projection's SQL
  names `rows_packed` only inside `length()`, and M2's acceptance is a
  test-only counter on `unpack` that must read zero after a detail render
  of a 4,100-fragment index. A grep is not enough on its own, because a
  future caller could reach `fragment_index`; the counter catches that.
- **Corrupt rows never become a ready badge** (§3.3.3). The marker is
  written at publication by the code that has the real rows in hand
  (§3.1), never inferred from `fragments` or from blob length. An
  unmarked legacy row is `Unverified` → `pending`, which is fail-closed.
  The redundant `packed_len == fragments * ROW_BYTES` assert demotes,
  never promotes.
- **Availability is an observation, not an authorisation** (§3.3.3). It
  has three states and a timestamp; anything past `MAX_AGE` becomes
  `Unknown`; and M3's acceptance includes a test that the playback start
  path does not read the cache at all. The authoritative open stays where
  it is.
- **Outstanding work is capped after callers time out** (C14). Probes live
  in a bounded in-flight map with a semaphore, not in caller futures; a
  timed-out caller leaves exactly one probe running and a second caller for
  the same path joins it rather than adding one.
- **A blocked syscall is not cancellable.** `tokio::fs::metadata` is a
  `spawn_blocking` hop; a dead NFS mount holds that thread until the kernel
  returns. The plan bounds the *request* and the *concurrency* (8), and
  logs `storage probe exceeded {n}s for a mount` at `warn` once per path
  per hour so a wedged export is visible in Settings → System → Logs. It
  does not claim to cancel the syscall.
- **Invalidation stays by mismatch.** Nothing in this plan deletes a
  `fragment_indexes` or `fragment_index_outcomes` row. `put`'s two existing
  `DELETE`s (`:289-308`) are untouched, and the backfill only writes a
  marker column.
- **Order and semantics of the badge are preserved.** `indexed` /
  `partial` / `refused` / `pending` keep their meanings from
  PLAYBACK-CAPS-V2-PLAN §4.7 as the comments at `browse.rs:398-430`
  record them; `index_refusal_summary` is not touched.
- **No feature gate, no new setting** (§3.5).
- **Not in scope:** the probe-JSON read itself. Pass 1 reads it once per
  file, as today. Making `get_file_probe_json` a batched or summarised read
  is real work with its own identity questions (the DV decision reads it)
  and belongs with the decode-facts effort
  ([DECODE-FACTS-GATE-AND-FALLBACK.md](../streaming/DECODE-FACTS-GATE-AND-FALLBACK.md));
  §7.1 states the measurement that would open it.

## 5. Milestones

### 5.1 M1 — the validation marker (`core/fragindex-validation-marker`)

1. `VALIDATION_REVISION` beside `SEGPLAN_VERSION`;
   `FRAGMENT_INDEXES_VALIDATION_COLUMN`; appended to the standalone
   `MIGRATIONS` array (`sqlite/mod.rs:821+`) and guarded into the sidecar
   batch as v10 (`store/telemetry.rs:418+`, bumping
   `SIDECAR_SCHEMA_VERSION`), with the `creating_indexes ||
   !column_exists(…)` guard the v6 comment (`:462-478`) explains.
2. `put` computes and writes the marker (§3.1).
3. The bounded backfill: a new arm in the existing background loop that
   owns index housekeeping, `VALIDATION_PAGE = 64` per
   `VALIDATION_INTERVAL = 30 s`, calling `get` per row and writing the
   marker; `plurx_index_validation_backfill_total`.
4. Tests in `fragindex.rs`'s test module:
   `put_marks_a_sound_row_validated`;
   `put_leaves_an_unparsable_promotion_unmarked` (construct a
   `FragmentIndex` whose promotion fails to serialise round-trip);
   `an_empty_row_set_is_never_marked`;
   `a_revision_bump_unmarks_without_deleting` (write at revision N, read
   at N+1, assert the row is still there and reports unverified);
   `backfill_marks_a_legacy_row_and_is_idempotent`.

Acceptance: `cargo test -p plurx-core store::fragindex` green;
`cargo test -p plurx-core store::telemetry` green (the sidecar migration
tests cover the fresh-vs-upgraded routes);
`sqlite3 <active hiqlite dir>/telemetry.db 'PRAGMA
table_info(fragment_indexes);' | grep validated_revision` — the sidecar
path is `active.join("telemetry.db")` at `cluster/migration.rs:579`, so
read it from there rather than guessing — prints a row on a node upgraded
in place **and** on a node created from empty, and the standalone
backend's database does the same.

### 5.2 M2 — the batched projection and the two-pass handler (`core/detail-index-projection`)

1. `FragmentIndexStatus`, `IndexPresence`, `fragment_index_status` on the
   trait and both backends; `STATUS_CHUNK = 256`.
2. `item_detail` rewritten as the two passes in §3.2, with
   `has_dovi_rpu()` / `dv_convert_enabled()` hoisted.
3. A `#[cfg(test)] static UNPACK_CALLS: AtomicUsize` incremented in
   `unpack` (`fragindex.rs:221`), exposed through a test-only accessor.
4. Tests:
   `a_detail_render_unpacks_no_index` (seed a 4,100-row index, render
   `/api/v1/items/{id}`, assert `UNPACK_CALLS` unchanged and the badge is
   `indexed`);
   `an_unverified_legacy_row_renders_pending_not_indexed`;
   `a_row_whose_length_disagrees_with_fragments_is_unverified`;
   `a_partially_indexed_dv_file_still_renders_partial` (three identities,
   two ready);
   `a_refused_identity_still_renders_its_summary` (pins
   `index_refusal_summary` through the new path);
   `a_three_hundred_part_audiobook_makes_two_status_calls` (count store
   calls through the existing test store).

Acceptance: `cargo test -p plurxd browse` and `cargo test -p plurx-core
store::fragindex` green;
`grep -n "rows_packed" crates/plurx-core/src/store/fragindex.rs` shows the
projection's only occurrence inside `length(`;
`grep -rn "fragment_index(" crates/plurxd/src/http/browse.rs` finds
nothing.

### 5.3 M3 — bounded availability observations (`core/storage-availability-cache`)

1. `crates/plurxd/src/availability.rs` per §3.3; wired into `AppState`;
   `item_detail` uses it; `FileDto` gains the two fields; API.md §6 row.
2. The four metrics in §3.4 that belong to availability.
3. Tests:
   `a_cached_available_answers_without_a_stat` (fake clock, counter on the
   probe);
   `an_observation_past_max_age_answers_unknown`;
   `a_timed_out_caller_leaves_one_probe_and_no_duplicate` (a probe held
   open, two callers, assert one in-flight entry and that both get
   `Unknown` within the budget);
   `the_inflight_cap_answers_unknown_instead_of_growing`;
   `unknown_does_not_set_available_false` (the DTO rule in §3.3);
   `missing_path_is_set_only_for_unavailable`;
   `the_playback_start_path_never_reads_the_cache` (assert the probe
   counter is untouched across a start).

Acceptance: `cargo test -p plurxd availability` and `cargo test -p plurxd
browse` green; `make unit` green once before un-WIP;
`grep -rn "availability::" crates/plurxd/src/transcode.rs
crates/plurxd/src/http/hls.rs` finds nothing (the authoritative open never
consults it).

## 6. Verification and rollout

### 6.1 Lanes

Per milestone the focused `cargo test` above, then `make unit` once before
un-WIP. `make validate-staged` before every push. M3 touches
[API.md](../API.md) §6, so `python3 -m pytest tests/operations/
-k api_doc_routes` must stay green.

### 6.2 Rollout

M1 first and alone, to one node, watched for one backfill convergence
(`plurx_index_validation_backfill_total` stops rising) before M2 goes
anywhere — M2's badge is only correct once markers exist, and on an
unbackfilled node every title reads `pending`, which is a visible
regression if the two land together. M2 and M3 to the fleet after that.
Nothing here changes recipe identity, argv fingerprints or cache digests;
the sidecar schema version moves, and a rollback to the previous
`sha-` image reads the new column as absent, which the frozen-create rule
(§2.4) already tolerates — the older binary never names it.

### 6.3 What only the fleet can prove — GPT prompt

```text
On lab2 (10.42.1.12), with the build carrying PR <M3 number>:
1. Confirm a normal detail page is fast: time the API call for a movie
   with one file —
   curl -s -o /dev/null -w '%{time_total}\n' \
     -H "Authorization: Bearer $TOKEN" \
     http://10.42.1.12:32400/api/v1/items/<movie id>
   Run it five times and report all five.
2. Same for an audiobook with 100 or more parts. Report five timings.
3. Unmount the nas export that library lives on:
   sudo umount -l /mnt/nas-media
   Repeat steps 1 and 2 and report the timings. Every call must return in
   under 1.5 s. In the JSON, report the values of "availability" and
   "available" for the first three files.
4. curl -s http://10.42.1.12:32400/metrics | \
     grep -E 'plurx_storage_availability|plurx_index_status' — paste it.
5. Remount (sudo mount /mnt/nas-media), wait 20 s, repeat step 1, and
   report that "availability" is back to "available".
6. Open the same title in the web client during step 3 and say what the
   page shows: whether the Play button is enabled, and what the index
   badge reads.
Report exact values. If any call in step 3 takes longer than 5 s, stop,
paste the last 50 lines of journalctl -u plurxd, and do not restart.
```

### 6.4 The query-instrumentation acceptance (§3.3.3's first clause)

The clause is "query instrumentation shows no bulk index payload for a
badge". Three independent checks, all in M2:

1. `UNPACK_CALLS` is zero across a detail render of a 4,100-fragment
   index (the direct proof).
2. `plurx_index_status_pairs` shows the pair count per render, so the
   audiobook case is a number rather than an assertion.
3. On `lab2`, before and after M2, `curl -w '%{time_total}'` on the same
   audiobook detail page, three runs each, reported in the PR body. The
   claim M2 may make is the one the table supports.

## 7. Open questions

1. **The probe-JSON read.** `get_file_probe_json` is one unbounded blob per
   file per detail render (§2.1) and a replicated read on hiqlite. Before
   proposing a batched or summarised form, measure it: add
   `plurx_detail_probe_json_bytes` (histogram) in M2 and decide from the
   p95 on the real library. If it is under a few hundred kilobytes per
   page, leave it.
2. **Who owns the backfill loop.** M1 step 3 needs a home in an existing
   background loop rather than a new one. The content-analysis housekeeping
   loop is the obvious host
   ([CONTENT-ANALYSIS-FAILURES-IMPLEMENTATION.md](../streaming/CONTENT-ANALYSIS-FAILURES-IMPLEMENTATION.md)),
   but it is lease-gated for cluster-singleton work and this backfill is
   node-local. Confirm at build time that the arm runs on every node, not
   only the lease holder; if the loop cannot host node-local work, the
   backfill gets its own interval task and M1 says so.
3. **`available` for `unknown` is a product call.** §3.3 chooses
   "unknown reads as available" and gives the reason. If Paul prefers the
   client to show a "cannot check this right now" state instead, that is a
   client change across three platforms and belongs in its own plan; the
   server field is already shaped for it.
4. **Whether `MAX_AGE` should be per-mount.** A local disk and a Wi-Fi NAS
   have different honest staleness windows. One constant is chosen because
   the server has no mount model today; `plurx_storage_availability_probe_seconds`
   is the measurement that would justify introducing one.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| | | | | | |
