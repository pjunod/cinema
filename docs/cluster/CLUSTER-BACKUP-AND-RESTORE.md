# Cluster backup and restore — one consistent cut, restored somewhere else first

**Status:** in progress · **Executes:** §2.3, S4, F-sc-4,
F-build-ops-codehealth-2 (the brief's "F-build-2") from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Read [OPERATIONS.md](../OPERATIONS.md) "Rolling back a deploy" first (it is
the gap this closes), then §2 here for what the tree already gives you, then
execute §5 as logical milestones in the one whole-plan draft PR required by
the work board. The deliverable is
the procedure, not the mechanism: a backup nobody has restored is a file. If a
step seems to require copying Raft log or snapshot directories between nodes,
inventing a force-new-cluster command around them, or restoring straight onto
the production data directory, stop and flag it — every one of those is a
listed non-goal (§4).

**Correction to the review:** four things the review states about the
vendored `backup` feature are not what the tree does.

1. `backup` cannot be enabled without `s3` today. The feature graph is
   `backup = ["dep:cron", "s3", "sqlite"]` and `s3 = ["backup"]`
   ([Cargo.toml:48-50, 97](../../vendor/hiqlite/Cargo.toml)), and
   [backup.rs:298-304, 319](../../vendor/hiqlite/src/backup.rs) reads
   `node_config.s3_config`, a field that only exists under
   `#[cfg(feature = "s3")]` ([config.rs:135-136](../../vendor/hiqlite/src/config.rs)).
   Deleting the `cryptr features = ["s3"]` edge at line 190-192 is necessary
   but not sufficient; three `cfg` gates in `backup.rs` and the `backup`
   feature list itself must change too (§5.1).
2. `QueryWrite::Backup` is `#[cfg(feature = "backup")]` *inside the
   replicated enum*
   ([state_machine.rs:202-211](../../vendor/hiqlite/src/store/state_machine/sqlite/state_machine.rs)).
   Turning the feature on moves `RTT` from discriminant 5 to 6 in every
   serialized log entry. A rolling deploy with the feature half-enabled would
   mis-decode `RTT` entries — the entry `trigger_db_snapshot` and the vendored
   restore path both write. The variant must be un-gated before any binary
   with the feature reaches a voter.
3. `Client::backup()` is a *replicated* entry
   ([client/backup.rs:36-56](../../vendor/hiqlite/src/client/backup.rs)):
   every voter's writer thread runs an in-place `VACUUM` and then
   `VACUUM main INTO` ([writer.rs:621-687](../../vendor/hiqlite/src/store/state_machine/sqlite/writer.rs)),
   with apply queued behind it on every node at once. That is S2's stall,
   cluster-wide, twice. The review's "enable the vendored `backup` feature as
   the mechanism" is therefore narrowed here: the artefact is built from one
   node's Raft snapshot (§3.1), which the writer already produces at one
   logical cut, and the vendored feature is not required by any milestone.
4. The tree already has the portable-image builder the review asks for:
   `snapshot_hiqlite_state_machine`
   ([migration.rs:1342-1368](../../crates/plurx-core/src/cluster/migration.rs))
   copies the state machine with the rusqlite `Backup` API, deletes
   `_metadata`, checkpoints and canonicalizes the journal, and
   `verify_state_machine_snapshot_identity` (`:1371-1402`) checks
   `settings.instance.id`. The single-voter readdress path builds a one-voter
   Raft around exactly such an image. Restore reuses that path rather than
   writing a second one.

## 1. Objective

An operator with three dead disks, one surviving disk, or a data directory
that a migration hotfix has just wedged can get the household back: users,
tokens, API keys, settings, libraries and files, watch and reading state, DVR
schedules and recordings metadata, library channels, Trakt links, and the
credential key that unseals them — from an artefact that lives on a different
machine, restored first onto an isolated instance, then promoted, with the
old cluster unable to come back by accident. Measured, not asserted: RPO and
RTO recorded from a drill on realistic data.

## 2. Contract today

Re-verify every line and path below at build time.

### 2.1 What a Raft snapshot is, and why it is one logical cut

The state-machine writer is a single thread consuming a `flume::bounded(1)`
channel ([writer.rs:143](../../vendor/hiqlite/src/store/state_machine/sqlite/writer.rs)).
Apply sends one request per entry and awaits it
([state_machine.rs:1131-1146, 1256-1270](../../vendor/hiqlite/src/store/state_machine/sqlite/state_machine.rs)).
A snapshot request runs on that same thread:

