# D3t review — binding triggers

**Status:** complete — every finding dispositioned on `claude/ps-d3t` (§ Dispositions below): both blockers and all four should-fix fixed, nits fixed or recorded.
- **Reviewer:** Fable (adversarial, read-only)
- **Date:** 2026-11-12
- **Scope:** `git diff 4347a3db8 7292fc30e` (base `origin/claude/ps-e1c-d3a`, head `origin/claude/ps-d3t`; 4 commits: 1b6217969, 001005092, 2e3694559, 7292fc30e). Head had not moved when the review closed.
- **Method:** source reading only (`git show/diff/grep`). Nothing was built or run; every "fails" below is by reading the assertion against the diff, not by a test run.
- **Verdict:** **Not ready to merge — 2 blockers.** The replicated (hiqlite) side is sound and mirrors `source_arbitration_shape` faithfully. The SQLite v129 entry breaks three pinned tests/fixtures that were outside the gates the builder ran, and the "closed" key list misses one setting the create chain reads.

## Blockers

### B1. SQLite v129 turns three pinned tests / fixtures red; none was in the D3t gate

`crates/plurx-core/src/store/sqlite/mod.rs:1346-1348` appends a `MIGRATIONS` entry, so `SQLITE_SCHEMA_VERSION` (`:1356`, `MIGRATIONS.len()`) becomes 129. Nothing else that pins that ordinal was touched:

1. `crates/plurx-core/src/store/sqlite/tests.rs:870-874` (`v6_rebuild_preserves_everything`): `assert_eq!(version, 128, "a new migration must be a deliberate bump…")` — now 129.
2. `crates/plurx-core/src/store/sqlite/dv_conversion.rs:1601-1775` (`the_downgrade_fixture_undoes_every_migration_after_the_guard`): `SQLITE_SCHEMA_VERSION - 44 == DROPPED_BY_THE_FIXTURE.len()`; the array is still `[&str; 84]` (`:1603`), the left side is now 85. The assertion's own message names the two fixtures that must learn to drop the new objects.
3. `crates/plurx-core/tests/support/source_extensions_fixture.rs:429-432` (`coordination_tail_rewind_plan`): `assert!(matches!(version, 127 | 128), "only published coordination tails may be rewound")`. Every fixture that winds a *current* database back panics at v129: `queue_fixture.rs:1118, 1161` (so `remove_common_queue_schema` and its users), `subtitle_translation_retarget.rs:208`, `sqlite_v43_guard_migration_refuses_an_unrecoverable_committed_claim` (`dv_conversion.rs:1280`), and `populated_v14_import_fixture` (`store_contract.rs:18974`, cluster-store-check lane).

Also, both wind-back fixtures drop `playback_input_generation` (`dv_conversion.rs:1285-1288`, `store_contract.rs:18981-18984`) but would leave the three `playback_input_keys_*` triggers standing over a missing table: any later write of one of the five keys in such a fixture fails with `no such table: playback_input_generation`.

The record's §8 gate list ran `--lib` filtered to the census/placeholder tests and only the touched store-contract cases, which is why none of this surfaced.

**Fix (pick one):**
- (a) Keep v129: bump the literal in `tests.rs:871` with its comment line; add the marker table to `DROPPED_BY_THE_FIXTURE` (85) and drop the three triggers + marker in both wind-back fixtures next to the `playback_settings_*` drops; teach `coordination_tail_rewind_plan` a 129 tail (drop the D3t objects, then proceed as 128). Then run the whole `-p plurx-core --lib` and `--test store_contract` / `subtitle_translation_retarget` once.
- (b) Preferred, and it also resolves S1: do not take a `MIGRATIONS` ordinal. Install the same SQL from `SqliteStore::open` after `migrate` as an idempotent extension step (every statement is already `IF NOT EXISTS` / `ON CONFLICT DO NOTHING`), with the same exact-shape check as hiqlite. `user_version` stays 128, the three pins stay true, and the fixtures need only the trigger drops.

### B2. The "closed" key list misses `subtitles.stored_sources`, which both sides of a burn start read

`crates/plurx-core/src/store/playback_input_keys.rs:25-31`; record §6 "How it was derived" / "Read but not bound".

