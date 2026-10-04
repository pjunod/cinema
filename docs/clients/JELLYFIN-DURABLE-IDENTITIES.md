# Jellyfin durable identities — preserve meaning across deletion and import

**Status:** open · J1 identity task implemented and locally checked; effort gate pending ·
**Written:** 2026-10-02.

Companion to [the build contract](JELLYFIN-COMPATIBILITY-BUILD.md) and
[the protocol foundation](JELLYFIN-PROTOCOL-FOUNDATION.md). This task adds
permanent catalog identity storage. Binding lifecycle, shared auth/watch
services, HTTP routes and physical client qualification remain open.

## 1. Storage — permanent tombstones, bounded allocation

`JellyfinIdentityStore` is part of the native Store boundary. Its entity kind
is a closed enum: user, library, item or file. Page allocation accepts at most
1,000 positive native integers, deduplicates them and returns existing live
mappings in native-ID order. Missing native entities are omitted. A repeated
page whose mappings exist creates no new IDs and submits no replicated write.

Each missing mapping gets a random wire UUID and a separate random incarnation,
supplied as parameters before replication. A partial unique index permits one
live mapping per native entity; conditional insertion returns the durable
winner under concurrent allocation. SQLite reads, allocates and reads back
inside its writer transaction. Hiqlite consistently reads the page, submits
all missing conditional inserts in one transaction, then consistently reads
the winning mappings. Deletion between those operations can remove a result;
a retired mapping never resolves to a replacement entity.

Deterministic `AFTER DELETE` triggers retire mappings on all four native tables.
Their updates execute inside the deleting transaction, including FK cascades.
There is no clock, RNG or cleanup TTL in the trigger. Retired rows remain
permanent: a reused native integer gets a new incarnation and wire UUID.
Native file path upsert retains its mapping; negotiated recipes must separately
validate changed source fingerprints when playback integration is added.

## 2. Migrations — upgrade and fresh bootstrap share one schema

SQLite appends migration 98. Hiqlite advances schema 73 to 74 and installs the
same idempotent schema during fresh bootstrap after native catalog tables
exist. The protocol activation range stays under its existing mechanism.
The import inventory includes all five identity columns from SQLite version
92 onward, including retired rows; an older source starts with an empty mapping
table. This preserves IDs and permanent non-reuse evidence during activation.

The SQL placeholder and SQLite transaction inventories include the new backend
modules. Allocation uses only enum-selected table names and generated numeric
placeholders; entity values are bound parameters. The protocol source guard
continues to reject mapped native table replacement writes.

## 3. Focused proof — identity behavior, not facade completeness

| Regression | Evidence it requires |
|---|---|
| `jellyfin_ids_converge_retire_cascades_and_survive_integer_reuse` | Concurrent allocation converges; repeat sync and file rescan retain IDs; missing/invalid/oversized batches are bounded; library deletion retires nested item/file mappings; reused integers cannot revive old IDs on SQLite and three voters |
| `jellyfin_user_retirement_survives_reopen_and_schema_replay` | SQLite restart and repeated schema installation preserve user retirement and all four triggers |
| `jellyfin_import_preserves_live_and_retired_ids_on_another_node` | SQLite import carries live and retired rows, and another cluster client resolves only the current identity |
| `fresh_bootstrap_matches_the_migration_chain_from_a_frozen_v42_tree` | Fresh cluster and existing upgrade chain produce the same schema |

The two backend contracts, restart/replay test and schema parity test pass on
pinned Rust 1.97.1. The fifteen placeholder-census tests, transaction inventory,
import inventory, four document-index tests, catalog lint and workspace
Clippy/format/script checks also pass. Integration evidence must be rerun
against the current effort when prerequisite tasks land.

Run the contract tests with `hiqlite-contract-tests` to include the real
three-voter backend. Core unit tests use `--features hiqlite-store`. These
receipts do not qualify play binding expiry, token replacement/logout,
watch-state revisions or either physical client.