```rust
// vendor/hiqlite/src/store/state_machine/sqlite/writer.rs:503-536
WriterRequest::Snapshot(SnapshotRequest { snapshot_id, path, ack }) => {
    sm_data.last_snapshot_id = Some(snapshot_id.to_string());
    persist_metadata(&conn, &sm_data).expect("Metadata persist to never fail");
    match create_snapshot(&conn, path) {            // VACUUM main INTO '{path}'
        Ok(_) => { /* PRAGMA optimize */ ack.send(Ok(SnapshotResponse { meta: sm_data.clone() })) }
        Err(err) => ack.send(Err(StorageError::IO { .. })),
    }
}
```

`sm_data` holds `last_applied_log_id` and `last_membership`; `persist_metadata`
writes them into the `_metadata` table *inside* the database immediately
before the copy, and nothing applies while the copy runs. The image, the
`_metadata` row inside it, and the `SnapshotMeta` the builder returns
([snapshot_builder.rs:101-120](../../vendor/hiqlite/src/store/state_machine/sqlite/snapshot_builder.rs))
therefore agree by construction. That is the property assessment correction
4 requires, and it is why the backup artefact is derived from a snapshot and
not from a read-pool `VACUUM INTO` (which would be an off-writer cut, S2's
open design question — see
[RAFT-SNAPSHOT-CADENCE-AND-CONSISTENT-CUT.md](RAFT-SNAPSHOT-CADENCE-AND-CONSISTENT-CUT.md)).

The published file is `<data_dir>/hiqlite/state_machine/snapshots/<uuid>`,
named by the `current` pointer file
([snapshot_builder.rs:20-24](../../vendor/hiqlite/src/store/state_machine/sqlite/snapshot_builder.rs)).
A local trigger already exists:

```rust
// vendor/hiqlite/src/client/mgmt.rs:160-173 (plurx patch)
pub async fn trigger_db_snapshot(&self) -> Result<u64, Error> {
    let state = self.inner.state.as_ref().ok_or_else(|| Error::Connect(
        "database snapshot trigger requires a local node client".to_owned()))?;
    let applied = state.raft_db.raft.client_write(QueryWrite::RTT).await?.log_id.index;
    state.raft_db.raft.trigger().snapshot().await?;
    Ok(applied)
}
```

It writes one `RTT` entry (so the cut is at or after `applied`) and builds
locally. `SelectedStore.local_client` ([migration.rs:230](../../crates/plurx-core/src/cluster/migration.rs))
is that local client.

### 2.2 Identity and key material on disk

| File (under `storage.data_dir`) | Owner | Restore treatment |
|---|---|---|
| `hiqlite/state_machine/db/plurx.db` | replicated state machine | restored from the image |
| `hiqlite/activation.json` | `ActivationMarker` (`migration.rs:162-179`): `cluster_id`, `replicated_schema_version`, `admitted_role`, table digests | rewritten from the manifest |
| `hiqlite-activated.json` | breadcrumb (`migration.rs:194-207`) | rewritten |
| `node.id` | node identity (`cluster.rs:100-140`) | **new** — the restored voter is a new node |
| `membership.json` | `LocalMembership` (`membership.rs:1318-1330`) | **new** single-voter record |
| `secret_raft`, `secret_api`, `activity_http_signing_key` | hiqlite auth and peer signing (`migration.rs:74-78`) | **new** — this is the fence (§3.4) |
| `credentials.key` | `secrets::CREDENTIAL_KEY_FILENAME` (`secrets.rs:38`); unseals Trakt rows | restored from the archive, or supplied |
| `hiqlite/telemetry.db` | node-local playback telemetry | not restored |
| `hiqlite/logs/`, `hiqlite/state_machine/snapshots/` | Raft log and snapshots | **never copied**; rebuilt around the image |
| `plurx.db`, `migration/` | frozen pre-activation source | left alone; irrelevant after restore |

`cluster_id` is `settings.instance.id` inside the database
(`ClusterIdentity`, [cluster.rs:33-44](../../crates/plurx-core/src/cluster.rs));
clients key saved servers on `instance_id` from `/api/v1/server`, and
`shared_cache_id` is combined with it. It is **kept** on restore. Fencing is
done with the secrets, not by minting a new instance id (§3.4).