The create chain reads `keys::SUBTITLE_STORED_SOURCES` (`"subtitles.stored_sources"`, outside both globs) through `subtitle_source::stored_as_empty` → `classify` → `access.is_enabled()` → `enabled()` (`plurxd/src/subtitle_source.rs:1152, 1238, 610-614, 652-654`, a plain `store.get_setting`):

- ingress: `burn_would_discard_this_session_hdr` (`http/hls/create.rs:1440-1477`), called from the create at `:2923`, `:3546`, `:6710` — decides `422 hdr_subtitle_burn_refused` versus a start without an overlay;
- owner: `burn_track_is_stored_empty` (`transcode/manager/create.rs:1201-1213`) from `prepare_vod_encoding_with_source_scoped` (`:2897-2899`) — decides copy versus full re-encode.

**Failure:** an operator flips `subtitles.stored_sources` (it is the documented kill switch for a bad stored artifact) between the ingress plan and the owner's build. The generation does not move, so the owner accepts binding *b* and builds a different shape from the one the ingress validated (ingress: "track is stored-empty, keep HDR copy"; owner: "store off, burn → SDR re-encode"), which is exactly what §4.4 says cannot happen after D3t ("at *b* means the same values the ingress read"). The key is in neither the bound list nor the record's reasoned exclusions, and the hand-written chain pin (`candidates.rs:2366`) does not contain it, so the "exactly the chain's keys the globs miss" assertion passes vacuously.

This matters more than a normal omission because the list cannot be widened cheaply afterwards (S2).

**Fix:** add `keys::SUBTITLE_STORED_SOURCES` to the SQL IN-lists, `PLAYBACK_INPUT_EXTRA_KEYS`, the literal pin, the chain list in `candidates.rs`, and record §6 — or, if the coordinator rules that the burn guard stays a Store read outside the plan in D3b, list it under "Read but not bound" with that reason and have §4.2/§6.1 say so. Either way it must be an explicit decision before the marker ships at version 1.

## Should-fix

### S1. SQLite takes a schema version; §4.1's rollback promise does not hold on a standalone store

`sqlite/mod.rs:1346-1348`; record §7-1. §4.1: "No schema version bump: old binaries ignore unknown trigger names … a rollback leaves them in place." On SQLite `migrate` refuses any `user_version` above its own (`sqlite/mod.rs:1838-1845`), so after one start on D3t a standalone node cannot be rolled back to the previous binary; the record acknowledges it for backups but not for rollback. The record's reasoning ("the only way an existing SQLite database gains the triggers") is not true: an idempotent post-migrate install (B1 fix b) does it without an ordinal. Also, "installed as an extension with its own shape marker … on both stores" is only half met: nothing on the SQLite side ever reads `playback_input_keys_schema` or checks the trigger shape. If v129 is kept, this is a contract conflict for Paul/the coordinator to reconcile, not a builder-recorded decision.

### S2. Exact-shape marker makes any later list change a hard open failure on D3t binaries

`hiqlite.rs:4170-4196`, `playback_input_keys.sql:6-9` (`CHECK(version=1)`), record §6 last sentence ("…added … with a new marker version").

