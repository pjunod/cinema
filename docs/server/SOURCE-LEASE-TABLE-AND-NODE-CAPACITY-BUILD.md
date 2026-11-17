# Source lease table and node capacity — build plan for steps 3 and 4

**Status:** built ([as built](SOURCE-LEASE-AS-BUILT.md) lists where the build differs) · **Executes:** §4.3 and §4.4 of
[SOURCE-LEASE-RELEASE-PLAN.md](SOURCE-LEASE-RELEASE-PLAN.md) · **Design:**
Claude (Fable 5.2), session `session_01BCWL9zn5U4msvhC85BdESw` · **Verified
against:** `origin/claude/source-lease-expiry-releases @ 7ad527dd1` (PR #1597),
`origin/claude/claim-local-verdict @ cc250dade` (PR #1625),
`origin/main @ cb9bba471` · **Written:** 2026-11-15

> **Who reads this.** The build agent. Work §9 one commit at a time and record
> each in the Execution log. Step 2 (proof machinery deleted) is assumed done;
> where it left a column or method this plan deletes, delete it here. Every
> line number below is from the refs above: re-verify before editing. If a
> step seems to need a proof, a witness, a per-symptom timer, or a second
> place that decides capacity, stop and flag it.

Companion to the release plan (the rule) and to
`CLAIM-LOCAL-VERDICT-BUILD.md` (PR #1625, not yet on this base) (A1,
which owns how `CLAIM_SQL` is assembled). This is *what to build*.

## 0. As built

The build differs from the text below in the ways listed, with reasons, in
[SOURCE-LEASE-AS-BUILT.md](SOURCE-LEASE-AS-BUILT.md); that list is
authoritative where they disagree.

## 1. Why — three tables and a view answer one question

"Who is reading source media, on which node, until when?" is answered today
by `foreground_source_owners`, `background_source_custody`, two reservation
tables and the `source_io_occupancy` union that reconciles them, with lapse
time derived per row by `custody_lapsed_at_sql`. Every capacity predicate
re-derives the answer, and the answer is one cluster-wide slot. Step 3 stores
the answer once, as a row with a real expiry. Step 4 counts those rows per
node against a setting Paul chose on 2026-11-15 (default 2).

## 2. Target model — one row per holder, expiry is a column

| Holder (`owner_kind`) | `lease_id` | Row written by | Renewed by | Ends |
|---|---|---|---|---|
| `playback` | owner UUID | `register_foreground` | `project_foreground` (5 s heartbeat) | `release_foreground`; else expiry |
| `job` | `job:<id>:<fence>` | trigger on the claim | trigger on job lease renewal | trigger on settle; else expiry |
| `analysis` | `analysis:<request>:<fence>` | trigger on the claim; coordination phase start | trigger on request renewal | trigger on success; else expiry |
| `lease` (read attestation) | `lease:<resource>:<fence>` | `register_background_custody` | trigger on `job_leases` | same trigger (close lowers expiry) |
| `offline` | `offline:<package>:<generation>` | `register_background_custody` | fixed at package expiry (as v1) | trigger on package state |

```
 lease row exists, phase != paused, expires_at_ms > now - 60 s ?
        │ no ──▶ holds nothing (the sweep deletes it; nothing waits on the sweep)
        │ yes
 owner_kind = playback ? ── yes ──▶ never counted; blocks background only
        │ no                        outside its permission window (as v1)
        ▼
 counts as one reader on its node_id, and on each mapped storage domain it names
```

Decisions, each with its reason:

1. **The grace is in the predicate, not the sweep.** A lease counts while
   `expires_at_ms > now - 60000`. Capacity frees on time even if no sweep ever
   runs; the sweep is garbage collection. `custody_lapsed_at_sql`, the
   `cleanup_pending` stamp and `background_source_current_owners` all go.
2. **Lease ids are computed in SQL.** Replicated statements cannot call
   `randomblob`, and a trigger cannot call `argv_fingerprint`. The id is the
   owner's identity, so a lost registration ack resolves by primary key.
3. **Job and analysis leases are written by triggers on the owner row.** The
   claim, the renewal and the settle already update `background_jobs` /
   `analysis_requests`; the lease follows in the same transaction, from any
   binary. No Rust path can forget to renew.
4. **`source_io` reservation rows stay as the job's bundle record.** Fifteen
   settle statements require "every required resource is reserved". They are
   no longer a capacity input and no longer capped at two slots (§3.3).
   Removing them is out of scope (§11).
5. **Playback is charged exactly as today.** It is never refused, never
   counted against the background cap, and its window blocks background
   cluster-wide on unmapped storage. Only the table it reads changes.

## 3. Schema and migration

New files `crates/plurx-core/src/store/source_leases.rs` and
`source_leases_expand.sql`, `source_leases_contract.sql`. SQLite appends two
migrations after the current head (v132 at the ref; use the next two numbers
on the real base). `background_jobs_foreground.sql` and
`background_job_turns.sql` are frozen history: add one header comment to each
("v1; superseded by source_leases_expand.sql") and change nothing else, since
their statement text is the v122/v124 catalogue.

### 3.1 The table (`source_leases_expand.sql`, first statements)

```sql
CREATE TABLE source_leases (
    lease_id TEXT PRIMARY KEY,
    owner_kind TEXT NOT NULL CHECK(owner_kind IN ('playback','job','analysis','lease','offline')),
    work_id TEXT NOT NULL,
    fence INTEGER NOT NULL CHECK(fence>0),
    node_id TEXT NOT NULL,
    owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
    resource_keys_json TEXT NOT NULL CHECK(json_valid(resource_keys_json)),
    phase TEXT NOT NULL CHECK(phase IN ('active','paused','startup','refill','sharing','transfer_service','source_idle','cleanup_pending')),
    projection_json TEXT CHECK(projection_json IS NULL OR json_valid(projection_json)),
    started_at_ms INTEGER NOT NULL,
    renewed_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL
) STRICT;
-- next statement
CREATE INDEX source_leases_node ON source_leases(node_id, expires_at_ms);
-- next statement
CREATE INDEX source_leases_work ON source_leases(owner_kind, work_id);
-- next statement
CREATE VIEW source_lease_permissions AS
SELECT l.lease_id,l.node_id,k.value AS resource_key,l.expires_at_ms,
  CASE WHEN json_extract(l.projection_json,'$.source_quiescent')=1 THEN 1 ELSE 0 END AS source_quiescent,
  CASE WHEN l.phase='source_idle' AND json_extract(l.projection_json,'$.source_quiescent')=1 THEN l.expires_at_ms
    WHEN l.phase='sharing' AND json_extract(l.projection_json,'$.source_quiescent')=1
      AND json_extract(l.projection_json,'$.contiguous_lead_ms')>json_extract(l.projection_json,'$.low_ms')
      THEN MIN(l.expires_at_ms,json_extract(l.projection_json,'$.observation_expires_at_ms'))
    WHEN l.phase='transfer_service' THEN MIN(l.expires_at_ms,json_extract(l.projection_json,'$.transfer_grant.expires_at_ms'))
    ELSE 0 END AS permits_through_ms
FROM source_leases l, json_each(l.resource_keys_json) k WHERE l.owner_kind='playback';
-- next statement
CREATE TABLE source_lease_schema(singleton INTEGER PRIMARY KEY CHECK(singleton=1),version INTEGER NOT NULL CHECK(version IN (2,3))) STRICT;
-- next statement
INSERT INTO source_lease_schema(singleton,version) VALUES(1,2);
```

`owner_json` is attribution for Activity (job kind, boot, claim; the playback
identity). It is never decoded back into `BackgroundSourceOwner`. `phase` for
background kinds is `active` or `paused`; the other values are the playback
phases, unchanged. `settlement_proof` and `activation_id` have no successor.

### 3.2 Owner triggers (same file)

```sql
CREATE TRIGGER source_lease_job_claim AFTER UPDATE ON background_jobs
WHEN NEW.state='running' AND NEW.fence>OLD.fence AND EXISTS(SELECT 1 FROM background_job_required_resources r
    WHERE r.job_id=NEW.id AND r.resource_key LIKE 'source_io%')
BEGIN
  INSERT INTO source_leases(lease_id,owner_kind,work_id,fence,node_id,owner_json,resource_keys_json,phase,started_at_ms,renewed_at_ms,expires_at_ms)
  SELECT 'job:'||NEW.id||':'||NEW.fence,'job',NEW.id,NEW.fence,NEW.owner_node_id,
    json_object('kind','job','job_kind',NEW.kind,'boot_id',NEW.owner_boot_id,'claim_id',NEW.claim_id),
    (SELECT json_group_array(resource_key) FROM (SELECT resource_key FROM background_job_required_resources
       WHERE job_id=NEW.id AND resource_key LIKE 'source_io%' ORDER BY resource_key)),
    'active',NEW.updated_at_ms,NEW.updated_at_ms,NEW.lease_expires_ms;
END;
-- next statement
CREATE TRIGGER source_lease_job_renew AFTER UPDATE OF lease_expires_ms ON background_jobs
WHEN NEW.state='running' AND NEW.fence=OLD.fence AND NEW.lease_expires_ms>OLD.lease_expires_ms
BEGIN
  UPDATE source_leases SET expires_at_ms=NEW.lease_expires_ms,renewed_at_ms=NEW.updated_at_ms
  WHERE lease_id='job:'||NEW.id||':'||NEW.fence;
END;
-- next statement
-- Cancelling: the reader is being stopped; charge it until release or the grace.
CREATE TRIGGER source_lease_job_cancelling AFTER UPDATE OF state ON background_jobs
WHEN OLD.state='running' AND NEW.state='cancelling'
BEGIN
  UPDATE source_leases SET expires_at_ms=MIN(expires_at_ms,NEW.updated_at_ms) WHERE lease_id='job:'||OLD.id||':'||OLD.fence;
END;
-- next statement
-- The owner settled while its job lease was live: its reader has returned.
-- An abandoned job (lease already expired) keeps its row until expiry + grace.
CREATE TRIGGER source_lease_job_settled AFTER UPDATE OF state ON background_jobs
WHEN OLD.state IN ('running','cancelling') AND NEW.state NOT IN ('running','cancelling')
  AND OLD.lease_expires_ms>NEW.updated_at_ms
BEGIN
  DELETE FROM source_leases WHERE lease_id='job:'||OLD.id||':'||OLD.fence;
END;
-- next statement
CREATE TRIGGER source_lease_attestation_renew AFTER UPDATE OF expires_at_ms ON job_leases
WHEN NEW.resource LIKE 'subtitle-read-attest:%'
BEGIN
  UPDATE source_leases SET expires_at_ms=NEW.expires_at_ms,renewed_at_ms=NEW.updated_at_ms
  WHERE lease_id='lease:'||NEW.resource||':'||NEW.fence;
END;
```

Write the analysis three the same way, copying the `WHEN` clauses of
`analysis_source_reserve_on_claim`, `analysis_source_renew` and
`analysis_source_release_on_success`
(`background_jobs_viewer_analysis.sql:319-356`): keys from
`analysis_required_resources`, node from `NEW.owner_node_id`, id
`'analysis:'||NEW.request_id||':'||NEW.fence`. A cancelled or retried request
keeps its lease to expiry, as its reservation does today. Write
`source_lease_offline_end` on `offline_packages`: when a `preparing` row
changes state, generation or node, or is deleted, delete
`'offline:'||OLD.id||':'||OLD.claim_generation` (verify the column names).

### 3.3 Dependents replaced in the same transaction

Each is `DROP` then `CREATE` under its existing name, text copied from the
file named and changed only as stated. Pin each with an "only this differs"
test like `custody_retention::only_the_custody_clause_differs…`.

| Object | From | Change |
|---|---|---|
| `background_job_claim_effects` | `background_jobs_resources.sql:40` | Reservation insert split in two: the existing statement gains `AND required.resource_key NOT LIKE 'source_io%'`; a second inserts each `source_io%` key at slot `(SELECT COALESCE(MAX(slot),1)+1 FROM background_job_reservations WHERE resource_key=required.resource_key)`. Not `rowid`: it is not stable across `VACUUM` |
| `background_source_pause_effects` / `_resume_effects` | `background_jobs_foreground.sql:39,57` | `UPDATE background_source_custody SET phase=…` becomes `UPDATE source_leases SET phase=…,renewed_at_ms=<now> WHERE lease_id='job:'||<job_id>||':'||<fence>`; resume takes its slot by the same `MAX(slot)+1` rule |
| `background_job_enqueue_command`, `background_job_maintenance_command` | `background_jobs_custody_retention.sql` | Delete the custody clause: back to the v85/v86 text. The clause existed for the census, which step 2 removed. Drop index `background_source_custody_owner_work` |
| `dispatch_turn_ordinary_occupancy` | `background_job_turns.sql:62` | One select: `source_leases h, json_each(h.resource_keys_json) k` where `h.owner_kind!='playback'`, counted (§4.1), and not live demand (the two existing `NOT EXISTS`, keyed on `h.owner_kind`/`h.work_id`); add column `node_id` |
| `dispatch_turn_contenders` | `background_job_turns.sql:74` | The bundle test (lines 92-110) becomes the three §4.1 terms with `a.now_ms` and `r.node_id`. The "lapsed custody stops a ready scan" branch is deleted |
| `dispatch_turn_independent` | `background_job_turns.sql:184` | `BEFORE INSERT ON source_leases`, keyed on `NEW.lease_id` |
| `background_storage_domain_holders` | `storage_domain_pending.sql:40` | Two selects: every `source_leases` row × key (`holder_kind=owner_kind`, `node_id` from the column, `custody_held=0`), and the existing `pause` select |

### 3.4 Rows carried over (end of the expand file)

| v1 rows | Carried as | Why |
|---|---|---|
| `foreground_source_owners` (all) | `playback` leases, same id, phase, projection, expiry | Live playback keeps its window; dead rows age out in ≤ 60 s |
| Running/cancelling jobs holding a `source_io%` reservation | `job` leases, expiry = `job.lease_expires_ms` | The holder is alive and renewing |
| `background_source_pauses` with `phase='paused'` | `job` leases, `phase='paused'` | So resume finds its row |
| Running `analysis_requests` with reservations | `analysis` leases | Same |
| `lease`/`offline` custody with `authority_expires_at_ms>0` | Leases, expiry = that authority | Their only record |
| Custody whose owner is not running; `settlement_proof`; `activation_id`; `cleanup_pending` | **Dropped** | An owner that is not running holds nothing: this is the rule. Carrying them is how capacity strands |

```sql
INSERT INTO source_leases(lease_id,owner_kind,work_id,fence,node_id,owner_json,resource_keys_json,phase,projection_json,started_at_ms,renewed_at_ms,expires_at_ms)
SELECT owner_id,'playback',json_extract(identity_json,'$.lifetime_id'),fence,json_extract(identity_json,'$.node_id'),
  identity_json,resource_keys_json,phase,projection_json,started_at_ms,updated_at_ms,lease_expires_at_ms FROM foreground_source_owners;
-- next statement
INSERT OR IGNORE INTO source_leases(lease_id,owner_kind,work_id,fence,node_id,owner_json,resource_keys_json,phase,started_at_ms,renewed_at_ms,expires_at_ms)
SELECT 'job:'||job.id||':'||job.fence,'job',job.id,job.fence,job.owner_node_id,
  json_object('kind','job','job_kind',job.kind,'boot_id',job.owner_boot_id,'claim_id',job.claim_id),
  (SELECT json_group_array(resource_key) FROM (SELECT held.resource_key FROM background_job_reservations held
     WHERE held.job_id=job.id AND held.fence=job.fence AND held.resource_key LIKE 'source_io%' ORDER BY held.resource_key)),
  'active',job.updated_at_ms,job.updated_at_ms,job.lease_expires_ms
FROM background_jobs job WHERE job.state IN ('running','cancelling') AND job.owner_node_id IS NOT NULL AND job.lease_expires_ms IS NOT NULL
  AND EXISTS(SELECT 1 FROM background_job_reservations held WHERE held.job_id=job.id AND held.fence=job.fence AND held.resource_key LIKE 'source_io%');
```

Write the paused, analysis and lease/offline inserts to the same pattern
(`INSERT OR IGNORE`, node from `paused.node_id`, `request.owner_node_id`,
`owner_json` `$.lease.owner_node_id` / `$.node_id`).

**Bridge for a rolling deploy.** The v1 tables stay in place after the expand,
so a node still on the old binary keeps serving playback. Three triggers on
`foreground_source_owners` (`source_lease_bridge_insert`/`_update`/`_delete`)
repeat the first insert above as `INSERT OR REPLACE`, the matching `UPDATE`
and `DELETE`, so an old node's playback still blocks new nodes' background
work. Old-binary jobs and analyses get leases from the §3.2 triggers, which
fire for every binary.

### 3.5 Contract (`source_leases_contract.sql`)

```sql
DROP TRIGGER source_lease_bridge_insert;  -- and _update, _delete
DROP VIEW source_io_occupancy;
DROP VIEW background_source_current_owners;
DROP VIEW foreground_source_permissions;
DROP VIEW foreground_source_required_resources;
DROP TABLE background_source_custody;
DROP TABLE foreground_source_owners;      -- takes foreground_source_owner_lease with it
DROP TABLE background_source_schema;
UPDATE source_lease_schema SET version=3 WHERE singleton=1;
```

One statement per `-- next statement`. SQLite runs it straight after the
expand. On the replicated store it runs only when every active node's running
binary has proved it no longer reads the v1 objects (§3.6).

### 3.6 Installers and shape oracles (`store/hiqlite.rs`, `store/sqlite/mod.rs`)

- Add `sqlite::catalogue_through(version) -> Vec<(name, sql)>`: an in-memory
  database migrated through that version. The four extensions the expand
  rewrites (source arbitration at `hiqlite.rs:4118`, `hiqlite/dispatch_turns.rs`,
  `custody_retention.rs`, storage-domain pending) accept their own names'
  shape from **any of** the pre-lease, expanded or contracted catalogue,
  and refuse anything else. Today each accepts exactly one.
- `source_leases::version(shape)` follows `dv_title_jobs_schema::version`:
  `None` (v1 only) · `Some(2)` expanded · `Some(3)` contracted · `Err` partial.
- New `install_source_leases_schema(admission)`, called **last** in both lists
  (`hiqlite.rs:2188` bootstrap and `:2394` migrate): `None` runs the expand
  through `schema_migration_transaction`, first statement the "raced" guard
  idiom of `install_source_arbitration_schema`. `Some(2)` attempts the
  contract, first statement
  `SELECT CASE WHEN <ready> THEN 1 ELSE json('source lease contract waits for every node') END`;
  a refusal is `Ok`, logged once at info. Bootstrap of a new cluster passes
  `fresh=true` and contracts unguarded.
- `<ready>` is `capability_ready_predicate("source_leases_v2")`
  (`cluster/membership.rs:3146`; export a `pub(crate)` wrapper). Publish the
  capability with the heartbeat exactly as `LIVE_TV_CAPABILITY` is
  (`membership.rs:6975`). This is the cluster layer's own "every running
  binary understands X" proof; nothing here asks nodes anything directly.
- The contract therefore lands on the first daemon start after every node has
  heartbeated on the new build, normally the next deploy. Until then the v1
  objects are inert. `hiqlite_import.rs:1866-1925` replaces the two v1
  `TablePlan`s with one for `source_leases`
  (`minimum_schema` = the expand's SQLite version).

## 4. Claim, renew, release, sweep

### 4.1 The capacity predicates (`store/source_leases.rs`, used everywhere)

```rust
pub const SOURCE_LEASE_GRACE_MS: i64 = 60_000;      // replaces SOURCE_AUTHORITY_REAP_GRACE_MS
/// `{l}` still holds capacity at `{now}`.
pub(crate) fn counted(l: &str, now: &str) -> String {
    format!("({l}.owner_kind!='playback' AND {l}.phase!='paused' AND {l}.expires_at_ms>{now}-60000)")
}
/// Readers per node, from the replicated setting (§5).
pub(crate) const NODE_CAP: &str = "(SELECT MAX(1,MIN(16,COALESCE((SELECT CAST(value AS INTEGER) FROM settings WHERE key='jobs.source_readers_per_node'),2))))";
```

For a claimant on node `N` requiring source key `K` at time `now`:

| Term (A1 fragment name) | Refuses when |
|---|---|
| `claim_foreground_window` | A `source_lease_permissions` row `fg` overlaps `K` (`fg.resource_key=K OR fg.resource_key='source_io' OR K='source_io'`, any node, as v1) and `(fg.expires_at_ms>now AND fg.permits_through_ms<=now) OR (fg.expires_at_ms<=now AND fg.expires_at_ms>now-60000 AND fg.source_quiescent=0)` |
| `claim_source_holders` | counted leases with `node_id=N` ≥ `NODE_CAP+1`; **or** `K!='source_io'` and counted leases naming `K` on any node ≥ 2 |
| `claim_maintenance_reader` | the claimant has no live demand **and** (counted, non-live leases with `node_id=N` ≥ `NODE_CAP`; **or** `K!='source_io'` and counted non-live leases naming `K` ≥ 1) |

So a node runs up to `NODE_CAP` maintenance readers plus one reserved for
live demand, and a mapped storage domain keeps today's cluster-wide numbers
(two readers, one maintenance) as the extra cap. "Live demand" is the existing
interest logic, unchanged: extract the block that appears twice in the
maintenance fragment into `macro_rules! live_job_interest { ($job:literal) }`
and use it for the claimant (`background_jobs.id`) and for a holder
(`held.owner_kind='job'` with `held.work_id`); an `analysis` holder is live on
the existing `playback-analysis` viewer test; `lease` and `offline` never are.
No self-exclusion: a job's current-fence lease does not exist before its
claim, and an older fence's lease is a real reader until it lapses.

### 4.2 Files and functions

| File · function | Change |
|---|---|
| `store/background_jobs.rs` · `CLAIM_SQL` | Replace the three source predicates with the §4.1 terms, written as `claim_foreground_window!`, `claim_source_holders!`, `claim_maintenance_reader!` taking `$job` (see §4.3). `ENQUEUE_SQL:472` loses its custody clause |
| `store/background_jobs_resources.rs` · `register_foreground`, `project_foreground`, `release_foreground`, `resolve_foreground`, `foreground_owners` | Same SQL against `source_leases` (`owner_kind='playback'`, `lease_id=owner_id`, `owner_json=identity`, `expires_at_ms`). `release` drops `settlement_proof IS NOT NULL`. `foreground_blocks` reads `source_lease_permissions` with the §4.1 window |
| same · `register_background_custody`, `register_background_typed` | `lease_id` from the new `background_owner_key` (the §2 strings). Job/analysis: return the trigger-written row's token, `LostOwnership` if absent. Lease/offline: insert, admitted by the maintenance term with `N` = the owner's node. Token = `{custody_id: lease_id, owner: <caller's>}`; replace every `Uuid::parse_str(&token.custody_id)` with one `lease_id_valid` |
| same · `release_background_custody`, `resolve_background_custody` | `DELETE` / `SELECT` by `lease_id`, no proof clause |
| same · `activate_*`, `abort_*`, `mark_*_settled`, `settled_*_custody`, `custody_lapsed_at_sql`, `SOURCE_ARBITRATION_SCHEMA` consumers of v1 names | Delete what step 2 left, with their `Store` trait methods (`background_jobs.rs:2793-2849`) and callers |
| same · `resume_background`, `background_may_continue`, `claim_background_service_inner`, `background_owner_resources` | Read `source_leases`; resume uses the holders term with `N` = the job's node. Byte-service arithmetic is unchanged |
| same · `reap_settled_source_custody` → `reap_expired_source_leases` | `DELETE FROM source_leases WHERE lease_id IN (SELECT lease_id FROM source_leases WHERE expires_at_ms<=$now-60000 OR (phase='paused' AND expires_at_ms<=$now) ORDER BY expires_at_ms,lease_id LIMIT 128)`, behind the existing read-first check; then `clear_orphaned_coordination_roles` keyed on `source_leases.lease_id`. Caller: `plurxd/src/state.rs:6816` |
| `store/fragment_index_cluster.rs:120-170` | `source_busy` and `background_reader_busy` become the holders and maintenance terms, `N` = the claiming node |
| `store/analysis_coordination.rs:289-327` | Phase start inserts the `analysis` lease; `active_custody_id` holds the lease id; release deletes it |
| `store/background_job_turns.rs:38,53`, `dv_title_jobs.rs:390,956`, `sharing_source_sessions.rs`, `storage_domain_pending` apply SQL | Occupancy reads move to `source_leases` with `counted` |
| `plurxd/src/source_custody.rs`, `background_jobs.rs`, `copy_analysis/physical.rs`, `subtitle_translation/read_attestation.rs`, `transcode/manager/{plan,produce}.rs` | Follow the trait changes. No renewal code is added: playback already heartbeats; the rest renew by trigger |

### 4.3 Interaction with PR #1625 (A1)

A1 rewrites the same `WHERE` clause into seven `macro_rules!` fragments and
builds `CLAIM_VERDICT_SQL` and `CANDIDATE_ADMISSION_SQL` from them. Step 3
changes only the **bodies** of the last three, keeping A1's names, `$job`
parameter and the `$1` fields (`node_id`, `now_ms`; both are already bound in
the statement, the verdict and the listing). So:

- If A1 has landed: edit the three bodies; touch nothing else in the file's
  claim section. `ClaimBlock` keeps its five variants; update the doc comments
  of `SourceHolders` ("the node or the storage domain is full") and
  `MaintenanceReader`.
- If it has not: write the three predicates as those macros now. The rebase
  conflict is then "take A1's assembly, keep step 3's bodies".
- A1's differential test (`crates/plurx-core/tests/claim_local_verdict.rs`)
  seeds v1 tables; reseed it through `source_leases`, and add `node_id` and
  the setting to its generator. Its property (verdict refuses exactly what the
  statement refuses) must hold unchanged.

## 5. Step 4 setting (`jobs.source_readers_per_node`)

| | |
|---|---|
| Key | `keys::JOB_SOURCE_READERS_PER_NODE = "jobs.source_readers_per_node"` in `store/mod.rs`, beside `MAX_HW_SESSIONS` |
| Meaning | Background (maintenance) source readers each node may run at once. One more is always reserved for live demand. Playback is not counted |
| Default · range | 2 · 1–16. Unset or unparsable reads as the default/floor in `NODE_CAP` |
| API | `source_readers_per_node` on the settings DTO and update request in `http/system.rs` (follow `transcode_max_hw_sessions`: `:2669`, `:3452`, `:4019`, `:4810`). Out-of-range input is a 400; nothing else refuses |
| Where read | Inside the claim statement, the analysis claim, resume and the dispatch-turn view: the replicated `settings` row, under the Raft writer. Every node decides from the same value at the same log position. No daemon cache, no restart |
| UI | `web/pages/settings-panels.js`, `librariesPanel`, above `storageDomainsCard()`: a number field "Background source readers per node" |
| Docs | `docs/API.md` settings table and the storage-domains rows (`:3781`); `docs/OPERATIONS.md` source section |

**Lowering it never refuses and never evicts.** The save commits; claims on a
node already above the new value wait until its readers finish. The save
response and the panel show that state from the Activity summary (§6):
"Saved — in effect" or "Saved — node-a is running 3 of 2; applies as they
finish". Raising it applies at the next claim.

### 5.1 Paul's decisions (2026-11-15) — these supersede §12 questions 2 and 3

- **Default plus per-node override.** `jobs.source_readers_per_node` is the
  cluster default (2, range 1–16). A node may carry an optional override
  (same range) in its own node settings; a node without one uses the default.
  The claim SQL reads the node's override when present, else the default, from
  the replicated store under the Raft writer (no daemon cache). The settings UI
  shows the default plus a per-node list (node name, effective value, override
  field with "use default").
- **Storage-domain cap becomes a number per domain.** Each storage domain gets
  `source_readers` (default 1, today's behaviour; range 1–16). The claim charges
  a domain's leases against that number cluster-wide, in addition to the per-node
  limit. Saving a lower value never refuses or evicts (same rule as above).

## 6. Activity and metrics — which node's leases hold what, and why

- `store/background_jobs_observation.rs` · `SOURCE_IO_HOLDERS_SQL` becomes one
  select over `source_leases` × key, joined to its job/request for kind and
  phase. `SourceIoHolder` gains `counts_against` (`node` · `domain` · `none`
  for playback and paused) and `live_demand`; `origin_node_id` is the
  `node_id` column; `active_occupancy` is `counted`.
- The summary (`http/analysis.rs:75`, `http/developer.rs:1578`) gains
  `source_capacity: {per_node, nodes: [{node_id, maintenance, live, playback, waiting_to_apply}]}`.
- `web/pages/activity.js:95`: group holders by node under a line
  "`<node>` — 2 of 2 background readers", each row naming the work, why it
  holds (maintenance · viewer · playback phase) and its lease age, with the
  existing cancel action as the way to stop it.
- Metrics (`http/system.rs:6472`): keep
  `plurx_background_source_recovery_pending` as "leases still present 180 s
  after expiry" (the sweep is not running); add
  `plurx_source_leases{node,kind}`, `plurx_source_readers_per_node` and
  `plurx_source_lease_schema_version`.

**How to read it:** a node at its cap with every row renewing is busy, not
stuck. A row whose lease age keeps growing past 60 s after expiry means the
sweep is not running; nothing else can hold capacity that long.

## 7. Pinned contracts, inventories and tests that must change

| Pin | Change |
|---|---|
| `background_jobs_resources.rs` `source_tests` (`:669-3245`), incl. `source_extension_shape_covers_…` (`:2568`), `lapse_expression_is_shared` (`:2318`) | Port to `source_leases`; delete the lapse-expression pin with the expression |
| `custody_retention.rs` tests | Extension shape is now "v85/v86 text restored"; rewrite the pin as equality |
| `hiqlite/dispatch_turns.rs` `expected_catalog`, storage-domain and source-arbitration shape checks | §3.6 three-catalogue rule |
| `hiqlite_import.rs` table plans + `hiqlite_import/tests.rs`; `hiqlite_durable.rs` digest | New table in, two out |
| `consistent_read_census.rs:577` (`source_arbitration_shape`) | Add the `source_leases` shape and contract reads as `STARTUP` |
| `crates/plurx-core/tests/store_contract.rs`, `support/{queue_fixture,source_extensions_fixture}.rs`, `subtitle_managed_schema.rs:350,707`, `sqlite/dv_conversion.rs:1688`, `store/mod.rs:520` | Fixtures build the migrated catalogue; the `playback_analysis_restore_effects` lookup still reads the frozen v1 text |
| `tests/operations/test_source_dispatch_turns.py` | Seed leases; add the node cap |
| `plurxd/src/http/tests.rs` settings round-trip; `tests/web/settings-sections.test.js:1091,2674`; `tests/web/activity-node-names.test.js` | New field, panel and grouping |
| `http/applied_settings_inventory.rs` | Expect no change (the read is in SQL, the DTO read is already pinned); if a count moves, classify it |
| `verification/{durable-jobs,operation-progress}/mapping.json`, `validation/points.toml`, `docs/server/README.md` | Repoint custody entries; add this doc to the index |

## 8. New tests — each fails on the old code

All in `crates/plurx-core` unless noted; SQLite and hiqlite where the store
contract runs both.

| Test | Asserts | Mutation check |
|---|---|---|
| `expand_carries_live_holders_and_drops_ownerless_custody` | From a v1 catalogue with one live job, one paused job, one live playback, one lapsed custody and one `joined` custody: three leases, none for the last two, and a claim on another node is admitted at once | Carry all custody rows: the claim is refused |
| `capacity_frees_without_a_sweep` | A lease 60 s past expiry does not refuse a claim although no sweep ran | Change `counted` to ignore expiry |
| `node_cap_is_per_node` | Cap 2: node A's third maintenance claim is refused while node B's first is admitted; a live-demand claim on A is admitted | Drop `node_id=N` from the term |
| `setting_changes_the_next_claim` | Set 1: second claim refused; set 3: admitted; lowering below current holders refuses nothing already running | Hard-code 2 |
| `playback_window_blocks_every_node_and_is_never_counted` | A `refill` playback on A refuses a background claim on B (unmapped); four playbacks on A leave A's background cap untouched | Scope the window to `N`; count playback |
| `job_lease_follows_the_job` | Claim writes it, renewal moves its expiry, owner settle deletes it, `cancelling` lapses it now, an abandoned job's lease remains until expiry + grace | Remove each trigger in turn |
| `domain_cap_still_binds` | Mapped domain: second maintenance reader on another node is refused | Drop the domain branch |
| `dispatch_turn_agrees_with_the_claim` | For generated holder sets, a turn is granted only where the claim statement admits | Leave the old view |
| `contract_waits_for_every_node` (hiqlite) | With one node lacking the capability the contract is refused and v1 objects remain; with all present it drops them and the marker reads 3 | Remove the guard |
| `old_owner_rows_bridge_until_contract` | An insert into `foreground_source_owners` after the expand blocks a claim through `source_leases` | Remove the bridge |
| `source_readers_setting_round_trips_and_rejects_only_bad_input` (`plurxd`) | GET default 2; PUT 3 with holders above it returns 200 and reports waiting; 0 and 17 are 400 | — |

## 9. Build order — small commits

1. `source_leases.rs` + expand SQL §3.1–3.2, SQLite migration, `version()`.
2. Dependents §3.3 with their "only this differs" pins.
3. Carry-over and bridge §3.4; test 1 and 10.
4. Predicates §4.1 and `CLAIM_SQL` (§4.3); A1's differential test reseeded.
5. Foreground and background store functions, sweep, trait cleanup §4.2.
6. Analysis, dispatch-turn Rust, title jobs, sharing, domain apply §4.2.
7. Contract SQL, capability, installers and oracles §3.5–3.6; import plans.
8. Daemon call sites; delete dead methods.
9. Setting, API, panel §5.
10. Activity and metrics §6; docs (`API.md`, `OPERATIONS.md`, the release
    plan's §4.3/§4.4 marked built, this log).

Commits 1–3 compile with the v1 code still in use; 4 is the cutover.

## 10. Acceptance

Local, each once on the final code:

```bash
cargo clippy --all-targets -- -D warnings                    # no dead v1 names left
cargo test -p plurx-core source_leases                       # §8 store tests
cargo test -p plurx-core --test claim_local_verdict          # A1 property still holds
cargo test -p plurxd source_readers_setting                  # setting round trip
node --test tests/web/settings-sections.test.js tests/web/activity-node-names.test.js
python3 -m pytest tests/operations/test_source_dispatch_turns.py
git grep -nE "background_source_custody|foreground_source_owners|source_io_occupancy" -- crates \
  ':!crates/plurx-core/src/store/background_jobs_foreground.sql' \
  ':!crates/plurx-core/src/store/background_job_turns.sql' \
  ':!crates/plurx-core/src/store/background_jobs_custody_retention.sql' \
  ':!crates/plurx-core/src/store/storage_domain_pending.sql' \
  ':!crates/plurx-core/src/store/source_leases*'             # must print nothing
```

Fleet (Paul watches Activity during and after his rolling deploy): queue a
library scan on every node. Activity shows up to two background readers **per
node**, grouped by node. Start a play: every node's background readers pause
through its startup and resume. `docker restart plurxd` on a node mid-scan:
its leases disappear from Activity within 90 s, other nodes never stop, and
`plurx_background_source_recovery_pending` stays 0. Set the value to 1 in
Settings → Libraries: the save succeeds at once and the panel shows which
nodes are still above it. After the next daemon start on any node,
`plurx_source_lease_schema_version` reads 3.

## 11. Non-goals

- **No rename of the `Store` trait methods or token types** beyond what is
  deleted. A rename touches every daemon call site and proves nothing.
- **`source_io` reservation rows are not removed** (decision 4).
- **No byte-service redesign.** `claim_background_service_inner` keeps its
  arithmetic.
- **No eviction on a lowered cap, no admin gate on the contract.** Both
  apply themselves when safe.
- ~~No per-node override and no configurable domain cap~~ — both built, per
  Paul's §5.1 decisions.

## 12. Risks and open questions

1. **Rolling deploy.** Between the first and last node upgrade, an old node
   does not see a new node's playback (its own v1 view has no bridge back),
   so it may start background reads during that playback's startup: a
   throughput cost for the length of the roll. A new node's sweep may retire
   an old node's in-flight coordinated analysis phase; the request retries.
   An old binary that restarts mid-roll refuses the changed catalogue until
   it is upgraded. The alternative, one cutover that drops the v1 tables at
   the first upgrade, would stop playback on every node still on the old
   binary; rejected.
2. **Unmapped shared storage — decided (§5.1):** With no storage domains, four
   nodes at the default run up to eight maintenance readers. If those nodes
   read one NAS, that is eight readers on it. Mapping the NAS as a storage
   domain restores today's cluster-wide cap of one maintenance reader for it,
   which is the serialising behaviour step 4 set out to remove. **Question:**
   leave the domain cap fixed, or make it a per-domain number too?
3. **One value for every node — decided (§5.1):** The setting is a single number
   applied to each node. A slower node cannot be set lower without a
   per-node override. **Question:** is one value enough?
4. **Trigger order.** `source_lease_job_claim` and
   `background_job_claim_effects` both fire on the claim and must not depend
   on each other; test 6 runs with `recursive_triggers` on and off, as
   `dv_title_jobs_schema` tests do.
5. **Offline leases** keep v1's long expiry (the package's); a crashed
   preparer holds a node slot until its package is re-claimed or expires.
   Unchanged from v1, now visible in Activity.

## Execution log

| Date | Milestone | Agent-Model | Agent-Session | Result |
|---|---|---|---|---|
| 2026-11-15 | Decisions §5.1 recorded (Paul) | claude-opus-5-5 | session_01BCWL9zn5U4msvhC85BdESw | per-node override; per-domain reader number |
| 2026-11-15 | Design | claude-fable-5-2 | session_01BCWL9zn5U4msvhC85BdESw | this document; read-only against the refs in the header |
| 2026-11-15 | Steps 3 and 4 built | claude-opus-5-5 | session_01BCWL9zn5U4msvhC85BdESw | branch claude/source-lease-steps-2-4; §0 lists the differences |