The credential key is opened through a census of sealed rows
([cluster.rs:97-121](../../crates/plurx-core/src/cluster.rs)): a database with
sealed Trakt rows and a key file whose id does not match refuses to start
and names the key it wants. Restore inherits that refusal for free.

### 2.3 What is derived, node-local, or dead after a restore

| State | Where | After restore |
|---|---|---|
| `items_fts`, `classification_fts` | inside the image; derived (`hiqlite_catalog.rs:1-5`) | ride along; `rebuild_search_index` if the integrity check flags them |
| `transcode_cache_recipes/locations`, `offline_packages`, fragment-index heads | replicated rows naming `node_id`s | rows for node ids not in the new cluster are tombstoned by the existing node-removal reconciliation; bytes are gone with the nodes |
| `media_sessions*`, `job_leases`, `cluster_operation_leases`, `cluster_node_*` | replicated wall-clock coordination | truncated at restore — they describe a dead cluster's leases |
| `cluster_nodes` | membership projection | every old row tombstoned (`removed_at`); the new voter heartbeats itself in |
| DVR recording files, cache/offline bytes, subtitle cache | node-local under `cache_dir`, `dvr.root` | not in the artefact; recording rows keep their paths and are remapped with media roots |
| library `root_fingerprint` | replicated | reset for every library whose paths were remapped (operator-verified replacement, `store/mod.rs:2874-2876`) |

### 2.4 Media roots

`libraries.paths` is a JSON array of absolute roots; `files.path` is absolute
and `UNIQUE` (`sqlite/mod.rs:112-131`); DVR recording rows carry absolute
paths under `dvr.root`. A host that mounts the same NAS at a different prefix
needs all three rewritten before the first scan, or the vanished-file pass
sees an empty root — which the fingerprint guard refuses, but only after the
operator has a confusing failure instead of a restore.

### 2.5 Leader-singleton jobs and metrics

`acquire_cluster_job` ([job_lease.rs:261-290](../../crates/plurxd/src/job_lease.rs))
gates on `may_run_cluster_jobs` (a learner never wins) and takes a 90 s lease
renewed every 30 s. Every cluster-wide resource is enumerated in
`CLUSTER_SINGLETON_RESOURCES` ([state.rs:10282-10288](../../crates/plurxd/src/state.rs));
adding one without listing it fails that test. `/metrics` already renders
`plurx_raft_snapshot_seconds{operation,outcome}` and
`plurx_store_operation_seconds{class,outcome}`
([system.rs:4761-4782](../../crates/plurxd/src/http/system.rs)).

## 3. Change

### 3.1 The artefact

A directory, not a single file, so the key can carry its own mode:

```text
plurx-backup-<UTC yyyymmddThhmmssZ>-<cluster_id[..12]>/     mode 0700
├── manifest.json        schema_version=1, cluster_id, node_id that built it,
│                        built_at_unix_ms, binary version::LONG,
│                        replicated_schema_version (from activation.json),
│                        raft_cut { term, index, membership voters/learners },
│                        image_sha256, image_bytes, integrity_check="ok",
│                        table_counts {users, tokens, libraries, items, files,
│                          watch_state, dvr_schedules, library_channels},
│                        credential_key_id (or null)
├── plurx.db             portable image: `_metadata` deleted, journal DELETE,
│                        checkpointed — byte-identical treatment to
│                        snapshot_hiqlite_state_machine
├── credentials.key      mode 0600, copied verbatim (absent when the node has
│                        no key, which the manifest says)
└── SHA256SUMS
```

Build sequence, run by the node holding the `backup:cluster` job lease:

1. Refuse unless `may_run_cluster_jobs()` and this node's
   `apply_lag_entries == 0` from the passive Raft metrics (a lagging image
   is a worse RPO than waiting one tick).
2. `applied = local_client.trigger_db_snapshot().await?`.
3. Poll `metrics_db().snapshot` until `last_log_id.index >= applied`
   (bounded by `snapshot_transfer_timeout` + `SNAPSHOT_CATCHUP_GRACE`).
4. Read `snapshots/current`, **open the named file immediately** and copy
   from the open descriptor — `snapshots_cleanup` may unlink a superseded
   snapshot while the copy runs; an open fd survives that on Linux.
5. In the staging copy (`<data_dir>/backups/staging/`): read `_metadata`,
   decode `StateMachineData`, require `last_applied_log_id.index >= applied`,
   record term/index/membership into the manifest, then `DELETE FROM
   _metadata`, `PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE`,
   `PRAGMA integrity_check` (must be exactly `ok`), `fsync`.