`playback_input_keys_shape` returns `Err("incompatible shape")` unless the four objects' stored SQL equal this binary's text byte for byte. The moment a later binary changes an IN-list (B2's key, "any encoded-path key D3a finds", any future lane), every D3t-era binary — a voter not yet upgraded that restarts, or a rollback — fails `migrate_schema` and does not open. That is a schema-version bump in everything but name, which §2 forbids. The precedent (`source_arbitration_shape`) has the same property, but its object set is not a list expected to grow.

**Fix:** make the trigger names carry the list revision (`playback_input_keys_v1_*`) and have the shape check validate only the objects of the revision this binary knows, tolerating unknown higher-revision `playback_input_keys_*` objects and a marker `version >= 1` (drop the `CHECK(version=1)`); a newer binary then adds `v2` triggers additively (double bumps are harmless) and old binaries keep opening. At minimum, record the constraint so D3b does not discover it.

### S3. The store-contract pin does not do what §4.1 and the brief ask

`tests/store_contract.rs:21598-21663`. Contract: "a write to every key in `QUALITY_PLANNING_KEYS` moves the generation on both stores." The test writes the five extra keys plus one representative per glob. The other 21 keys are covered only by `playback_input_key_moves_generation` (`playback_input_keys.rs:35-39`), a Rust `starts_with` restatement of the SQL `GLOB`s that nothing ties to `PLAYBACK_INPUT_SCHEMA` — so the plurxd census (`candidates.rs:2336`) proves agreement with a predicate, not with the triggers. **Fix:** move the full key list (or a `pub` copy pinned equal to `QUALITY_PLANNING_KEYS` by a plurxd test) into plurx-core and loop the contract test over it; or add a core unit test that runs each `QUALITY_PLANNING_KEYS`-shaped key through real SQL and asserts it agrees with the predicate.

### S4. The create-chain census is a hand list, so B2-class misses cannot fail it

`candidates.rs:2366-2427`. The brief asked for "a census or pin so a key added later without trigger coverage fails." That holds for `QUALITY_PLANNING_KEYS`, but the chain list is typed by hand from a reading of the code; a new `get_setting` in the create chain (or one missed, as in B2) fails nothing. **Fix:** either a source census in the `inv.rs` style over the create-chain files (every `get_setting`/`get_settings`/`get_setting_pair`/`num_setting` key literal must be covered or on a reasoned exclusion list), or state plainly in the record that the list is a reviewed derivation that D3b's §6.1 source walk will replace, and make D3b's walk assert coverage.

## Nits

- **N1.** `hiqlite.rs:4209`: the raced-install guard uses `name LIKE 'playback_input_keys_%'` while the shape check uses the exact `IN (names)` list. `_` is a LIKE wildcard, and a future sibling object with that prefix makes the guard fail forever while the shape reads `None`. Use the same `IN ({names})` as the precedent (`:4146`).
- **N2.** `consistent_read_census.rs:51-52`, contract §2 ("D3 adds no `query_consistent*` site"): D3t adds two (67→69). Unavoidable with the precedent, but it is a departure and belongs in record §7, not only in §6's prose.
- **N3.** No-op writes move the generation (`put_setting` with the same value; `hiqlite_import.rs:2756` `UPDATE settings SET updated_at…`). Same as the glob triggers and safe-side (one extra owner refusal + re-plan), so no change asked; say so in the record since `cluster.*` switches are now bound.
- **N4.** `docs/streaming/PLAYBACK-START-STATUS.md:52` still says the schema is SQLite v127/v128; stale if v129 is kept.
- **N5.** `validation_set_schema_lineage` (`hiqlite.rs:5181-5189`, contract-test only) drops `playback_input_generation` on a store that now also carries the D3t triggers; harmless today because no lineage test writes a bound key in that window, but worth a `DROP TRIGGER playback_input_keys_*` beside it.

## Checked and found sound

- **Trigger semantics.** Insert / update (NEW or OLD key) / delete mirror the glob triggers exactly; rename in and out counts; `ON CONFLICT DO UPDATE` fires the update trigger, `INSERT OR REPLACE` fires at least the insert trigger; `WHERE 0` backup anchor fires nothing; no trigger exists on `playback_input_generation`, so no recursion; the `dv_queue_admission_settings_ai` self-delete touches only internal keys. No key is in both a glob and the list (pinned), so no accidental double bump except on a cross-family rename (harmless).
- **Hiqlite install.** Bootstrap (`:2168`) and `SchemaMigrationAction::Current` (`:2370`) both install; a `MigrateFrom` chain loops to `Current`, so upgrade and restore of an older backup both reach it; idempotent on restart (`Some(true)` short-circuit); one replicated transaction, so every replica and every snapshot carries the same objects; expected text is derived from the same linked SQLite, as the precedent does.
- **Lineage bridge.** `PLAYBACK_INPUT_SCHEMA` is byte-identical; `schema_lineage` `OBJECT_QUERY`/`FINGERPRINT_QUERY` select by a fixed name list that does not include the new objects, so old and new backups fingerprint as before. No cluster schema version, protocol or wire change.
- **Labels.** Both new reads carry `"playback_input_keys_shape"` with `// authority:` reasons; the install goes through `schema_migration_transaction` (already labelled).
- **Formal rebind (001005092).** The three pinned sha256 values equal the blobs at head; every anchor in `hiqlite.rs` (`:298, :308, :1716, :3734, :7515`), `sqlite/mod.rs` and `store_contract.rs` is far from the D3t hunks (`hiqlite.rs:2168, 2370, 4167-4226`; `sqlite/mod.rs:1346`; `store_contract.rs:1019, 21591`) and the anchored text is unchanged. Honest.
- **Old-code failure.** On the base, the five extra keys move nothing, so both new store tests fail there as intended. No default seeding of the five keys exists, so the contract test's first write is a real insert.

## Dispositions (2026-11-12, applied on `claude/ps-d3t`)

The coordinator decided each; paths are under `crates/plurx-core/` unless a path is given. The build record is [PLAYBACK-START-D3-RECORD.md](../streaming/PLAYBACK-START-D3-RECORD.md) §6-§8.

| # | Finding | Disposition | Where |
|---|---|---|---|
| B1 | SQLite v129 turns three pinned tests red | **Fixed (reviewer's option b).** No `MIGRATIONS` entry: `install_sqlite` runs from `SqliteStore::init` after `migrate`, so `user_version` stays 128 and the three pins hold untouched. The two wind-back fixtures drop the triggers and marker beside `playback_input_generation`. | `src/store/playback_input_keys.rs`, `src/store/sqlite/mod.rs`, `src/store/sqlite/dv_conversion.rs`, `tests/store_contract.rs` |
| B2 | `subtitles.stored_sources` read by both sides of a burn start is unbound | **Fixed.** Bound in the SQL, `PLAYBACK_INPUT_EXTRA_KEYS`, the literal pin and the record. The source scan (S4) fails without it: removing the key from the list fails `every_setting_the_create_chain_reads_is_bound_or_excluded` naming `subtitle_source.rs: keys::SUBTITLE_STORED_SOURCES`. | `src/store/playback_input_keys.{rs,sql}`; `crates/plurxd/src/transcode/manager/candidates.rs` |
| S1 | SQLite takes a schema version; rollback promise fails | **Fixed with B1.** SQLite has the same exact-shape check as the replicated store (`sqlite_shape`), and no ordinal. | `src/store/playback_input_keys.rs` |
| S2 | Exact-shape marker makes a later list change an open failure | **Fixed.** Trigger names carry the revision (`playback_input_keys_r1_*`); the shape check reads only revision-1 objects, tolerates later-revision objects, and the marker is `version >= 1`. `a_partial_shape_is_refused_and_a_later_revision_is_tolerated` proves both. | `src/store/playback_input_keys.{rs,sql}`, `src/store/hiqlite.rs` |
| S3 | Contract pin covers 5 + 2 keys, predicate untied to the SQL | **Fixed.** The contract test writes every `PLAYBACK_PLANNING_KEYS` key, every extension key and a representative of each glob, on both stores; `the_store_contract_key_list_is_the_planning_list` (plurxd) pins that list equal to `QUALITY_PLANNING_KEYS`; `the_predicate_and_the_triggers_describe_the_same_set` runs the predicate against the real triggers. | `tests/store_contract.rs`; `src/store/playback_input_keys.rs`; `crates/plurxd/src/transcode/manager/candidates.rs` |
| S4 | The create-chain list is hand-typed | **Fixed.** `every_setting_the_create_chain_reads_is_bound_or_excluded` scans the create-chain files for `keys::NAME`, resolves each from the `keys` module, and fails an unbound key that is not on `UNBOUND_CHAIN_READS` (one entry, with its reason). It fails without the B2 fix. | `crates/plurxd/src/transcode/manager/candidates.rs` |
| N1 | Raced-install guard uses `LIKE` | **Fixed.** Exact `IN` list from `OBJECT_NAMES`, as the precedent. | `src/store/hiqlite.rs` |
| N2 | 67 to 69 `query_consistent` sites | **Recorded** as decision 5 in the D3 record; the new label is allow-listed as startup in the label census. | `docs/streaming/PLAYBACK-START-D3-RECORD.md` §7 |
| N3 | No-op writes move the generation | **Recorded** as decision 6; a unit test asserts it. | D3 record §7; `src/store/playback_input_keys.rs` |
| N4 | STATUS.md line 52 | **Fixed.** With no v129 the schema line is accurate; a sentence names the extension and that no version moves. | `docs/streaming/PLAYBACK-START-STATUS.md` |
| N5 | `validation_set_schema_lineage` leaves the triggers | **Fixed.** The downgrade drops the three triggers and the marker beside `playback_input_generation`. | `src/store/hiqlite.rs` |
