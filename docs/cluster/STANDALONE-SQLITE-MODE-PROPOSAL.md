# Standalone SQLite — local storage without Raft or quorum authority

**Status:** built — runtime and safe conversion implemented; focused source checks passed.
Release qualification and affected-platform acceptance remain open.
**Written:** 2026-11-03 · **Source baseline:** `71a776b3aceabc5905805d7196de4bd6647fac7f`.
**Owner:** Codex implementation, authorized by the repository owner on 2026-11-03.
**Integration branch:** `effort/standalone-sqlite`; task `codex/standalone-sqlite-runtime`.

Companion to [ARCHITECTURE.md](../ARCHITECTURE.md) §2 and
[REQ-HA-1](../REQUIREMENTS.md#7-high-availability--the-cluster-contract), which
now specify native standalone storage. This document retains the earlier
decision's history and records the approved replacement and its implementation
contract. The owner approved implementation after the investigation. The [clustering plan](CLUSTERING-PLAN.md) owns the
existing replicated path; the [backup plan](CLUSTER-BACKUP-AND-RESTORE.md) owns
existing cluster recovery.

The earlier experimental timeout patch `d833a8d36` remains separately preserved
on `codex/standalone-serving-authority`. It is excluded from this proposal's
base, has not been deployed, and is not a prerequisite for this design.

## 1. Finding — unconditional Raft was a conscious maintenance tradeoff

This was not required by SQLite, by local playback, or by the Store interface.
The documented reasons were one storage execution path, avoiding a separate
"cluster edition", and allowing a single server to gain voters later.

| Decision or change | Historical evidence | Consequence |
|---|---|---|
| Founding architecture, 2026-07-19 | `431eca5a9` already specified the one-voter path, while its original REQ-HA-1 promised dormant clustering with zero overhead/configuration. | The initial documents contradicted one another; the initial runtime still used native SQLite. |
| Choose Hiqlite, 2026-07-19 | `df442e914`; [Phase 3 decision](PHASE3-SPIKE.md#decision-summary). Its SQL API fitted the existing row mappers; it embedded without an external database and supplied cluster/self-heal tests. | A reasonable clustered backend choice also inherited the planned one-voter policy. |
| Resolve the contradiction in favor of Raft, 2026-08-06 | `bb3f6a1c7`; [review verdict](CLUSTERING-PLAN-REVIEW.md#1-verdict--keep-the-sequencing-rewrite-3-and-4-decompose-m1) and its §S2. The review explicitly favored avoiding two storage paths because standalone might receive less exercise. REQ-HA-1 was amended. | "Dormant clustering" became a measured-overhead promise instead of an absence-of-consensus promise. |
| Make activation unconditional, 2026-08-15 landing | `9ff8a4d9a`, `feat(cluster): activate one-voter Hiqlite`. Enabled the daemon's `hiqlite-store` dependency and first-boot import/activation without a cluster-enabled condition. | Ordinary standalone installs acquired a one-member Raft database. Interrupted activation retained only a temporary SQLite recovery boot. |
| Add quorum proofs and playback fencing, 2026-08-22–23 | `9aad04c45` introduced 500 ms sampling, 750 ms timeout and a one-second proof lease; `025c65bb7` made serving depend on those proofs. | The later authority policy applied to embedded one-member databases as well as clusters. |
| Derive the displayed cluster mode from deployment topology, 2026-10-07 | `f1673a159`; configured advertisement or durable non-loopback membership determines the presentation. | A server can correctly appear standalone in Settings while still running Raft underneath. |

These are local Git/history findings, not inferred author intent. Inspect the
exact changes with `git show <commit>`; current explanations remain in
[architecture §2.1](../ARCHITECTURE.md#21-consensus--storage--embed-the-store-dont-run-a-database)
and the [review](CLUSTERING-PLAN-REVIEW.md).

The qualification record was narrower than today's failure. The
[M0 measurements](PHASE3-SPIKE.md) recorded 10,000 watch-row updates on an
M3 Max/APFS machine: SQLite/one-voter p95 was 0.051834/0.083333 ms and additional
idle RSS was 6.52 MiB. The record explicitly says neither side fsynced every
write and that it was an acknowledgment comparison, not a power-loss durability
measurement. M2 subsequently checked progress coalescing and compacted growth.
These are inspected historical reports, not measurements rerun here. Their
stated scope does not qualify standalone playback under the subsequently added
expiring authority checks. This is a limit of these records, not proof that no
other test exists.

The maintenance argument also needs revisiting: native SQLite was never
removed. Both implementations remain, and
[the Store contract harness](../../crates/plurx-core/tests/store_contract.rs)
already runs memory-backed and file-backed SQLite fixtures. Returning that
backend to normal daemon service therefore reuses maintained storage code,
while requiring explicit runtime parity work.

## 2. Approved decision — two real deployment modes, one application contract

Make **standalone SQLite the default**. Use the existing native `SqliteStore`
through `Arc<dyn Store>`. Construct Hiqlite only during explicitly requested
cluster creation/join or while reopening a directory already committed to
cluster mode. The same shipped binary may support both modes; linking Hiqlite
does not mean constructing a Raft node.

| Property | Standalone | Cluster |
|---|---|---|
| Authoritative application database | Native SQLite writer/read pool | Hiqlite replicated SQLite |
| Storage mutations | Local SQLite transactions | Replicated transactions |
| Catalogue reads | Local authoritative Store reads | Existing authority/bounded-replica policy |
| Raft node, log, elections, snapshots | None | Existing implementation |
| Raft/private cluster API listeners | None | Existing authenticated transport |
| Quorum sampler, watermark permits, quorum serving fence | None | Existing proof and loss behavior |
| Background ownership | Local atomic claims and publication generations | Replicated claims and membership authority |
| Readiness | Local startup/storage/drain state | Local readiness plus cluster authority |
| Enter this mode | Default fresh install or verified conversion | Explicit create/join transaction |

The shared application contract must remain one Store/API contract with two
tested implementations.

The owner additionally requires clustering to remain extractable. Native
identity/storage/backup/authority live under `plurx-core::storage`, with local
Source admission behind a neutral interface. The daemon's `storage_runtime`
selects the backend; `cluster_runtime` owns replicated construction, settings
observation, membership workers, Source coordination and managed promotion.
The shared backup scheduler accepts a provider; quorum readiness is an optional
serving provider in its own module. Native modules do not import Hiqlite or
create dummy voters. Existing HTTP membership/reporting compatibility adapters
are confined to the composition layer; their presence does not start Raft.

There is no Developer toggle for this correctness change. Settings should report the actual storage/deployment mode and provide
the deliberate cluster setup operation in its permanent section.

**Accepted replacement for REQ-HA-1:** a standalone server uses native SQLite
with zero required cluster configuration and no consensus runtime or quorum
checks. Creating or joining a cluster explicitly converts/selects replicated
storage; HA still requires three or more voters, with two-node HA unsupported.
An existing cluster never becomes standalone merely because peers are missing.

REQ-HA-1, architecture §2.1, the clustering plan's default deployment contract
and operations/configuration examples are updated in this implementation. Preserve this history and the old qualification
records rather than rewriting them as if they tested this design.

## 3. Backend selection must precede replicated initialization

Introduce normal `StandaloneSqlite` selection distinct from `SqliteRecovery`.
Resolve it under the existing exclusive data-directory lock before constructing
Raft, cluster credentials/listeners, startup peer-clock admission or daemon
state. [Today's selector](../../crates/plurx-core/src/cluster/migration.rs)
has only replicated and interrupted-activation recovery outcomes.

Use a small, versioned node-local storage manifest to name the active backend,
authoritative relative database path, logical/node identity and storage
generation. Publish it durably by atomic replacement and parent-directory sync.
Transition intent and verification receipts live outside compiler/cache roots.
The manifest is backend selection authority; a stored application setting or
temporarily empty peer list cannot override it.

Validate the format version, relative-path containment, regular-file identity,
backend/schema and embedded identity/generation binding before opening a writer.
Reject absolute paths, traversal and targets outside the owned root, including
symlink escapes. A missing manifest on a directory already stamped with the new
mode schema is a lost authority record, not a fresh-install opportunity.

New directories default to SQLite and persist that selection. The optional
`[storage]` mode preference may seed a new directory, but must not switch an
existing directory by itself. A mismatch requires the managed conversion
operation. Public status should distinguish backend, deployment topology and
readiness rather than derive one from another.

| Directory at upgrade/start | Required selection |
|---|---|
| Fresh directory | Initialize native SQLite; persist standalone selection. |
| Existing native SQLite, no activation handoff | Validate/upgrade it and persist standalone selection; do not import it into Raft. |
| Existing active Hiqlite, including private loopback one-voter | Keep that authoritative backend until verified conversion; never silently select retained root SQLite. |
| Join intent | Explicit cluster admission into an eligible directory; never overwrite a standalone library. |
| Interrupted mode transition | Recover from its durable phase record and verified artifacts, without guessing which database is newest. |
| Missing active data with a prior handoff record | Refuse and require recovery; do not create a fresh empty database or reopen stale state. |
| Malformed/unknown manifest, invalid target path, or identity/generation/schema mismatch | Refuse before opening a writer; never fall back to another backend. |
| Missing manifest with a new-mode schema or unresolved mode-transition evidence | Refuse or execute the recorded recovery phase; never reinterpret it as an unconverted legacy directory. |

Fresh native installations and upgrades of existing native installations keep
their authoritative database at the legacy canonical `<data_dir>/plurx.db` path.
The native schema's compatibility refusal therefore sits where pre-mode
binaries actually look. Converted installations also publish their native
authority at this canonical path; incoming and retirement directories carry
generation names while the database embeds its generation binding. They retain
the legacy activation-handoff refusal described in §6. An unknown manifest
alone is insufficient old-binary protection.

Opening a normal standalone server must not create cluster secret files or
manufacture a Raft ID. Preserve existing server/node ownership identities;
separate their type from optional replication identity. The
[current identity type](../../crates/plurx-core/src/cluster.rs) and local
[purpose-key census](../../crates/plurx-core/src/store/sharing_purpose_keys.rs)
historically required a positive Raft ID. Native bootstrap now uses neutral local
identity and census APIs; the cluster adapter retains its real Raft identity.

## 4. Local correctness remains, distributed authority does not

Separate common admission/drain/publication safety from quorum readiness in
[serving authority](../../crates/plurxd/src/serving_fence.rs). Standalone uses
local startup and shutdown state, the process/data-directory lock and local
transactional publication guards. It must neither fabricate a quorum proof nor
renew an "always valid" distributed lease. The current SQLite-ready test is a
useful starting regression, not complete daemon acceptance.

Keep restart/shutdown drains, bounded local database errors, transaction
rollback, claim generations, stale-producer refusal and recoverable work.
SQLite contention or a transient busy response must not become
`serving_fenced`/"lost quorum". Fatal local storage failures still have accurate
local error/readiness behavior; removing Raft does not make failed writes
successful.

Use the existing `UnclusteredJobAuthority` and native lease/publication Store
methods. Local job leases coordinate tasks and restart recovery; they do not
require another node or consensus. Audit every startup loop explicitly: retain
local workers, cache cleanup and orphan reconciliation, and do not construct or
spawn quorum sampling, membership heartbeat, peer health, cluster clock or
cross-node takeover machinery in standalone mode. This is stronger than
starting those tasks and relying on them to return early.

The native writer currently configures WAL with `synchronous=NORMAL`
([housekeeping](../../crates/plurx-core/src/store/sqlite/housekeeping.rs)).
Propose WAL plus `synchronous=FULL` for authoritative standalone data and an
explicit platform flush policy, including macOS `fullfsync`. Measure that
policy rather than carrying over the old acknowledgment-only comparison.
SQLite documents the WAL durability distinction between NORMAL and FULL, and
the platform-specific full-sync option in its
[pragma reference](https://www.sqlite.org/pragma.html#pragma_synchronous).
Do not claim protection from failed storage hardware or waive durability to
meet a latency number.

## 5. Native daemon parity is part of the mode, not a later workaround

| Boundary | Investigation baseline | Implementation contract |
|---|---|---|
| Storage interface | Native SQLite implements the complete composed Store traits; default contracts include file-backed fixtures. | Reuse it and add daemon-level acceptance; table/method presence alone does not prove feature readiness. |
| Source/Cinema initialization | `SelectedStore::prepare_source_schema_before_serving` returned early for SQLite at the investigation baseline, while the replicated path provisions dispatch, purpose keys and schema authority. | Provide local schema/key/publication initialization for supported single-server Source/Cinema flows, with ordinary local authority. Keep peer-cluster transport separate from remote-user/client access. |
| Backup | [BackupManager](../../crates/plurxd/src/backup.rs) refused when no Hiqlite client existed at that baseline. | Implement native SQLite backup/restore with schema/backend identity, consistent image, hashes, key material and existing scheduling/retention. |
| Discovery/status/settings | Displayed cluster-enabled state is independent of the backend today. | Report native SQLite/standalone truthfully; suppress cluster-only controls and metrics without inventing synthetic quorum health. |
| Operational commands | Some maintenance and connection paths assume an activated Store. | Inventory CLI/admin maintenance, password recovery, diagnostics, imports and package/service lifecycle; select the actual backend without bypassing current authentication/cache-invalidation rules. |
| Cache/offline/media ownership | Ownership uses logical and node identities, not merely database location. | Preserve IDs, paths, valid cache/offline ownership and credential AAD during conversion. Reconcile expired sessions/claims explicitly. |

The backup implementation can reuse SQLite's
[online backup API](https://www.sqlite.org/backup.html) for a consistent native
image. A cluster backup restored by today's
`restore_cluster_backup_archive` creates another one-voter Hiqlite instance;
it is not the native restore implementation this proposal needs.

Single-host playback, subtitles, Live TV/DVR, scanning, watch state, offline
packages, background processing, auth and supported Source/Cinema operations
must not acquire an implicit dependency on enabling a cluster. Cross-node HA,
replicated membership and takeover remain cluster features. Runtime gaps found
during this audit require explicit tasks and acceptance, not false readiness or
silent removal of otherwise supported local features.

## 6. Existing standalone installations require a real data conversion

**Do not reopen the retained root `plurx.db`.** After activation it is a
pre-activation rollback source. The current selector deliberately refuses to
reuse it after authority has moved. Every post-activation user change lives in
the active Hiqlite state machine.

**Do not rename that state-machine database into place either.** At this
baseline the native schema ends at `PRAGMA user_version = 123`, while the
replicated schema uses `cluster_meta.schema_version = 82`. Their
[lineage bridges](../../crates/plurx-core/src/store/schema_lineage.rs) also
have different ordinal ranges. A valid replicated SQLite file is not thereby
a native SqliteStore database.

Implement a versioned logical exporter/importer into a fresh native schema,
with an exhaustive inventory classifying every source table/column as copied,
semantically transformed, derived/rebuilt or replication-only/retired.
Unknown schema shapes or unclassified durable data refuse conversion. Reuse
the existing import's hashing/census techniques, not an assumption that its
SQLite-to-Hiqlite SQL can simply run backward.

The first supported conversion cohort is a proven private one-node
installation that has not admitted peers. Configured "standalone", one live
process, one reachable voter or an empty peer cache is insufficient. Validate
committed membership including learners, admission/removal history, identity
and outstanding cluster-scoped custody/ownership. Ambiguous history requires
the separate decommission/export procedure, not an automatic downgrade.

The managed conversion is a maintenance operation:

1. Acquire the root's exclusive daemon control/transition ownership. Refuse new
   work, drain writers and media safely, and establish a fully applied
   authoritative cut through the existing backend. Record its membership,
   schema, identity and cut. Preserve the current image and keys in an immutable
   backup outside scratch/cache. A stopped image without an established cut is
   not a substitute; recovery may temporarily need the existing backend to
   settle its durable log before export.
2. Shut down the old runtime and hold the root lock for offline conversion.
   Build a fresh native target in an incoming generation. Preserve logical
   server and node IDs, row IDs, user/token/API keys, watch state, library/DVR
   metadata and sealed credential material. Preserve publication counters;
   settle/requeue live claims through documented restart semantics rather than
   copying them as live workers. Account explicitly for cache/offline files and
   Source/Cinema sessions, custody, purpose keys and AAD.
3. Verify every classified durable field with semantic row hashes and counts;
   verify integrity, foreign keys, search rebuilding, credential decryptability,
   purpose-key census and normal native Store reads. Native schema migrations
   stamp their own version. A whitelist of a few familiar tables is inadequate.
4. Fsync the verified target and receipt. Retire the old auto-detected Hiqlite
   path under the recoverable transition journal, retain its handoff breadcrumb,
   then atomically publish the manifest naming the new native generation. This
   is an authority handoff, not deletion of rollback material. Crash points
   between retirement and publication must recover deterministically.
5. Start an ordinary standalone runtime from the published target. Verify
   backend identity, no consensus activity, actual playback and recovery of
   durable local work before declaring conversion accepted.

Before publication, a cancelled/failed conversion retains the
old authoritative backend. After SQLite has accepted writes, returning to the
retired Hiqlite image loses those writes and is forbidden. Repair forward or
perform a verified export/import from the current SQLite authority.

Older binaries must not reopen retired Hiqlite or automatically activate a
new standalone SQLite database. For converted installs, preserve the existing
activation-handoff refusal and move the old active directory out of its
auto-detected path. For native/fresh installs, publish the new mode metadata
through a new native schema migration so supported pre-mode binaries hit their
existing future-schema refusal before import. Reserve/revalidate that migration
against the implementation base. The refusal must be present at the legacy
canonical detection paths, including root `plurx.db`, not only in the target
named by the new manifest. Add exact old-binary rollback drills with ignored or
missing manifests and canonical converted targets; an unknown new
manifest by itself cannot fence an old executable.

Existing multi-node and formerly clustered deployments keep their replicated
backend. Converting them requires a separate explicit healthy-cluster
decommission/export contract: retire peers and old authority, transfer ownership
and obtain a complete cut. A minority must never export itself as the current
whole cluster merely because the other nodes are unavailable.

## 7. Cluster adoption becomes an explicit operation

Offer managed **Create cluster** and **Join cluster** operations. Creating a
cluster from standalone quiesces writes, backs up the current native authority,
imports into a fresh replicated target with parity checks, and publishes the
mode handoff before reopening service. The existing forward import provides a
starting mechanism; its restart/recovery behavior must obey the new persistent
mode contract instead of automatically retrying on every standalone boot.

Joining an existing cluster consumes an eligible fresh directory as today.
Joining with an existing standalone library requires an explicit data
disposition or separately designed merge; no implicit overwrite is allowed.
Cluster setup may require a restart. Local playback never requires cluster
adoption simply to make a backend-specific operation work.

## 8. Acceptance must prove the absence of consensus in standalone service

These are acceptance requirements. Section 11 records executed source checks
and identifies the remaining platform, playback and release qualification.

| ID | Required evidence |
|---|---|
| S1 | Fresh and restarted normal daemon uses file-backed SqliteStore. Process/task/constructor instrumentation proves zero Raft nodes, elections, quorum attempts and cluster listeners, including on binaries built with Hiqlite support. |
| S2 | Source/runtime paths cannot obtain a Hiqlite client or issue a quorum/replica permit in standalone mode. Cluster services fail construction in a dedicated negative fixture. Native catalogue requests use local authority. |
| S3 | Web playback starts and remains usable under background indexing, local database contention and laptop suspend/resume. Errors reflect actual local failures; missing/expired quorum cannot produce a refusal. Record start latency and long-run behavior on the affected macOS class and a supported Linux host. |
| S4 | File-backed daemon acceptance covers auth, scanning, progress/resume, subtitles, offline packages, Live TV/DVR, local jobs, backup/restore and supported Source/Cinema flows. Existing Store contracts also run against both backends. |
| S5 | Crash/restart and graceful shutdown preserve local drains, exclusive process ownership, claim fencing and orphan recovery; stale producers cannot publish after a replacement generation. |
| S6 | Convert representative current and supported older activated singleton images; compare the complete classified data inventory, identities, decrypted credential semantics, file ownership and resumed durable work. Fail closed on unknown/future schemas. |
| S7 | Inject failure at every transition durability boundary. Reject malformed/unknown/missing manifests, unsafe paths and manifest-target identity/generation mismatches before writes. New startup chooses exactly one authoritative generation. Old binaries refuse even when they ignore or cannot see the manifest including when a manifest is missing, instead of reading stale root data or bootstrapping Raft. Never write to a retained rollback database. |
| S8 | Backup/restore returns a working native standalone daemon with key material, not an incidental one-voter cluster. Verify restore in isolation before replacing an installation. |
| S9 | Cluster create/join is explicit and crash-safe. Existing three-voter/learner, partition, membership, removal and quorum-loss fencing tests keep their original semantics; an isolated/former member cannot auto-downgrade. |
| S10 | Benchmark SQLite with the accepted real durability settings: playback latency, foreground/background contention, acknowledged commit latency, idle RSS, write rate and compacted growth. Distinguish process-crash tests from actual OS/power-loss evidence. No "zero physical overhead" claim. |

The existing memory/file Store contract harness and
`serving_fence::tests::sqlite_is_ready_without_a_quorum_sampler` can be reused.
They do not replace the negative daemon-level assertions, migration crash
matrix or real playback acceptance above.

## 9. Approved implementation sequence

Use one `effort/standalone-sqlite` integration branch because the tasks share
startup, Store and identity files. Follow the existing effort/main gates;
do not claim a proposal or compiler run is release qualification.

| Stage | Owned surface | Exit evidence |
|---|---|---|
| M0 — ratify and inventory | Governing requirements/architecture, authoritative-directory cases, complete data/feature/CLI inventory and supported old-binary matrix | Accepted mode/durability contract; migration inventory has no unknown durable classes; no runtime patch yet. |
| M1 — native selection and authority | Config/selection/identity, `main.rs`, `state.rs`, serving admission and coordination | Fresh/native installs pass S1, S2 and S5; actual cluster state remains unchanged. |
| M2 — daemon parity and backup | Native Source/Cinema startup, jobs, operational commands, backup/restore, settings/status | S4 and S8 pass; no feature depends on a fabricated Raft identity. |
| M3 — existing-data conversion | Versioned exporter/importer, authority manifest/journal, schema compatibility and rollback fencing | S6 and S7 pass with immutable recovery material; existing singleton users have an upgrade path. |
| M4 — explicit cluster transitions | Forward import/create/join lifecycle and decommission refusals | S9 passes, with no automatic collapse or library overwrite. |
| M5 — qualify and deploy | Combined tree, installation/update lifecycle, affected Mac and supported Linux acceptance | S3 and S10 plus all preceding acceptance on the final candidate; deploy only through the existing release process. |

Do not ship a fresh-install-only mode while leaving today's standalone users
trapped on the old backend. M1/M2 can integrate before M3, but product completion
requires the supported existing-singleton conversion and playback proof.

## 10. Options and present disposition

| Option | Assessment |
|---|---|
| Keep one-voter Raft and tune leases/apply waits | Rejected for this request. It retains consensus lifecycle and quorum-derived availability in standalone service. The earlier patch is not the proposed solution. |
| Put Hiqlite into a nominal local mode but continue constructing its Raft runtime | Rejected. It fails the requested architecture and the S1/S2 acceptance boundary. |
| Restore native SQLite standalone; explicitly activate Hiqlite for clusters | Approved. It restores a real local mode using existing Store code and pays the explicit runtime/conversion/testing costs described here. |

The architecture and implementation are authorized. Runtime source changes
are under validation; no running installation or user database has been
changed. Release/promotion and physical acceptance remain distinct from
implementation and focused source regressions. Knowledge lookup at this baseline has no
reviewed semantic coverage for this backend decision; broad imported check
associations are impact hints, not proof of obligations or executed acceptance.


## 11. Implementation ledger — source evidence, not release qualification

The implementation is on `codex/standalone-sqlite-runtime`. The following checks
ran on an isolated, non-voter Linux VM using Rust 1.97.1 and a source-only
archive. Full logs and exact Git tree identities are retained in the task's
owned evidence directory outside compiler caches. No source credentials or
repository metadata were sent to the compiler host.

| Source snapshot | Observed result | Scope |
|---|---|---|
| `477ffb133c3acf31e686e235e70787f70f4074db` | Native `storage::` suite passed 32/32 without Hiqlite features. | Exclusive ownership through cloned/queued writers; identity and missing-manifest refusal; WAL-aware backup/restore; sealed purpose history; six-table local telemetry; interrupted authority handoff recovery. |
| `656c58beb9e52567bc92ca621ae62e76ab3e87b1` | Native Source passed 8/8 without Hiqlite features. | Real native factory, local census/boot fencing, retained sealed material, restored purpose history, actual writer cancellation and exact external dependency preservation/refusal. |
| `73628aace20b2f15e7b8c642c92c3b10ea10f3b8` | Native `sharing_source_session` passed 20/20; schema dependency race regression passed with Hiqlite enabled. | Real native boot and accepted-driver capability, exact lease fencing, cancellation/custody settlement; atomic dependency detach/recreation without accepting unknown schema. |
| `5931aeeac88112dc7b82bb4837fc8895f1c0dc7e` | Reverse exporter passed 12/12. | Typed census, immutable snapshot refusal, exact FTS rebuild, row/sequence continuity, pending Source admission refusal and canonical protocol retirement with domain grants/generations preserved. A substituted bootstrap view refuses before destination mutation. |
| `5931aeeac88112dc7b82bb4837fc8895f1c0dc7e` | Native promotion passed 5/5, including an actual applied singleton. | Interrupted authority boundaries, real membership provenance, retained master/Source material and packed local data, accepted target writes, and root-lease reacquisition after terminal drain. |
| `d875ae63f0e48f72f4ca9627e2b1306c1d3f1cbe` | Real daemon managed create/restart passed. | Native-to-cluster promotion, retained sealed keys/local data, post-handoff writes, handled-error writer drain and actual lease reacquisition. |
| `5931aeeac88112dc7b82bb4837fc8895f1c0dc7e` | Daemon selection passed 6/6; explicit-cluster provenance passed 2/2; configuration passed 17/17. | Native boot with occupied cluster ports, loss/corruption refusal, committed-schema restart preflight, deliberate cluster creation/join/restore remaining clustered without peer-reachability inference, and unused Raft tuning ignored by native loading. |
| `5931aeeac88112dc7b82bb4837fc8895f1c0dc7e` | Workspace/all-targets Clippy passed with warnings denied; workspace formatting passed. | Final runtime source, including the startup/serving owner guard, committed-schema binding, explicit membership provenance and native job authority. |
| `5931aeeac88112dc7b82bb4837fc8895f1c0dc7e` | Forward importer passed 8/8; serving-fence suite passed 25/25. | Full typed native census and accepted-data refusal, actual Source custody refusal, FTS/sequence continuity, native admission without an external proof, and preserved cluster outage/drain semantics. |
| `5931aeeac88112dc7b82bb4837fc8895f1c0dc7e` | Both shipping-daemon singleton conversion journeys passed. | Actual pre-Source and Sharing-enabled replicated stores become native SQLite with current data, identities, master and sealed Source keys preserved; daemon readiness/auth, no cluster listeners, native backup API, restore CLI and native restarts. |
| `5931aeeac88112dc7b82bb4837fc8895f1c0dc7e` | Backup suite passed 11/11; three real Source producer/HTTP regressions passed. | Native archive creation without a cluster client, retained cluster cut/deadline semantics, actual producer admission, expired original cluster observation refusal, and H1/H2 playlist/init/segment lineage. |
| `d75e8230b4c181192b8520b05d33917be42b9a94` | Native Source provider passed 2/2. | Real native boot/driver registry, capability revocation and missing-provider refusal without membership fallback. |
| `e1ce9af4ce7247c6e9743f9952692758259282bc` | All 155 focused storage regressions and four affected web files passed after integrating current main. | Includes actual AUTH81/AUTH82 shipping-daemon conversion, native restarts, backup/restore and managed promotion; precedes the additional blocking-worker cancellation correction below. |
| `c65eca14b5c57a2167d2b9aaa565c0c91129d816` | Promotion suite passed 6/6; real daemon managed create/restart passed. | All three blocking promotion workers retain the actual root lease through cancellation. The new regression cancels an awaiting task during a paused SQLite transaction, proves exclusion until completion and verifies its committed write; the daemon retains Source keys, local data and accepted restart writes. |

Web settings, cluster-membership, asset-order and asset-load contracts passed,
including all 80 settings cases. The validation catalog passed with 38 points
and 44 checks; paperwork passed all 15 checks. The final commit also runs the
normal formatting, catalog, knowledge, Clippy and embedded-JavaScript hooks.

Review corrections include retaining the actual selected cluster and directory
lease across cancellation/failed writer drainage, binding cluster manifests to
the schema already committed before Source preparation, and preventing a lost
native manifest from being bypassed with cluster configuration. Native service
construction belongs to the backend composition module; optional cluster
construction and the replicated Source provider remain in the cluster adapter.
Native job execution, offline packages, Trakt and watched drains receive an
explicit local job authority rather than an inactive membership manager.

PR review also corrected the lifetime of independent blocking promotion work:
archive preparation and both telemetry workers now retain their own directory
lease. Cancelling the parent future cannot admit another writer while those
workers still mutate SQLite. The strengthened regression reacquires a fresh
root lease before starting the worker, preventing startup handles from masking
premature release.

Astra separately reviewed the exact previously admitted job/lease/SQLite
source deltas. All three formal correspondence checks passed without drift;
bounded smoke completed nine durable-job configurations, two operation-progress
configurations and one worker-refinement configuration. Thirty-eight focused
formal-runner tests passed. Models, configurations, properties and fairness
inputs are unchanged. Native selection, directory ownership, migration and
shutdown remain external runtime obligations; these results do not transfer
the finite worker models' claims to those mechanisms.

These are focused source checks, not full release qualification. The recorded
snapshots distinguish runtime changes from later documentation; the normal
commit hook recompiles/lints the exact staged tree. The effort/main promotion gates,
broader feature and multi-node acceptance, old-binary execution drills, affected
Mac playback/suspend, and durable SQLite performance acceptance remain pending.
FFmpeg on this compiler VM is 8.0.1; a passing
software check there does not claim standard-runner FFmpeg 6 or physical Mac
acceptance. Power-loss/OS-crash durability and Windows execution remain pending;
process-crash recovery tests do not provide that evidence. No installation or
production database has been upgraded, and no release qualification is claimed.