6. Copy `credentials.key`, write `manifest.json` and `SHA256SUMS`, `fsync`
   the directory, then `rename` into the destination (same filesystem) or
   stream to it and verify the sums after the write when it is a mount.
7. Set `plurx_backup_last_success_seconds` and prune to `backup.keep`.

The snapshot this triggers is a real snapshot: it resets the node's
`logs_until_snapshot` clock and purges log up to it. Once a night on one node
that is the intended cost; it is why the job is a singleton and why S2's
cadence work measures with this job enabled.

### 3.2 Settings and readiness (no in-code gate)

Three replicated settings under Settings → Developer, all advisory-listed,
none blocking anything else:

| Key | Meaning | Default |
|---|---|---|
| `backup.destination` | absolute directory the artefact is written to | empty = the job does not run |
| `backup.schedule_utc` | `HH:MM` once a day, UTC | `02:30` |
| `backup.keep` | artefacts retained at the destination | `14` |

Readiness list entries (advisory): destination exists and is writable by the
daemon; destination `statvfs` fsid differs from `data_dir`'s (off-node or at
least off-disk — a copy beside the same three disks does not cover site
loss); free space ≥ 2 × current image size; last success age < 26 h.

### 3.3 CLI

- `plurxd cluster backup --server … --token-file … [--output DIR]` — asks the
  running voter to build one artefact now, through the same job lease, and
  prints the manifest. Admin route `POST /api/v1/cluster/backups`.
- `plurxd restore --archive DIR --data-dir NEW_DIR [--remap OLD=NEW]...
  [--credential-key FILE] [--advertise-host H]` — **offline**, refuses if the
  daemon lock is held or `NEW_DIR/hiqlite` exists. Verifies sums and
  `integrity_check`, applies remaps, then builds the one-voter directory
  exactly as the readdress path does: image → `state_machine/db/plurx.db`,
  new `node.id`, new `membership.json` (voter, bootstrap = itself), new
  `secret_raft`/`secret_api`/`activity_http_signing_key`, `activation.json`
  from the manifest, `hiqlite-activated.json`. Prints what it minted and what
  the operator must now retire.
- `plurxd restore --verify --archive DIR` — sums + integrity + key/census
  agreement, no writes.

### 3.4 Fencing the old cluster

The new voter has new `secret_raft`/`secret_api`, so no surviving old voter
can replicate with it or be admitted by it, and every old `membership.json`
names peers that no longer answer with the same secret. That is the
mechanical fence. The procedural fence is written into the runbook and
enforced by the restore command's output: before clients are pointed at the
restored instance, every surviving old node's `hiqlite/` is renamed to
`hiqlite.retired-<UTC>` with the daemon stopped, and rejoins through a fresh
join token like any new node. `restore` additionally binds one parameter,
`settings.cluster.restore_generation = <built_at_unix_ms>`, into the image
before activation so the Cluster page can say "restored from
2026-09-20T02:30Z artefact" and a support bundle can prove which lineage a
node is on.

### 3.5 Remapping

`--remap /srv/media=/mnt/nas/media` rewrites, in one transaction on the
staged image, `files.path` (prefix match on a path boundary), each element
of `libraries.paths`, and DVR recording paths, then resets
`root_fingerprint` for every library touched. A remap that would make two
`files.path` values collide aborts with both paths named. The command
prints counts per table.

### 3.6 Order of restore, always

Isolated instance first — a lab node or the container drill with a fresh
`data_dir` and a loopback bind — log in with a restored admin token, open
Home, open a title's detail page, check DVR schedules and Trakt link status,
run `plurxd restore --verify`. Only then the production sequence: stop the
survivor(s), retire their directories, restore on the designated first
voter with the production `advertise_host`, start, `curl /readyz`, rejoin
the others with new join tokens, re-run scans so vanished-file detection
sees the remapped roots.

## 4. Guardrails (non-goals)

- **No copying of `hiqlite/logs/` or `snapshots/` between nodes, and no
  force-new-cluster around copied Raft files** (§2.3 of the review;
  F-sc-4 "not an accepted product restore procedure by itself"). The
  vendored `HQL_BACKUP_RESTORE` path wipes the data directory of every node
  whose id is not 1 (`backup.rs:277-284`) and assumes node 1 becomes leader;
  it is not used.
- **`instance.id` is not changed.** Clients and shared-cache identity depend
  on it. Fencing is by secrets and retirement (§3.4).
