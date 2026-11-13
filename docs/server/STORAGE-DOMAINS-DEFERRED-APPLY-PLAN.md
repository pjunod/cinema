# Storage domains — save always, apply when safe

**Status:** implementation in progress · owned by Codex · see
[execution status](STORAGE-DOMAINS-DEFERRED-APPLY-STATUS.md) ·
**Updated:** 2026-11-12 · **Source baseline:** `origin/main` at `54aebebf9`
(after PR #1546)

Settings → Libraries → **Shared storage budgets** cannot be saved on a busy
cluster. The save is refused with HTTP 409 whenever any background lease,
analysis reservation, background custody, unsettled playback owner or live
pause exists. Playback is almost always active, and a stuck custody row
(the separate bug in [source custody recovery](SOURCE-CUSTODY-RECOVERY.md))
blocks it permanently. This plan makes the save a pure configuration write
and moves the safety rule to the point where the configuration is applied.

Companion documents: [source custody recovery](SOURCE-CUSTODY-RECOVERY.md)
(why stuck custody exists), [job queue RCA](JOB-QUEUE-ROOT-CAUSE-AND-FIX.md)
(the storage-domain arbiter), [API](../API.md) rows for
`/api/v1/cluster/work/storage-domains`.

## 1. What happens today

| Step | Code | Behaviour |
|---|---|---|
| UI | `crates/plurxd/src/web/pages/settings-panels.js` `storageDomainsCard` (≈466), `saveStorageDomains` (≈487) | PUTs every non-empty input as `[{library_id, root_path, domain_id}]` |
| HTTP | `crates/plurxd/src/http/background_jobs.rs::replace_storage_domains` (≈418) | `validate()`, then 409 "Storage identities cannot change while background work owns live reservations…" when the Store says `false` |
| Store | `crates/plurx-core/src/store/background_jobs_resources.rs::replace` (≈41) | One `INSERT INTO background_job_commands … 'storage_domains'` whose `result_json` is `'true'` only when no holder of any kind exists |
| Trigger | `background_jobs_resources.sql` `background_storage_domains_replace` | Deletes and re-inserts `background_storage_domains` when `result_json='true'` |

The refusal predicate checks every holder in the cluster, not the holders a
change would actually affect.

## 2. How resource keys are used — the facts the safety rule depends on

### 2.1 Capacity uses keys stored at claim time

| Holder | Stored keys | Written by |
|---|---|---|
| Job reservation | `background_job_reservations.resource_key` | claim, from view `background_job_required_resources` (`background_jobs_resources.sql` ≈17, replaced by later `subtitle_managed_*` views) |
| Analysis reservation | `analysis_source_reservations.resource_key` | claim and every physical phase start, from view `analysis_required_resources` (`background_jobs_viewer_analysis.sql` ≈297) |
| Background custody | `background_source_custody.resource_keys_json` | `register_background_custody` (`background_jobs_resources.rs` ≈3145; ReadAttestation/Offline derive at ≈3430) |
| Playback owner | `foreground_source_owners.resource_keys_json` | `register_foreground` (≈273, derivation ≈294) |
| Pause | `background_source_pauses.resource_keys_json` | `pause_background`; reused unchanged by `resume_background` |

Every capacity predicate (`foreground_blocks`, `claim_background_service_inner`,
`resume_background`, `project_foreground`, `analysis_source_capacity_terms`
in `fragment_index_cluster.rs`, `background_job_turns.sql/.rs`) compares a
**new** claim's keys, derived from the current mapping, with **stored**
holder keys. Two `source_io*` keys conflict when they are equal or either is
bare `source_io`. Capacity counts distinct holders (two per domain).

### 2.2 Job ownership re-derives keys from the current mapping

This is the fact the first draft missed. A running job keeps its ownership
only while **every** key its library derives *now* is held, exactly:

| Predicate | Where |
|---|---|
| `RENEW_SQL` — returns `lost_ownership` unless each `background_job_required_resources` key is held in `background_job_reservations` (or the job's pause row) | `crates/plurx-core/src/store/background_jobs.rs` ≈691–720 |
| `CLAIM_RESOURCE_SNAPSHOT` — the same test at claim acknowledgement and resolution | ≈2976, used ≈4756, ≈4827 |
| The same `NOT EXISTS (… required … NOT EXISTS held …)` ownership test in completion, publication and per-kind paths | `background_jobs.rs` ≈596–612; `background_jobs_domain.rs` ≈40; `background_jobs_embeddings.rs` ≈246; `background_jobs_integrity.rs` ≈128; `background_jobs_library.rs` ≈319; `background_jobs_probe.rs` ≈121; `background_jobs_provider.rs` ≈53; `background_jobs_subtitle.rs` ≈136; `background_jobs_transcription.rs` ≈50, ≈353; `dv_title_jobs.rs` ≈293, ≈314; `hiqlite_publication.rs` ≈56; `sqlite/mod.rs` ≈2060; `background_jobs_resources.sql` ≈56, ≈73 |

The test is exact set inclusion, not conflict coverage. A job holding
`source_io` loses ownership as soon as its root maps to `nas-a`, because
`source_io:nas-a` is now required and not held. Changing these predicates
would not help in a rolling upgrade: an older node sends its own SQL.

### 2.3 Every other re-derivation site, and why it is safe

| Site | What it derives | Why applying is safe |
|---|---|---|
| `analysis_coordination.rs` ≈289 (physical phase start), `analysis_coordination.sql` ≈89, `background_jobs_viewer_analysis.sql` ≈319/≈333 | inserts `analysis_source_reservations` from the current mapping, behind `analysis_source_capacity_clause` | a new acquisition under the new mapping, capacity-checked against stored holders. Existing reservations continue to charge their stored keys. Current physical-phase completion clears its reservation before a later phase begins; that later acquisition derives the applied keys and retries a capacity refusal without losing the coordinator claim (§8.1 test) |
| `fragment_index_cluster.rs` ≈293 `analysis_source_admission_sql` | capacity check for an already-claimed request, current keys | same: an admission decision under the new mapping, never an ownership loss |
| `background_job_turns.sql` ≈40, ≈87–89; `background_job_turns.rs` ≈36, ≈60 | a *candidate* scan's keys from the current mapping, compared with stored held keys | candidate side only; equivalent to a new claim. `dispatch_turn_wait.keys_json` is fairness state, not capacity; a stale entry misorders at most one turn |
| `background_jobs_resources.rs` ≈571 `foreground_pressure`; ≈3430 ReadAttestation/Offline registration | a new request's keys | new claims |
| `background_jobs_probe.sql` ≈27, `subtitle_managed_resources_v2…v6.sql`, `subtitle_managed_schema.sql` ≈17 | redefinitions of `background_job_required_resources` | covered by the job rule (§3.1 B) |

**Every derivation is library-wide:** each maps **every root of the item's
library** to `COALESCE('source_io:'||mapping.domain_id,'source_io')`.

### 2.4 Mappings of removed roots stay behind

Nothing deletes a `background_storage_domains` row when its library or root
is removed. Such rows are inert in every derivation (all join through
`libraries.paths`), so the safety rule must ignore them too.

## 3. The safety rule

### 3.1 Statement

A **changed root** is a root currently listed in some `libraries.paths`
whose applied domain (or none) differs from its pending domain (or none).
Rows for roots no longer in `libraries.paths` are ignored.

A named key `source_io:d` is **disturbed** when some changed root is
currently applied to `d`. Bare `source_io` is never disturbed.

The pending revision is applied, atomically, once both hold:

- **A — no reader would lose a conflict.** No live holder holds a disturbed
  key while holding no bare `source_io` key. Live: reservation not expired,
  or custody present, or playback lease live, or `custody_held=1`, or a pause
  of a running job.
- **B — no running job would lose ownership.** No job in state `running`
  or `cancelling` with an unexpired lease (paused jobs are `running`) belongs
  to a library with a changed root and has a `source_io*` row in
  `background_job_required_resources`.

### 3.2 Why this is sufficient

*Rule A, capacity.* Write *K(H)* for a holder's stored keys and *K′(H)* for
the keys its library derives under pending. A new claim *C* must be counted
against *H* when *K′(C)* conflicts with *K′(H)*; it is counted when *K′(C)*
conflicts with *K(H)*. That is guaranteed when *K(H)* contains bare
`source_io`, or *K′(H)* ⊆ *K(H)*. Invariant: for every live holder without a
bare key, each live root of its library maps to a key in *K(H)*. It holds at
claim; an application with no disturbed key in *K(H)* keeps it, because every
root mapped to a key in *K(H)* keeps that key. The older nodes' all-idle
replace also keeps it. Re-derivation sites of §2.3 are new acquisitions and
fall under *C*.

*Rule B, ownership.* A job's required set changes only if a root of its
library changes. With no such running job, every running job's required set
after the application equals the one before, which it already holds. Jobs
claimed after the application derive and reserve the new keys.

### 3.3 Consequences

| Change | Applies |
|---|---|
| Root moves from bare to `nas-a` | when no running job in that library remains. A stuck bare custody, playback owners and analysis reservations do not block |
| Root moves `nas-a` → `nas-b` | when no live holder of `source_io:nas-a` without a bare key remains **and** no running job in that library remains |
| Root moves `nas-a` → bare | same as above |
| Roots of `nas-b` merged into `nas-a` | holders of `nas-a` do not block; holders of `nas-b` and running jobs in the moved root's library do |
| Unchanged save, or only removed roots differ | immediately |
| Holder with `{source_io, source_io:nas-a}` | never blocks under A; may block under B if it is a running job of a changed library |

Rule A is conservative on a split: it waits for every holder of `nas-a`, not
only those whose roots moved. Rule B waits for library jobs, which are
usually short scans and probes. A stuck named custody blocks only its own
domain, and the UI says it is stuck.

## 4. Data model

### 4.1 New objects

New module `crates/plurx-core/src/store/storage_domain_pending.rs` with
`storage_domain_pending.sql`, modelled on `custody_retention.rs`
(`SCHEMA`, `SHAPE_SQL`, `MARKER_SQL`, `statements()`, `version()` with a
LazyLock shape built from a freshly migrated SQLite catalogue) and
`pub const SQLITE_INTRODUCED_SCHEMA: i64 = 131;`.

```sql
-- Desired storage identities. Saving never depends on runtime state; the
-- daemon applies the newest revision once no holder would lose a conflict
-- and no running job would lose ownership.
CREATE TABLE IF NOT EXISTS background_storage_domain_pending (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    revision INTEGER NOT NULL CHECK (revision > 0),
    mappings_json TEXT NOT NULL CHECK (json_valid(mappings_json) AND json_type(mappings_json) = 'array'),
    base_json TEXT NOT NULL CHECK (json_valid(base_json) AND json_type(base_json) = 'array'),
    saved_at_ms INTEGER NOT NULL CHECK (saved_at_ms >= 0),
    saved_by INTEGER,
    state TEXT NOT NULL CHECK (state IN ('pending','applied','superseded')),
    settled_at_ms INTEGER,
    settled_reason TEXT CHECK (settled_reason IS NULL OR length(settled_reason) <= 64)
) STRICT;
-- next statement
-- Applied rows whose root is still a library root. Rows for removed roots are inert.
CREATE VIEW IF NOT EXISTS background_storage_domains_live AS
SELECT applied.library_id, applied.root_path, applied.domain_id
FROM background_storage_domains applied JOIN libraries library ON library.id = applied.library_id
WHERE EXISTS (SELECT 1 FROM json_each(CASE WHEN json_valid(library.paths) THEN library.paths ELSE '[]' END) root
    WHERE root.value = applied.root_path);
-- next statement
CREATE VIEW IF NOT EXISTS background_storage_domain_changed_roots AS
SELECT library.id AS library_id, root.value AS root_path,
    (SELECT applied.domain_id FROM background_storage_domains applied
      WHERE applied.library_id = library.id AND applied.root_path = root.value) AS applied_domain
FROM background_storage_domain_pending pending, libraries library,
    json_each(CASE WHEN json_valid(library.paths) THEN library.paths ELSE '[]' END) root
WHERE pending.state = 'pending'
  AND COALESCE((SELECT applied.domain_id FROM background_storage_domains applied
        WHERE applied.library_id = library.id AND applied.root_path = root.value), '')
   != COALESCE((SELECT json_extract(wanted.value, '$.domain_id') FROM json_each(pending.mappings_json) wanted
        WHERE json_extract(wanted.value, '$.library_id') = library.id
          AND json_extract(wanted.value, '$.root_path') = root.value), '');
-- next statement
CREATE VIEW IF NOT EXISTS background_storage_domain_disturbed AS
SELECT DISTINCT 'source_io:' || applied_domain AS resource_key
FROM background_storage_domain_changed_roots WHERE applied_domain IS NOT NULL;
-- next statement
-- Rule A holders: one row per (holder, stored key). The caller's clock decides liveness.
CREATE VIEW IF NOT EXISTS background_storage_domain_holders AS
SELECT held.owner_kind AS holder_kind, held.work_id, held.fence, held.resource_key,
    held.expires_at_ms, CASE WHEN held.custody_id IS NULL THEN 0 ELSE 1 END AS custody_held,
    CASE held.owner_kind
      WHEN 'job' THEN (SELECT job.owner_node_id FROM background_jobs job WHERE job.id = held.work_id)
      WHEN 'analysis' THEN (SELECT request.owner_node_id FROM analysis_requests request WHERE request.request_id = held.work_id)
      ELSE (SELECT CASE custody.owner_kind
              WHEN 'lease' THEN json_extract(custody.owner_json, '$.lease.owner_node_id')
              ELSE json_extract(custody.owner_json, '$.node_id') END
            FROM background_source_custody custody WHERE custody.custody_id = held.custody_id)
    END AS node_id, NULL AS library_id
FROM source_io_occupancy held WHERE held.resource_key LIKE 'source_io%'
UNION ALL
SELECT 'foreground', owner.owner_id, owner.fence, fg.resource_key, fg.lease_expires_at_ms,
    fg.custody_held, json_extract(owner.identity_json, '$.node_id'), NULL
FROM foreground_source_permissions fg JOIN foreground_source_owners owner ON owner.owner_id = fg.owner_id
WHERE fg.resource_key LIKE 'source_io%'
UNION ALL
SELECT 'pause', paused.job_id, paused.fence, resource.value, job.lease_expires_ms, 0, paused.node_id, NULL
FROM background_source_pauses paused
JOIN background_jobs job ON job.id = paused.job_id AND job.fence = paused.fence
    AND job.state IN ('running','cancelling'),
json_each(paused.resource_keys_json) resource WHERE resource.value LIKE 'source_io%';
-- next statement
-- Rule B: running jobs whose required keys would change.
CREATE VIEW IF NOT EXISTS background_storage_domain_affected_jobs AS
SELECT DISTINCT job.id AS job_id, job.fence, job.owner_node_id AS node_id,
    job.lease_expires_ms, changed.library_id
FROM background_jobs job
LEFT JOIN files file ON file.id = json_extract(job.payload_json, '$.file_id')
LEFT JOIN items item ON item.id = COALESCE(file.item_id, json_extract(job.payload_json, '$.item_id'))
JOIN background_storage_domain_changed_roots changed
    ON changed.library_id = COALESCE(json_extract(job.payload_json, '$.library_id'), item.library_id)
WHERE job.state IN ('running','cancelling')
  AND EXISTS (SELECT 1 FROM background_job_required_resources required
      WHERE required.job_id = job.id AND required.resource_key LIKE 'source_io%');
-- next statement
CREATE TRIGGER IF NOT EXISTS background_storage_domains_apply
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'storage_domains_apply'
BEGIN
    DELETE FROM background_storage_domains
    WHERE json_extract(NEW.result_json, '$.verdict') = 'applied';
    INSERT INTO background_storage_domains(library_id, root_path, domain_id)
    SELECT json_extract(wanted.value, '$.library_id'), json_extract(wanted.value, '$.root_path'),
        json_extract(wanted.value, '$.domain_id')
    FROM background_storage_domain_pending pending, json_each(pending.mappings_json) wanted
    WHERE json_extract(NEW.result_json, '$.verdict') = 'applied'
      AND EXISTS (SELECT 1 FROM libraries library,
            json_each(CASE WHEN json_valid(library.paths) THEN library.paths ELSE '[]' END) root
          WHERE library.id = json_extract(wanted.value, '$.library_id')
            AND root.value = json_extract(wanted.value, '$.root_path'));
    UPDATE background_storage_domain_pending
    SET state = json_extract(NEW.result_json, '$.verdict'),
        settled_at_ms = json_extract(NEW.request_json, '$.now_ms'),
        settled_reason = json_extract(NEW.result_json, '$.reason')
    WHERE state = 'pending' AND json_extract(NEW.result_json, '$.verdict') IN ('applied','superseded');
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;
-- next statement
CREATE TABLE IF NOT EXISTS background_storage_domain_pending_schema (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    version INTEGER NOT NULL
) STRICT;
-- next statement
INSERT OR IGNORE INTO background_storage_domain_pending_schema (singleton, version) VALUES (1, 1);
```

Node paths follow `background_source_current_owners`
(`background_jobs_foreground.sql` ≈138–152): `$.token.node_id` for job
custody (job rows are answered from `background_jobs` above), `$.node_id`
for analysis and offline, `$.lease.owner_node_id` for lease. An application
deletes every applied row, including removed-root rows, and inserts only
pending rows whose root still exists, so stale rows do not survive it.

### 4.2 SQLite

Append `super::storage_domain_pending::SCHEMA` to `MIGRATIONS` in
`crates/plurx-core/src/store/sqlite/mod.rs` after v130
`offload::SQLITE_MIGRATION` (≈1346–1347) as **v131** with the comment
`// v131: storage-domain changes are saved at once and applied when safe.`
Current main uses v130 for offload; v131 follows it. Add
`"CREATE TABLE IF NOT EXISTS background_storage_domain_pending"` to the
recognised-statement list in `crates/plurx-core/src/store/sqlite/dv_conversion.rs`
beside `background_custody_retention_schema` (≈1766).

### 4.3 hiqlite

In `crates/plurx-core/src/store/hiqlite.rs`, add
`storage_domain_pending_schema_version()` and
`install_storage_domain_pending_schema(admission)` as copies of the
custody-retention pair (≈4618–4675). `SHAPE_SQL` names
`background_storage_domain_pending`, `background_storage_domains_live`,
`background_storage_domain_changed_roots`, `background_storage_domain_disturbed`,
`background_storage_domain_holders`, `background_storage_domain_affected_jobs`,
`background_storage_domains_apply` and `background_storage_domain_pending_schema`.
Call it after `install_custody_retention_schema` in the bootstrap list
(≈2128) and in the `SchemaMigrationAction::Current` chain of
`migrate_schema` (≈2324, where the chain's `return` moves to the new call).
A partial shape is refused, as for every extension.

Also add the table to `hiqlite_import.rs` `TablePlan` beside
`background_storage_domains` (≈1828, `minimum_schema` 131) and the
table-to-schema map in `hiqlite_import/tests.rs` (≈1056); `hiqlite_durable.rs`
`DurableDump` (≈416, ≈472); `validation_reset_contract_state` beside
`DELETE FROM background_storage_domains` (≈5471); the table list in
`crates/plurx-core/tests/store_contract.rs` (≈19210).

### 4.4 Rolling upgrade

| Node | Reads | Writes | Effect |
|---|---|---|---|
| Older binary | `background_storage_domains` only | old `'storage_domains'` command; old trigger stays installed | unchanged; its all-idle rule implies rules A and B |
| Newer binary | both tables | `'storage_domains_apply'` only | applies pending when safe |

Older nodes run their own renew and claim SQL against the applied table;
rule B is what keeps their running jobs owned across an application. If an
older node replaces the mappings while a revision is pending, `base_json` no
longer matches the live applied rows and the revision is marked
`superseded` (reason `applied_changed_elsewhere`). The old trigger is
removed in a later cleanup, once the whole fleet runs the new binary.

## 5. Store API

Types in `background_jobs_resources.rs`; trait methods at
`background_jobs.rs` ≈2616 (impls ≈3689):

```rust
pub struct StorageDomainWait { pub holder_kind: String, pub work_id: String, pub fence: i64,
    pub node_id: Option<String>, pub resource_key: Option<String>, pub library_id: Option<i64>,
    pub reason: String /* "old_domain_reader" | "library_job" */, pub stuck: bool }
pub struct StorageDomainPending { pub revision: i64, pub mappings: Vec<StorageDomainMapping>,
    pub saved_at_ms: i64, pub saved_by: Option<i64>, pub state: String,
    pub settled_at_ms: Option<i64>, pub settled_reason: Option<String> }
pub struct StorageDomainState { pub applied: Vec<StorageDomainMapping>,
    pub pending: Option<StorageDomainPending>, pub waiting_on: Vec<StorageDomainWait>, pub waiting_total: i64 }
pub enum StorageDomainApply { Applied { revision: i64 }, Waiting { revision: i64 },
    Superseded { revision: i64 }, NothingPending }

async fn storage_domains(&self) -> Result<Vec<StorageDomainMapping>, StoreError>;      // unchanged
async fn save_storage_domains(&self, mappings: Vec<StorageDomainMapping>, saved_by: Option<i64>, now_ms: i64)
    -> Result<Option<i64>, StoreError>;          // Some(revision); None: a root is not a library root
async fn apply_pending_storage_domains(&self, now_ms: i64) -> Result<StorageDomainApply, StoreError>;
async fn storage_domain_state(&self, now_ms: i64) -> Result<StorageDomainState, StoreError>;
```

`replace_storage_domains` is removed from the trait; callers move to
save + apply (§8.4). `validate()` keeps its rules; the 64 KiB body bound is
checked before any I/O.

### 5.1 Save (`$1` = `{"mappings":[…],"now_ms":…,"saved_by":…}`)

```sql
INSERT INTO background_storage_domain_pending(singleton, revision, mappings_json, base_json,
    saved_at_ms, saved_by, state, settled_at_ms, settled_reason)
SELECT 1, COALESCE((SELECT revision FROM background_storage_domain_pending WHERE singleton = 1), 0) + 1,
    json_extract($1, '$.mappings'),
    (SELECT json_group_array(json_object('library_id', library_id, 'root_path', root_path, 'domain_id', domain_id))
       FROM background_storage_domains_live),
    json_extract($1, '$.now_ms'), json_extract($1, '$.saved_by'), 'pending', NULL, NULL
WHERE NOT EXISTS (SELECT 1 FROM json_each($1, '$.mappings') mapping
    WHERE NOT EXISTS (SELECT 1 FROM libraries library,
        json_each(CASE WHEN json_valid(library.paths) THEN library.paths ELSE '[]' END) root
      WHERE library.id = json_extract(mapping.value, '$.library_id')
        AND root.value = json_extract(mapping.value, '$.root_path')))
ON CONFLICT(singleton) DO UPDATE SET revision = excluded.revision, mappings_json = excluded.mappings_json,
    base_json = excluded.base_json, saved_at_ms = excluded.saved_at_ms, saved_by = excluded.saved_by,
    state = 'pending', settled_at_ms = NULL, settled_reason = NULL
RETURNING revision
```

No row returned means a mapping names a root that is not a current library
root: `Ok(None)`, and the handler answers HTTP 400 "A library root changed
since this page loaded. Load library roots again." That is an input error,
never a runtime-state refusal. A newer save replaces an older pending one.

### 5.2 Waiting query (`$1` = `{"now_ms":…}`), shared by apply and state

```sql
WITH now AS (SELECT json_extract($1, '$.now_ms') AS ms),
live AS (SELECT h.* FROM background_storage_domain_holders h, now
    WHERE h.expires_at_ms > now.ms OR h.custody_held = 1),
readers AS (
  SELECT h.holder_kind, h.work_id, h.fence, h.node_id, h.resource_key, NULL AS library_id,
      'old_domain_reader' AS reason, CASE WHEN h.expires_at_ms <= now.ms THEN 1 ELSE 0 END AS stuck
  FROM live h JOIN background_storage_domain_disturbed d ON d.resource_key = h.resource_key, now
  WHERE NOT EXISTS (SELECT 1 FROM live b WHERE b.holder_kind = h.holder_kind
      AND b.work_id = h.work_id AND b.fence = h.fence AND b.resource_key = 'source_io')),
jobs AS (
  SELECT 'job', j.job_id, j.fence, j.node_id, NULL, j.library_id, 'library_job', 0
  FROM background_storage_domain_affected_jobs j, now WHERE j.lease_expires_ms > now.ms)
SELECT * FROM readers UNION ALL SELECT * FROM jobs
```

The state call returns the first 32 rows ordered by `reason, resource_key,
holder_kind, work_id`, and `waiting_total` as
`count(DISTINCT json_array(work_id, fence))` over the same rows, so a paused
job (pause row, job reservation, affected job) counts once. Keep the text a
Rust `const` in the new module so apply and state cannot drift.

### 5.3 Apply (`$1` = `{"id":uuid,"now_ms":…}`)

```sql
INSERT INTO background_job_commands(id, operation, request_json, result_json)
SELECT json_extract($1, '$.id'), 'storage_domains_apply', $1, json_object('revision', pending.revision,
  'verdict', CASE
    WHEN pending.state != 'pending' THEN 'nothing'
    WHEN <base differs> THEN 'superseded'
    WHEN EXISTS (<§5.2 body>) THEN 'waiting'
    ELSE 'applied' END,
  'reason', CASE WHEN pending.state = 'pending' AND <base differs> THEN 'applied_changed_elsewhere' END)
FROM background_storage_domain_pending pending WHERE pending.singleton = 1
RETURNING result_json
```

`<base differs>` compares `pending.base_json` with
`background_storage_domains_live` as sets (an element of either missing from
the other), so removed-root rows never make a save look superseded. One
statement decides and the trigger applies, in one replicated transaction.
A concurrent save is ordered before or after it by the log; whichever
revision is pending at execution is the one judged. Two concurrent appliers
produce one `applied` and one `nothing`.

## 6. Who applies, and when

Only a node for which `may_run_cluster_jobs()` is true applies. That is the
set that already runs queue upkeep and job claims, so liveness (`now_ms`)
comes from the same clocks the job leases are judged by. The cluster clock
skew guard bounds disagreement between them.

1. **On save.** The PUT handler saves, then attempts one apply if this node
   may run cluster jobs. Otherwise it saves and reports the state read with
   its own clock (read-only), and the next voter tick applies.
2. **On every scheduler upkeep.** `crates/plurxd/src/state.rs`
   `maintain_background_queue` (≈6791), after `maintain_jobs` (≈6799):
   `apply_pending_storage_domains(clock_ms())`. Log `applied` and
   `superseded` at info with the revision; nothing for `waiting`.
3. **At daemon start.** Once, after migration and before the scheduler
   loop, beside the first `replay_settled_source_custody` (≈6590), if
   `may_run_cluster_jobs()`.

Latency after the last blocker finishes is at most one scheduler tick. No
new task, timer or watchdog is added.

## 7. HTTP and UI

### 7.1 API

`GET /api/v1/cluster/work/storage-domains` (admin):

```json
{"mappings":[…applied…], "libraries":[…], "slots_per_domain":2, "provider_slots":2,
 "pending": null | {"revision":7,"mappings":[…],"saved_at_ms":…,"saved_by":3,"state":"pending",
                    "settled_at_ms":null,"settled_reason":null},
 "waiting_on":[{"holder_kind":"foreground","work_id":"…","fence":1,"node_id":"media1",
                "resource_key":"source_io:nas-a","library_id":null,"reason":"old_domain_reader","stuck":false},
               {"holder_kind":"job","work_id":"…","fence":4,"node_id":"lab3",
                "resource_key":null,"library_id":2,"reason":"library_job","stuck":false}],
 "waiting_total":2}
```

`pending` is the latest revision in any state, so the UI can report a recent
`superseded`. `PUT` keeps its body and returns
`{"saved":true,"revision":7,"applied":false,"waiting_on":[…],"waiting_total":2}`.
200 on every save; 400 for invalid input or a stale root; 401/403 as today.
The 409 branch is deleted. Update `docs/API.md` ≈3764–3765.

### 7.2 Settings card copy (`settings-panels.js`)

Replace the second hint ("Save identity changes while background jobs are
idle…") with:

> Changes save at once and take effect as soon as nothing would lose its
> budget: library jobs on a changed root finish first, and readers on a
> domain a root is leaving finish first. Playback never has to stop.

Add `<div id="storage-domain-status" role="status"></div>` above the error
line. `saveStorageDomains` renders from the PUT result, `loadStorageDomains`
from GET:

| State | Status line | Toast |
|---|---|---|
| applied | "Saved and in effect (revision 7)." | "Storage domains saved and in effect" |
| pending, readers | "Saved (revision 7). Waiting on 3 readers on nas-a to finish before it takes effect." | "Storage domains saved — takes effect when current work finishes" |
| pending, jobs | "Saved (revision 7). Waiting on 2 library jobs in Movies to finish before it takes effect." (both sentences when both apply) | same |
| pending, some stuck | append: "2 of the readers belong to a process that is no longer running. They clear when source custody recovery settles them (Activity → Source readers)." | — |
| superseded | "Revision 7 was not applied: storage domains were changed from a node running an older version. Check the roots below and save again." | — |

Every pending line ends "Playback and jobs keep their current budgets until
then." Under it, list up to eight holders as
"<kind> <first 8 chars of work id> on <node> · <domain or library name>".
Kind labels: `job` "Background job", `analysis` "Analysis", `lease` "Library
lease", `offline` "Offline download", `foreground` "Playback", `pause`
"Paused job". Domain names drop the `source_io:` prefix; library names come
from the GET `libraries` list. When a revision is pending, load fills each
input with the pending value and marks changed roots with
`<span class="pill">pending · now <old or "Shared default"></span>`. Use
singular and plural forms.

## 8. Tests

Each test fails on today's code. Store contract tests go in
`crates/plurx-core/tests/store_contract/background_jobs.rs` and run on both
backends.

### 8.1 Store contract

| Test | Proves |
|---|---|
| `storage_domain_save_is_never_refused_for_runtime_state` | with a running job lease, an analysis reservation, a background custody, an unsettled playback owner and a live pause, save returns revision 1 (today `false`) |
| `storage_domain_bare_to_named_applies_beside_stuck_bare_custody` | a custody holding `["source_io"]` with expired authority, no running job in the library; mapping the root to `nas-a` applies at once; the custody row is untouched and still counted against a new `source_io:nas-a` claim |
| `storage_domain_bare_to_named_waits_for_running_job_in_library` | running job holds `source_io`; save → `waiting`, reason `library_job`; job finishes → applies |
| `storage_domain_running_job_survives_renew_and_claim_ack_across_apply` | job A runs in library 1, root of library 2 changes and applies; A renews (`RENEW_SQL`) and resolves its claim (`CLAIM_RESOURCE_SNAPSHOT`) as owned; a job in library 1 never sees an application while running |
| `storage_domain_named_to_named_waits_then_applies_after_release` | root on `nas-a`, job and playback owner hold `source_io:nas-a`; save to `nas-b` → waiting on both; applied table still `nas-a`; release both; `apply_pending` → `Applied` |
| `storage_domain_named_to_bare_waits_for_old_domain` | same shape, target unmapped |
| `storage_domain_merge_waits_only_for_the_moved_domain` | `r1→nas-a`, `r2→nas-b`, playback holder on `nas-a`; move `r2` to `nas-a` → applies; holder on `nas-b` instead → waits |
| `storage_domain_holder_with_bare_key_never_blocks_rule_a` | playback owner on a library with one mapped and one unmapped root; move the mapped root → applies |
| `storage_domain_removed_root_never_disturbs_or_supersedes` | applied row for a root later removed from the library; saves of unrelated roots apply and are not `superseded` |
| `storage_domain_paused_job_counts_once` | paused job with reservation and pause row → `waiting_total` = 1 |
| `storage_domain_stuck_named_custody_is_reported_stuck` | expired-authority custody on `nas-a`; save `nas-a→nas-b` → waiting, `stuck=true` |
| `storage_domain_analysis_phase_start_after_apply_reserves_new_domain` | running analysis request on a moved root starts a physical phase after the application: it reserves the new key, keeps the old row, and a capacity refusal leaves the request running (retry, not loss) |
| `storage_domain_pending_is_superseded_by_older_node_replace` | while pending, insert the old `'storage_domains'` command directly; apply → `Superseded` |
| `storage_domain_concurrent_appliers_apply_once` | two concurrent applies → one `Applied`, one `NothingPending` |
| `storage_domain_save_with_stale_root_is_input_error_without_pending_row` | unknown root → `Ok(None)`, no pending row |
| `storage_domain_newer_save_replaces_older_pending` | two saves while blocked → revision 2 pending |

### 8.2 Schema and import

- `storage_domain_pending.rs` unit test `extension_shape_equals_sqlite_v130_catalogue`.
- `crates/plurx-core/tests/support/source_extensions_fixture.rs`:
  `storage_domain_pending_install_is_idempotent_and_refuses_partial_shape`.
- `hiqlite_import` test: the table imports at schema ≥ 130 and is skipped below.

### 8.3 Daemon

- `http/background_jobs.rs`: `storage_domains_put_never_conflicts_on_runtime_state`
  (200 with a live playback owner; today 409) and
  `storage_domains_get_reports_pending_and_waiting_holders`.
- `state/tests.rs`: `upkeep_applies_pending_storage_domains_after_last_blocker`
  and `learner_saves_but_does_not_apply_storage_domains`.

### 8.4 Existing tests that change

`store_contract/background_jobs.rs` ≈1729, ≈2065, ≈2126 (the "unchanged"
refusal becomes a waiting assertion), ≈2169, ≈2206, ≈4838;
`store_contract/formal_publication.rs` ≈669; `store_contract.rs` ≈693
(method list) and ≈9828; `background_jobs_tests.rs` ≈2322;
`crates/plurxd/src/library_work.rs` ≈2130 and ≈2324 (save, apply, assert
`Applied`).

### 8.5 Web (`tests/web/settings-sections.test.js`)

- `storage domains save reports in effect`.
- `storage domains save reports pending readers and library jobs` — two
  readers on `source_io:nas-a` (one stuck) and one library job; the status
  names both and the stuck sentence; the error element stays empty.
- `storage domains load marks pending roots`.
- `storage domains superseded asks to save again`.

`tests/web/cluster-membership.test.js` ≈2841 needs no route change.

### 8.6 Gate

Fast lane: `cargo test -p plurx-core --test store_contract storage_domain`
(both backends), `cargo test -p plurx-core storage_domain_pending`,
`cargo test -p plurxd storage_domains`,
`node --test tests/web/settings-sections.test.js`. Full suite once after the
review fixes.

## 9. Docs touched

- `docs/API.md` ≈3764–3765: GET fields, PUT result, no 409.
- `docs/OPERATIONS.md` ≈4637: saving is immediate; what it waits for; the
  card shows it.
- `docs/features/SETTINGS-NAVIGATION-AND-DEVELOPER-STATUS.md` ≈202: the
  pending state.
- This plan moves to `STORAGE-DOMAINS-DEFERRED-APPLY.md` in this folder, linked
  from `docs/server/README.md`.

## 10. Delivery

One corrective PR to `main` (`claude/storage-domains-deferred-apply`). Nothing
is enabled or gated, so no Developer-tab entry. Deploy order does not matter
(§4.4).

## 11. Review

Fable reviewed the first draft against `origin/main` at `54aebebf9`.

| Finding | Resolution |
|---|---|
| A1 blocker: renew and claim-ack re-derive job keys | §2.2 lists every ownership predicate; rule B (§3.1) blocks on running jobs in a library with a changed root; §3.3, UI copy and §8.1 (`…bare_to_named_waits_for_running_job…`, `…running_job_survives_renew_and_claim_ack…`) updated |
| A2: incomplete derivation list | §2.3 covers each site with its argument; analysis phase start gets its own test |
| A3: removed-root rows | `background_storage_domains_live`; changed roots and `base_json` use live roots only; test `…removed_root_never_disturbs…` |
| A4: paused job double-counted | `waiting_total` counts distinct `(work_id, fence)`; test |
| A5: applier clock | applying restricted to `may_run_cluster_jobs()` nodes (§6) |
| A6: line drift | every reference re-pinned to `54aebebf9` |
| Settled: node paths, v130 | `CASE` per custody kind in `background_storage_domain_holders`; v130 confirmed free |

## Execution log

| Date | Agent model | Agent session | Step | Result |
|---|---|---|---|---|
| 2026-11-13 | Codex + delegated lower-cost agents | storage-domain apply | Claimed | Schema, API and UI under implementation; review and qualification pending. |