- **The artefact is never built off-writer** from a read-pool connection.
  Until S2's single-cut design ships, the only consistent cut is the writer's
  own snapshot (§2.1).
- **The vendored `backup` feature is not enabled in `Cargo.toml:38`** until
  §5.1's enum un-gating is merged and the fleet is on it; `Client::backup()`
  is not called by any milestone (replicated in-place `VACUUM`).
- **No restore onto a live data directory.** `restore` refuses a held daemon
  lock and an existing `hiqlite/`.
- **The credential key is never written into the SQLite image** and never
  travels without its 0600 mode; the destination must be as protected as
  `data_dir` (readiness says so).
- **Nothing here changes recipe identity or cache digests.** Cache rows for
  vanished nodes are tombstoned through the existing removal reconciliation,
  not rewritten.
- **A Raft snapshot is not a backup** (not portable across cluster
  identities); the artefact strips `_metadata` precisely so it is.

How each assessment disposition is honoured: schema/version check →
manifest `replicated_schema_version` + the existing newer-schema refusal;
encrypted data/key handling → `credentials.key` copied with mode and matched
by the sealed-row census at start; cluster identity reset/fencing → §3.4;
off-node retention → readiness fsid check + `backup.keep`; recovery of
users/settings/watch state → drill acceptance in §5.5 reads them back.

## 5. Milestones

### 5.1 M1 — fork hygiene: `backup` without `s3`, stable wire format

In `vendor/hiqlite/Cargo.toml`: `backup = ["dep:cron", "sqlite"]`,
`s3 = ["backup", "cryptr/s3"]`, `[dependencies.cryptr] default-features =
false` with no `features`. In `backup.rs`: gate the three `s3_config` uses
and `BackupSource::S3` behind `#[cfg(feature = "s3")]`. In
`state_machine.rs:208-209`: remove the `cfg` from the `Backup` variant (keep
the `cfg` on the *handler* at `:1201`, which returns an error when the
feature is off). Record all three in `PLURX-PATCH.md`. Plurx's own feature
list stays `["auto-heal", "macros", "sqlite"]`.

Acceptance: `cargo tree -d -p plurxd | grep -E 'aws-lc-sys|reqwest|s3-simple'`
shows one `reqwest`, no `aws-lc-sys` duplicate and no `s3-simple`;
`cargo check -p hiqlite --no-default-features --features sqlite,backup` and
`--features sqlite,backup,s3` both pass; `make hiqlite-vendor-clippy` green;
a new vendored unit test serializes `QueryWrite::RTT` with and without
`--features backup` and asserts identical bytes.

### 5.2 M2 — the artefact builder and `plurxd cluster backup`

`crates/plurxd/src/backup.rs` (new): the §3.1 sequence behind
`acquire_cluster_job("backup:cluster")`; add the resource to
`CLUSTER_SINGLETON_RESOURCES`; admin route + CLI; `plurx_backup_last_success_seconds`
(gauge, no labels), `plurx_backup_runs_total{outcome="ok"|"error"|"skipped"}`,
`plurx_backup_artifact_bytes`. Schedule loop reads `backup.schedule_utc`
and `backup.destination` with `get_setting_pair` once per minute.

Acceptance: `cargo test -p plurxd backup::` covers: manifest agrees with the
image's `_metadata` before deletion; a superseded snapshot unlinked mid-copy
still yields a verified artefact (test unlinks after open); `integrity_check`
≠ `ok` fails the run and leaves no artefact at the destination; two nodes
ticking simultaneously produce one artefact (lease); a learner never builds.
`make cluster-store-check` unchanged.

### 5.3 M3 — `plurxd restore` and `--remap`

Extract the one-voter directory build from `readdress_single_voter_if_needed`
into a shared helper both call; implement §3.3/§3.5. `restore --verify`
runs the sealed-row census against the archive's key.

Acceptance: `cargo test -p plurx-core cluster::migration::restore_` : a
restored directory starts as a sole voter, `instance.id` unchanged,
`cluster_nodes` holds only the new node, all lease/session tables empty,
`restore_generation` set; a remap test moves `/srv/media` → `/mnt/nas/media`
and asserts `files.path`, `libraries.paths`, DVR paths and fingerprint
reset; a wrong-key archive refuses with the census message; an archive whose
`replicated_schema_version` is newer than the binary refuses with the
existing newer-schema text.

### 5.4 M4 — container-smoke drill

Extend [`scripts/container-smoke`](../../scripts/container-smoke): after the
first readiness probe, create a user and a library through the API, run
`plurxd cluster backup --output /var/lib/plurx/backups`, start a **second**
container with an empty volume, run `plurxd restore --archive … --data-dir
/var/lib/plurx`, start it, and assert the same `instance_id`, the user logs
in, and the library lists.

Acceptance: `make container-smoke` green locally and in the `container`
scope lane; the script's runtime increase is recorded in the PR body.

### 5.5 M5 — fleet drills and RPO/RTO (GPT)

Two drills on the lab (lab1–lab3 as voters, `nas` as destination):

- **One-node loss.** Power off lab2. Confirm the nightly artefact still
  builds from a surviving voter (`plurx_backup_last_success_seconds`
  advances) and that lab2 comes back by rejoining, not by restore.
- **Majority loss.** Power off lab1 and lab2. Restore the latest artefact
  onto lab4 as an isolated instance, verify per §3.6, then perform the
  production sequence with lab3 as the retired survivor rejoining.

Record `RPO = artefact age at the moment of loss` and `RTO = wall time from
"restore begins" to `/readyz` 200 plus rejoin complete`, with DB size, in
`benchmarks/evidence/backup-restore-<sha>.json`.

Acceptance: the evidence file exists with both drills and both numbers;
OPERATIONS.md's "no automated … backup" paragraph is replaced by the
runbook (docs PR, same milestone).

```text
GPT prompt (fleet, needs SSH to lab1–lab4 and nas):
On the lab cluster running <sha>, set backup.destination to
/mnt/nas/plurx-backups, backup.schedule_utc to now+5 min, wait for
plurx_backup_last_success_seconds to advance on exactly one voter, and copy
the manifest.json here. Then: (1) stop plurxd on lab2, wait for the next
artefact, confirm it built on a surviving voter, start lab2 and confirm it
rejoins without any restore step; (2) stop lab1 and lab2, on lab4 with an
empty /srv/plurx run `plurxd restore --archive <latest> --data-dir
/srv/plurx --advertise-host 10.42.0.14`, start it, log in with a token that
existed before the loss, open Home and one title, report DVR schedule count
and Trakt link status; then stop lab3, rename its hiqlite/ to
hiqlite.retired-<UTC>, issue a join token on lab4 and rejoin lab3. Report:
DB size, artefact bytes, minutes from restore start to /readyz 200, minutes
to lab3 voter again, and anything the restore command told you to do that
you did not expect.
```

## 6. Verification and rollout

Fast lane per PR: `make unit`; M1 adds `make hiqlite-vendor-clippy`; M2/M3
add `cargo test -p plurxd backup::` and `cargo test -p plurx-core
cluster::migration::restore_`; M4 adds `make container-smoke`. Node/Python
gate: `tests/operations/test_docs_index.py` for the README row and
`test_contracts.py` if `scripts/container-smoke` is asserted there.

Rollout: M1 first and alone (a vendored wire-format-neutral patch, deployed
to every voter before M2 ships — the un-gated variant makes a later feature
flip safe). M2 lands with `backup.destination` empty everywhere, then the
setting is filled on the lab, then media1. M3 has no runtime effect until
invoked. Rollback of any milestone is a redeploy of the previous image; no
schema changes are introduced (`restore_generation` is a settings row).

## 7. Open questions

1. Should `credentials.key` travel in the archive at all, or should the
   archive carry only its id and the operator restore the key from their
   own secret store? The former makes the drill self-contained; the latter
   makes a stolen destination less valuable. Default here: in the archive,
   0600, with the readiness item saying the destination is a secret.
2. `backup.keep` counts artefacts; is a time-based retention (days) wanted
   too? The vendored cleanup is day-based.
3. Whether `plurxd cluster backup` should also be reachable without an admin
   token from the local socket for cron use on hosts without Settings
   access — not planned; the schedule setting covers the fleet case.
4. F-hist-style question for §5.5: the majority-loss drill retires lab3's
   directory even though it held committed writes newer than the artefact.
   A "merge the survivor's newer writes" path is deliberately not proposed;
   confirm that is acceptable, since the alternative is a per-table conflict
   design.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M1 | [PR #426](http://192.168.4.7:3000/noirr/plurx/pulls/426) | Wire-stable `QueryWrite::RTT`; `backup` builds without S3 and `backup,s3` remains supported. Focused feature-matrix checks are recorded in the PR. |
