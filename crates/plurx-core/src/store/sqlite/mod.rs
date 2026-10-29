//! Single-node SQLite backend for the [`Store`](super::Store) trait family.
//!
//! rusqlite is synchronous, so all access hops onto the blocking pool via
//! `spawn_blocking` around one mutex-guarded connection. That is plenty for
//! Phase 0–2 write rates (see ARCHITECTURE §2.2); read-heavy paths can grow a
//! read pool later without touching the traits.
//!
//! Implementation is split by domain area: `users`, `library`, `media`,
//! `watch` — this file owns open/migrate, shared row mappers, and settings.

mod annotations;
mod apikeys;
mod background_jobs;
mod cache;
mod classification;
mod coordination;
mod dv_conversion;
mod dvr;
mod file_grants;
mod fragindex;
mod fragment_index_cluster;
mod housekeeping;
mod invitations;
mod jellyfin_catalog;
mod jellyfin_identity;
mod jellyfin_login;
mod jellyfin_play;
mod library;
mod library_channels;
mod live_tv_resource;
mod media;
mod offline;
mod outbox;
mod playback_handoffs;
mod pretranscode;
mod publication;
mod reading;
mod remote;
mod sessions;
mod shared_cache;
mod sharing;
mod telemetry;
mod timeline_annotations;
mod trakt;
mod users;
mod watch;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension, Row};

use super::{
    keys, validate_generated_settings, ArtworkRepairFence, MetricsStore, PrometheusStoreSnapshot,
    SettingsStore,
};
use crate::cluster::coordination::Lease;
use crate::domain::{DolbyVisionFacts, Item, ItemKind, MediaFile, OfflinePackageStats, User};
use crate::error::StoreError;
use crate::store::telemetry::{NETWORK_PRIORS_V2_SCHEMA, PLAYBACK_EVENTS_SCHEMA};

/// The SQLite migration that adds `offline_packages.audio_recipe`, and the
/// schema version it lands at. A source below that version has no resolved
/// audio snapshot, which the Hiqlite import projects as NULL.
pub(crate) const OFFLINE_AUDIO_RECIPE_COLUMN: &str =
    "ALTER TABLE offline_packages ADD COLUMN audio_recipe TEXT;";
pub(crate) const OFFLINE_AUDIO_RECIPE_SCHEMA: i64 = 92;

/// Whether migration `version` (1-based, as stored in `user_version`) is
/// exactly `sql`. Used by the compile-time assertions below so a schema
/// version named elsewhere cannot drift from its entry in [`MIGRATIONS`].
const fn migration_is(version: i64, sql: &str) -> bool {
    if version < 1 || version as usize > MIGRATIONS.len() {
        return false;
    }
    let actual = MIGRATIONS[version as usize - 1].as_bytes();
    let expected = sql.as_bytes();
    if actual.len() != expected.len() {
        return false;
    }
    let mut index = 0;
    while index < actual.len() {
        if actual[index] != expected[index] {
            return false;
        }
        index += 1;
    }
    true
}

const _: () = assert!(
    migration_is(OFFLINE_AUDIO_RECIPE_SCHEMA, OFFLINE_AUDIO_RECIPE_COLUMN),
    "OFFLINE_AUDIO_RECIPE_SCHEMA does not name the audio_recipe migration"
);
const _: () = assert!(
    migration_is(
        super::candidate_recovery::SQLITE_INTRODUCED_SCHEMA,
        super::candidate_recovery::SCHEMA
    ),
    "candidate_recovery::SQLITE_INTRODUCED_SCHEMA does not name its migration"
);
const _: () = assert!(
    migration_is(
        super::background_jobs::ENCODED_OUTPUT_SQLITE_SCHEMA,
        super::background_jobs::ENCODED_OUTPUT_SCHEMA
    ),
    "background_jobs::ENCODED_OUTPUT_SQLITE_SCHEMA does not name its migration"
);

/// First SQLite schemas containing the continuous-quality tables: the
/// cancellation receipts and preparation owners, then the parent-fenced
/// ledger. The Hiqlite import gates those tables on these, and the
/// assertions below keep them naming their entries in [`MIGRATIONS`].
pub(crate) const QUALITY_CANCELLATION_SQLITE_SCHEMA: i64 = 102;
pub(crate) const QUALITY_LEDGER_SQLITE_SCHEMA: i64 = 103;
const _: () = assert!(
    migration_is(
        QUALITY_CANCELLATION_SQLITE_SCHEMA,
        super::quality_cancellation::QUALITY_CANCELLATION_SCHEMA
    ),
    "QUALITY_CANCELLATION_SQLITE_SCHEMA does not name its migration"
);
const _: () = assert!(
    migration_is(QUALITY_LEDGER_SQLITE_SCHEMA, super::quality_ledger::SCHEMA),
    "QUALITY_LEDGER_SQLITE_SCHEMA does not name its migration"
);

/// Ordered, append-only migration list. `PRAGMA user_version` tracks the last
/// applied index + 1. Never edit an entry that has shipped — append instead.
/// Visible to the import inventory guard, which needs to know which migration
/// first creates each table it plans to read.
pub(crate) const MIGRATIONS: &[&str] = &[
    // v1: settings KV — the seed of all replicated durable state.
    "CREATE TABLE settings (
        key        TEXT PRIMARY KEY,
        value      TEXT NOT NULL,
        updated_at INTEGER NOT NULL DEFAULT (unixepoch())
    ) STRICT;",
    // v2: Phase 1 — users/auth, libraries, media items & files, watch state,
    // and full-text search over items.
    "CREATE TABLE users (
        id            INTEGER PRIMARY KEY,
        username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
        password_hash TEXT NOT NULL,
        is_admin      INTEGER NOT NULL DEFAULT 0,
        created_at    INTEGER NOT NULL DEFAULT (unixepoch())
    ) STRICT;

    CREATE TABLE tokens (
        token_hash   TEXT PRIMARY KEY,
        user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        device       TEXT,
        created_at   INTEGER NOT NULL DEFAULT (unixepoch()),
        last_seen_at INTEGER NOT NULL DEFAULT (unixepoch())
    ) STRICT;

    CREATE TABLE libraries (
        id         INTEGER PRIMARY KEY,
        name       TEXT NOT NULL UNIQUE,
        kind       TEXT NOT NULL CHECK (kind IN ('movies','shows')),
        paths      TEXT NOT NULL,
        created_at INTEGER NOT NULL DEFAULT (unixepoch())
    ) STRICT;

    CREATE TABLE items (
        id             INTEGER PRIMARY KEY,
        library_id     INTEGER NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
        kind           TEXT NOT NULL CHECK (kind IN ('movie','show','season','episode')),
        parent_id      INTEGER REFERENCES items(id) ON DELETE CASCADE,
        title          TEXT NOT NULL,
        sort_title     TEXT NOT NULL,
        year           INTEGER,
        overview       TEXT,
        tmdb_id        INTEGER,
        imdb_id        TEXT,
        season_number  INTEGER,
        episode_number INTEGER,
        air_date       TEXT,
        runtime_ms     INTEGER,
        poster_path    TEXT,
        backdrop_path  TEXT,
        added_at       INTEGER NOT NULL DEFAULT (unixepoch()),
        updated_at     INTEGER NOT NULL DEFAULT (unixepoch())
    ) STRICT;
    CREATE INDEX idx_items_library_kind ON items(library_id, kind);
    CREATE INDEX idx_items_parent ON items(parent_id);
    CREATE INDEX idx_items_added ON items(added_at DESC);

    CREATE TABLE files (
        id               INTEGER PRIMARY KEY,
        item_id          INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
        path             TEXT NOT NULL UNIQUE,
        size             INTEGER NOT NULL,
        mtime            INTEGER NOT NULL,
        duration_ms      INTEGER,
        container        TEXT,
        video_codec      TEXT,
        video_profile    TEXT,
        width            INTEGER,
        height           INTEGER,
        bit_depth        INTEGER,
        hdr              TEXT,
        bitrate          INTEGER,
        audio_streams    TEXT NOT NULL DEFAULT '[]',
        subtitle_streams TEXT NOT NULL DEFAULT '[]',
        probe_json       TEXT,
        scanned_at       INTEGER NOT NULL DEFAULT (unixepoch())
    ) STRICT;
    CREATE INDEX idx_files_item ON files(item_id);

    CREATE TABLE watch_state (
        user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        item_id     INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
        position_ms INTEGER NOT NULL DEFAULT 0,
        duration_ms INTEGER,
        watched     INTEGER NOT NULL DEFAULT 0,
        updated_at  INTEGER NOT NULL DEFAULT (unixepoch()),
        PRIMARY KEY (user_id, item_id)
    ) STRICT;
    CREATE INDEX idx_watch_updated ON watch_state(user_id, updated_at DESC);

    CREATE VIRTUAL TABLE items_fts USING fts5(
        title, overview, content='items', content_rowid='id'
    );
    CREATE TRIGGER items_fts_ai AFTER INSERT ON items BEGIN
        INSERT INTO items_fts(rowid, title, overview)
        VALUES (new.id, new.title, new.overview);
    END;
    CREATE TRIGGER items_fts_ad AFTER DELETE ON items BEGIN
        INSERT INTO items_fts(items_fts, rowid, title, overview)
        VALUES ('delete', old.id, old.title, old.overview);
    END;
    CREATE TRIGGER items_fts_au AFTER UPDATE OF title, overview ON items BEGIN
        INSERT INTO items_fts(items_fts, rowid, title, overview)
        VALUES ('delete', old.id, old.title, old.overview);
        INSERT INTO items_fts(rowid, title, overview)
        VALUES (new.id, new.title, new.overview);
    END;",
    // v3: Phase 2 — mark a (shows) library as anime, so the scanner uses
    // absolute episode numbering and enriches from AniList.
    "ALTER TABLE libraries ADD COLUMN anime INTEGER NOT NULL DEFAULT 0;",
    // v4: a human HDR label incl. the Dolby Vision profile ("Dolby Vision ·
    // Profile 7 (HDR10-compatible)", "HDR10+"). `hdr` stays the coarse type the
    // decision engine keys on; this is display detail. Backfilled on next scan.
    "ALTER TABLE files ADD COLUMN hdr_format TEXT;",
    // v5: Trakt account links (per user — one row each) and the per-file
    // manual A/V sync correction. Rescans never touch audio_offset_ms.
    "ALTER TABLE files ADD COLUMN audio_offset_ms INTEGER NOT NULL DEFAULT 0;

    CREATE TABLE trakt_auth (
        user_id         INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
        access_token    TEXT NOT NULL,
        refresh_token   TEXT NOT NULL,
        expires_at      INTEGER NOT NULL,
        trakt_username  TEXT,
        connected_at    INTEGER NOT NULL DEFAULT (unixepoch()),
        last_sync_at    INTEGER NOT NULL DEFAULT 0,
        last_activities TEXT
    ) STRICT;",
    // v6: home video & photos. The v2 schema baked the allowed kinds into
    // CHECK constraints, and SQLite cannot alter a CHECK — so both tables are
    // rebuilt. The CHECKs are dropped rather than extended: kind validation
    // already lives in `LibraryKind::parse` / `ItemKind::parse` (and
    // `item_from_row` fails loudly on an unknown kind), so the constraint was
    // redundant with app-level validation that has to exist anyway — and
    // every future media type would otherwise repeat this dance. STRICT
    // typing stays. Three traps, all handled here:
    //   1. `ALTER TABLE … RENAME` rewrites child FK references, so this is
    //      create-new → copy → drop-old → rename-new, never rename-old-first.
    //   2. FK enforcement must be OFF (the runner does that — `PRAGMA
    //      foreign_keys` is a no-op inside the transaction this runs in), or
    //      `DROP TABLE libraries` fails against its children.
    //   3. `items_fts` is contentless-external over `items`; rebuilding
    //      `items` orphans it, so the triggers and table are recreated and the
    //      index is rebuilt from scratch (now including tags).
    "DROP TRIGGER items_fts_ai;
    DROP TRIGGER items_fts_ad;
    DROP TRIGGER items_fts_au;

    CREATE TABLE libraries_new (
        id         INTEGER PRIMARY KEY,
        name       TEXT NOT NULL UNIQUE,
        kind       TEXT NOT NULL,
        paths      TEXT NOT NULL,
        created_at INTEGER NOT NULL DEFAULT (unixepoch()),
        anime      INTEGER NOT NULL DEFAULT 0
    ) STRICT;
    INSERT INTO libraries_new (id, name, kind, paths, created_at, anime)
        SELECT id, name, kind, paths, created_at, anime FROM libraries;
    DROP TABLE libraries;
    ALTER TABLE libraries_new RENAME TO libraries;

    CREATE TABLE items_new (
        id             INTEGER PRIMARY KEY,
        library_id     INTEGER NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
        kind           TEXT NOT NULL,
        parent_id      INTEGER REFERENCES items(id) ON DELETE CASCADE,
        title          TEXT NOT NULL,
        sort_title     TEXT NOT NULL,
        year           INTEGER,
        overview       TEXT,
        tmdb_id        INTEGER,
        imdb_id        TEXT,
        season_number  INTEGER,
        episode_number INTEGER,
        air_date       TEXT,
        runtime_ms     INTEGER,
        poster_path    TEXT,
        backdrop_path  TEXT,
        added_at       INTEGER NOT NULL DEFAULT (unixepoch()),
        updated_at     INTEGER NOT NULL DEFAULT (unixepoch()),
        recorded_at    TEXT,
        tags           TEXT NOT NULL DEFAULT '[]',
        nfo_seeded_at  INTEGER
    ) STRICT;
    INSERT INTO items_new
        SELECT id, library_id, kind, parent_id, title, sort_title, year, overview,
               tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms,
               poster_path, backdrop_path, added_at, updated_at, NULL, '[]', NULL
        FROM items;
    DROP TABLE items;
    ALTER TABLE items_new RENAME TO items;
    CREATE INDEX idx_items_library_kind ON items(library_id, kind);
    CREATE INDEX idx_items_parent ON items(parent_id);
    CREATE INDEX idx_items_added ON items(added_at DESC);

    DROP TABLE items_fts;
    CREATE VIRTUAL TABLE items_fts USING fts5(
        title, overview, tags, content='items', content_rowid='id'
    );
    CREATE TRIGGER items_fts_ai AFTER INSERT ON items BEGIN
        INSERT INTO items_fts(rowid, title, overview, tags)
        VALUES (new.id, new.title, new.overview, new.tags);
    END;
    CREATE TRIGGER items_fts_ad AFTER DELETE ON items BEGIN
        INSERT INTO items_fts(items_fts, rowid, title, overview, tags)
        VALUES ('delete', old.id, old.title, old.overview, old.tags);
    END;
    CREATE TRIGGER items_fts_au AFTER UPDATE OF title, overview, tags ON items BEGIN
        INSERT INTO items_fts(items_fts, rowid, title, overview, tags)
        VALUES ('delete', old.id, old.title, old.overview, old.tags);
        INSERT INTO items_fts(rowid, title, overview, tags)
        VALUES (new.id, new.title, new.overview, new.tags);
    END;
    INSERT INTO items_fts(items_fts) VALUES('rebuild');",
    // v7: scheduled jobs. Intervals are per library and in minutes, `0` = off
    // (the default, so an upgrade changes nobody's behavior). The last-run
    // stamps live on the row rather than in memory so a restart can't reset the
    // clock — a server that reboots nightly would otherwise either scan on
    // every boot or never scan at all, depending on which way that was fudged.
    // Plain ALTERs: no CHECK to fight, so no table rebuild.
    "ALTER TABLE libraries ADD COLUMN scan_interval_mins INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE libraries ADD COLUMN refresh_interval_mins INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE libraries ADD COLUMN last_scan_at INTEGER;
    ALTER TABLE libraries ADD COLUMN last_refresh_at INTEGER;",
    // v8: scoped API keys — a second kind of credential, for machines.
    //
    // A login token IS a user: it carries that user's privileges wholesale,
    // and an admin's token can read the TMDB/Trakt secrets straight out of
    // GET /api/v1/settings. So handing another application "a token so it can
    // trigger scans" hands it every secret plurx holds. A key carries a scope
    // list and nothing else — no user, no admin flag, no way to widen itself.
    //
    // Stored like tokens: SHA-256 of the secret, never the secret. The
    // plaintext is shown exactly once, at creation, and is unrecoverable
    // afterwards — losing it means issuing a new key, which is the correct
    // cost and keeps "the database leaked" from meaning "the keys leaked".
    "CREATE TABLE api_keys (
        id           INTEGER PRIMARY KEY,
        name         TEXT NOT NULL,
        key_hash     TEXT NOT NULL UNIQUE,
        scopes       TEXT NOT NULL DEFAULT '[]',
        created_at   INTEGER NOT NULL DEFAULT (unixepoch()),
        last_used_at INTEGER,
        disabled     INTEGER NOT NULL DEFAULT 0
    ) STRICT;
    CREATE INDEX api_keys_hash ON api_keys(key_hash);",
    // v9: "has the provider been consulted for this item yet?" as its own
    // fact, instead of inferring it from `tmdb_id IS NULL`.
    //
    // That inference was safe only while plurx itself was the only thing that
    // ever wrote a tmdb id. Now another application can hand one over on the
    // scan request (§3 of the integration plan), and under the old rule such
    // an item was treated as already enriched — id set, and therefore never
    // given a title, overview or poster. The id it arrived with is the reason
    // to enrich it, not a reason to skip it.
    //
    // Backfilled from `updated_at` for everything that already has an id, so
    // an upgrade does not re-fetch a library's worth of metadata from TMDB.
    "ALTER TABLE items ADD COLUMN metadata_at INTEGER;
    UPDATE items SET metadata_at = updated_at WHERE tmdb_id IS NOT NULL;",
    // v10: an outbox for watched notifications (master plan §11.1).
    //
    // plurx's FIRST outbound push. Everything before this was inbound — other
    // applications called plurx and plurx answered — and answering needs no
    // durability, because the caller is still there to be told. Pushing is the
    // opposite: the moment that matters is one where the far side may be
    // restarting, and nobody is waiting to retry on our behalf.
    //
    // So it is a table, not a channel. `next_at` makes the backoff survive a
    // restart, which is the whole reason to have rows at all: the common
    // failure is a host reboot, and an in-memory retry dies with the process
    // that owns it.
    //
    // `username` is here because of a decision recorded in the master plan:
    // the signal is per-user rather than aggregate. That is viewing history
    // leaving the application that has a reason to hold it, so the feature is
    // off by default and nothing is enqueued unless an admin turned it on.
    "CREATE TABLE watched_outbox (
        id          INTEGER PRIMARY KEY,
        payload     TEXT NOT NULL,
        attempts    INTEGER NOT NULL DEFAULT 0,
        last_error  TEXT NOT NULL DEFAULT '',
        status      TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending', 'ok', 'failed')),
        next_at     INTEGER NOT NULL DEFAULT 0,
        created_at  INTEGER NOT NULL DEFAULT (unixepoch()),
        updated_at  INTEGER NOT NULL DEFAULT (unixepoch())
    ) STRICT;
    CREATE INDEX watched_outbox_due ON watched_outbox(status, next_at);",
    // v11: the pre-transcode cache (PERF-PLAN §6.1).
    //
    // Two tables, not one, and the split is the whole design. A recipe is
    // *what* a transcode is — the content-addressed name of some bytes. A
    // location is *where a copy of it happens to be*, which is a different
    // fact with a different lifetime: on a cluster the same recipe exists on
    // several nodes, any of which may evict its copy without that saying
    // anything about the others. One row with one `dir` cannot express "A has
    // it, B had it and dropped it", and the day it has to, the migration is
    // far more expensive than the extra table is now.
    //
    // Directories are RELATIVE to the configured cache root. An absolute path
    // is a fact about one machine's mounts, and the point of the location
    // table is that the row travels while the mount does not.
    //
    // Deliberately not under `data_dir/transcode`, which is wiped at every
    // boot: a cache that empties on restart is a warm-up cost with none of the
    // benefit.
    "CREATE TABLE transcode_cache_recipes (
        recipe_hash    TEXT PRIMARY KEY,
        file_id        INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
        recipe_version INTEGER NOT NULL,
        created_at     INTEGER NOT NULL DEFAULT (unixepoch())
    ) STRICT;
    CREATE INDEX transcode_cache_recipes_file ON transcode_cache_recipes(file_id);

    CREATE TABLE transcode_cache_locations (
        recipe_hash   TEXT NOT NULL REFERENCES transcode_cache_recipes(recipe_hash)
                                    ON DELETE CASCADE,
        node_id       TEXT NOT NULL,
        storage_class TEXT NOT NULL CHECK (storage_class IN ('local', 'shared')),
        relative_dir  TEXT NOT NULL,
        bytes         INTEGER NOT NULL DEFAULT 0,
        complete      INTEGER NOT NULL DEFAULT 0,
        last_used_at  INTEGER NOT NULL DEFAULT (unixepoch()),
        last_seen_at  INTEGER NOT NULL DEFAULT (unixepoch()),
        PRIMARY KEY (recipe_hash, node_id, storage_class)
    ) STRICT;
    -- Eviction reads this: complete copies on this node, coldest first.
    CREATE INDEX transcode_cache_lru
        ON transcode_cache_locations(node_id, complete, last_used_at);",
    // v12: "was artwork ever actually fetched for this item, and what
    // happened?" — the fact that `poster_path IS NULL` could not express.
    //
    // The enrichment queue (`items_needing_metadata`) keys on `metadata_at`,
    // NOT on `poster_path`, and `metadata_at` was stamped whenever a provider
    // answered — including when the *poster download* inside that answer
    // failed. One 429 or one dropped connection therefore marked the item
    // permanently done with a null poster, and nothing in the schema could
    // tell that apart from "TMDB genuinely has no image for this". Both are
    // a null column; only one is worth retrying.
    //
    // Not backfilled: every existing null-poster item is left with a null
    // attempt stamp, which the retry sweep reads as "never tried" and picks
    // up on its next pass. That is deliberate — draining the backlog an
    // upgrade inherits is the point of shipping this.
    "ALTER TABLE items ADD COLUMN artwork_attempted_at INTEGER;
    ALTER TABLE items ADD COLUMN artwork_error TEXT;
    CREATE INDEX idx_items_missing_artwork ON items(artwork_attempted_at)
        WHERE poster_path IS NULL;",
    // v13: what a title *is* — genres, as a JSON array on the item.
    //
    // No server-side genre data has ever existed for catalogue media. TMDB
    // has returned `genres` on every `/movie/{id}` details call plurx has
    // ever made and the field was read straight past; the only `<genre>`
    // handling in the tree folds an NFO's genres into a *home* library's
    // free-form `tags`, which is a different fact wearing the same clothes.
    // So this is new storage, not a migration of anything that exists.
    //
    // A JSON array on `items`, NOT an `item_genres` join table — and the join
    // table is genuinely the faster shape for the query that motivated this,
    // `?genre=` on the library grid. Measured on a seeded catalogue (three
    // genres per title, 18 distinct, a third of the paged library matching):
    // with 6.7k items in the library the filtered first page costs 3.2 ms
    // unfiltered / 7.5 ms via `json_each` / 1.4 ms via an indexed join; at
    // 67k it is 47 / 100 / 25 ms. The join wins because it narrows *before*
    // the sort, where `json_each` roughly doubles a partition scan that was
    // happening anyway. Three reasons that outlast those milliseconds say
    // column regardless:
    //
    //   1. The filter is not the dominant consumer — *displaying* genres is.
    //      Every card, detail page, hub row and search hit wants them, and a
    //      column rides along in `ITEM_COLS` for free and, more to the point,
    //      uniformly. A join table means a per-endpoint batch lookup
    //      (`item_max_heights`'s shape), which is exactly how `resolution`
    //      came to be populated on some responses and quietly absent from
    //      others — and a client cannot tell that apart from "this film has
    //      no genres".
    //   2. It would be the first table in the schema to reference `items` for
    //      *content* rather than state, and v6 is the standing lesson about
    //      what that costs: rebuilding `items` (SQLite cannot alter a CHECK,
    //      and that will happen again) means every child reasoned about under
    //      `foreign_keys = OFF`, plus a rename that rewrites child FK
    //      references. One more child is one more way for that day to eat a
    //      library.
    //   3. `tags` is already a JSON list of strings on this row, seeded from
    //      the same NFO element. Storing the neighbouring fact a second,
    //      different way is a tax every future reader pays.
    //
    // Revisit if a real library makes the filtered page slow. The number to
    // beat is the *unfiltered* page on that same library, not zero: both scan
    // the same partition and sort it in a temp B-tree, so the filter moves
    // the constant, never the class.
    //
    // NOT NULL DEFAULT '[]', like `tags`: "the provider named no genres" and
    // "nobody has asked yet" are deliberately not distinguished here, because
    // `metadata_at` already answers the second one, and a nullable second
    // flavour of empty would be a third state with no reader.
    //
    // Not backfilled by this migration. There is no stored provider payload
    // to recompute from (`tmdb::Match` carries nine fields, none of them
    // genres), so filling these costs one API call per title — work an
    // operator arms deliberately (`genres.backfill`), never something an
    // upgrade starts on its own. v9 records what an upgrade that re-fetches a
    // whole catalogue looks like from TMDB's side.
    "ALTER TABLE items ADD COLUMN genres TEXT NOT NULL DEFAULT '[]';",
    // v14: durable, app-managed offline packages and their renewable transfer
    // capabilities. `file_id` is intentionally not a foreign key: a rescan may
    // replace a file row, but it must not silently delete a queued package,
    // its lease, or the source-unavailable diagnostic the client needs.
    // Source identity is snapshotted for the same reason.
    //
    // One lease per package is load-bearing. AVFoundation cannot resume an
    // asset after its URL changes and Media3 includes child URIs in HLS cache
    // keys. The client creates the random token and retries the same PUT; only
    // its hash reaches this table.
    "CREATE TABLE offline_packages (
        id                 TEXT PRIMARY KEY,
        request_id         TEXT NOT NULL,
        user_id            INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        file_id            INTEGER NOT NULL,
        node_id            TEXT NOT NULL,
        source_path        TEXT NOT NULL,
        source_size        INTEGER NOT NULL,
        source_mtime       INTEGER NOT NULL,
        recipe_hash        TEXT,
        target_height      INTEGER NOT NULL,
        output_width       INTEGER,
        output_height      INTEGER,
        audio_index        INTEGER,
        audio_offset_ms    INTEGER NOT NULL DEFAULT 0,
        subtitle_index     INTEGER,
        subtitle_language  TEXT,
        subtitle_mode      TEXT NOT NULL
                           CHECK (subtitle_mode IN ('none', 'native', 'burned')),
        state              TEXT NOT NULL
                           CHECK (state IN ('queued', 'preparing', 'ready', 'failed')),
        phase              TEXT NOT NULL,
        progress_millis    INTEGER NOT NULL DEFAULT 0,
        estimated_bytes    INTEGER NOT NULL DEFAULT 0,
        reserved_bytes     INTEGER NOT NULL DEFAULT 0,
        actual_bytes       INTEGER,
        duration_ms        INTEGER,
        error_code         TEXT,
        error_message      TEXT,
        created_at         INTEGER NOT NULL DEFAULT (unixepoch()),
        updated_at         INTEGER NOT NULL DEFAULT (unixepoch()),
        last_access_at     INTEGER NOT NULL DEFAULT (unixepoch()),
        expires_at         INTEGER NOT NULL,
        UNIQUE (user_id, request_id)
    ) STRICT;
    CREATE INDEX offline_packages_queue
        ON offline_packages(node_id, state, created_at);
    CREATE INDEX offline_packages_recipe
        ON offline_packages(node_id, recipe_hash, state);
    CREATE INDEX offline_packages_user_state
        ON offline_packages(user_id, state, updated_at);

    CREATE TABLE offline_package_leases (
        token_hash      TEXT PRIMARY KEY,
        package_id      TEXT NOT NULL UNIQUE
                        REFERENCES offline_packages(id) ON DELETE CASCADE,
        created_at      INTEGER NOT NULL DEFAULT (unixepoch()),
        last_access_at  INTEGER NOT NULL DEFAULT (unixepoch()),
        expires_at      INTEGER NOT NULL
    ) STRICT;
    CREATE INDEX offline_package_leases_expiry
        ON offline_package_leases(expires_at);",
    // v15: cluster-safe scan reconciliation. Root identity catches a scanner
    // pointed at the wrong mount; the per-reconciliation guard lets a fixed
    // transaction refuse a stale-but-present mount whose vanished-file set is
    // larger than the configured budget before any delete commits.
    "CREATE TABLE library_roots (
        library_id  INTEGER PRIMARY KEY REFERENCES libraries(id) ON DELETE CASCADE,
        fingerprint TEXT NOT NULL
    ) STRICT;

    CREATE TRIGGER library_roots_paths_au AFTER UPDATE OF paths ON libraries
    WHEN old.paths <> new.paths BEGIN
        DELETE FROM library_roots WHERE library_id = new.id;
    END;

    CREATE TABLE scan_reconcile_guards (
        library_id INTEGER PRIMARY KEY REFERENCES libraries(id) ON DELETE CASCADE
    ) STRICT;

    CREATE TABLE scan_reconcile_items (
        library_id INTEGER NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
        item_id    INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
        PRIMARY KEY (library_id, item_id)
    ) STRICT;",
    // v16: lease watched-outbox delivery across cluster voters. A worker may
    // disappear after reading a pending row; the deadline makes the claim
    // recoverable and also prevents a stale worker from settling a row that a
    // surviving voter already reclaimed.
    "ALTER TABLE watched_outbox ADD COLUMN claim_until INTEGER NOT NULL DEFAULT 0;
    DROP INDEX watched_outbox_due;
    CREATE INDEX watched_outbox_due
        ON watched_outbox(status, next_at, claim_until);",
    // v17: bounded, node-local playback telemetry. No foreign keys on purpose:
    // events outlive users/files, and retention is strictly age-based.
    PLAYBACK_EVENTS_SCHEMA,
    // v18: immutable effective rate-control identity for resumable offline
    // packages. Existing rows were created before quality mode could be
    // durable, so their only truthful value is the legacy VBR recipe.
    // SQLite's CHECK limits the representation family; the exact u8 grammar
    // is validated in Rust at admission and consumption boundaries.
    "ALTER TABLE offline_packages ADD COLUMN effective_rate_control TEXT NOT NULL
        DEFAULT 'vbr'
        CHECK (effective_rate_control = 'vbr'
               OR (effective_rate_control GLOB 'qvbr:[0-9]*'
                   AND substr(effective_rate_control, 6) NOT GLOB '*[^0-9]*'
                   AND length(substr(effective_rate_control, 6)) BETWEEN 1 AND 3
                   AND CAST(substr(effective_rate_control, 6) AS INTEGER) BETWEEN 0 AND 255
                   AND printf('%d', CAST(substr(effective_rate_control, 6) AS INTEGER)) =
                       substr(effective_rate_control, 6)));",
    // v19: opt-in, bounded, node-local network priors. No foreign keys on
    // purpose: the hiqlite backend carries this exact table in its per-voter
    // telemetry sidecar rather than replicating observations through Raft.
    NETWORK_PRIORS_V2_SCHEMA,
    // v20: per-user text-publication state. A locator is bound to one exact
    // file revision because the same chapter href in a replaced edition is
    // not evidence that it names the same text. Progress is millionths on
    // purpose: stable integer ordering/storage with an exact [0,1] wire range.
    "CREATE TABLE reading_state (
        user_id            INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        item_id            INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
        file_id            INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
        file_size          INTEGER NOT NULL,
        file_mtime         INTEGER NOT NULL,
        locator_json       TEXT NOT NULL,
        progression_millis INTEGER NOT NULL
                           CHECK (progression_millis BETWEEN 0 AND 1000000),
        completed          INTEGER NOT NULL DEFAULT 0 CHECK (completed IN (0, 1)),
        updated_at         INTEGER NOT NULL,
        PRIMARY KEY (user_id, item_id, file_id)
    ) STRICT;
    CREATE INDEX reading_state_recent
        ON reading_state(user_id, updated_at DESC);",
    // v21: first-class book facts. The source column is load-bearing: a
    // scheduled EPUB pass is weaker than an explicit Curator handoff and must
    // not overwrite it. Work ids are nullable because title + author is never
    // sufficient evidence that text and audio files are editions of one work.
    "ALTER TABLE items ADD COLUMN author TEXT;
    ALTER TABLE items ADD COLUMN book_work_id TEXT;
    ALTER TABLE items ADD COLUMN book_edition_id TEXT;
    ALTER TABLE items ADD COLUMN book_metadata_source TEXT
        CHECK (book_metadata_source IN ('epub', 'curator'));
    CREATE INDEX idx_items_book_work ON items(book_work_id)
        WHERE book_work_id IS NOT NULL;",
    // v22: isolate N4.2 network priors by credential generation. The old PK
    // used a numeric user_id that survives delete/recreate unchanged, which
    // lets a prior from the old identity contaminate the new one. Replacing
    // it with an opaque SHA-256 credential-generation digest — derived from
    // user.id, user.created_at, and the complete Argon2 PHC password_hash —
    // makes each credential generation a separate prior namespace. Old
    // numeric-key prior rows are dropped because they cannot be translated:
    // the user_id alone is not enough material to recover the credential
    // generation, and any translation scheme would preserve the very
    // cross-generation contamination this migration fixes.
    "DROP TABLE IF EXISTS network_priors;
    CREATE TABLE network_priors (
        user_id               INTEGER NOT NULL,
        credential_generation TEXT NOT NULL,
        client_class          TEXT NOT NULL,
        network_fingerprint   TEXT NOT NULL,
        sustained_kbps        INTEGER,
        worst_rung_height     INTEGER,
        starved_at_ms         INTEGER,
        sample_count          INTEGER NOT NULL DEFAULT 0,
        updated_at_ms         INTEGER NOT NULL,
        PRIMARY KEY (credential_generation, client_class, network_fingerprint)
    ) STRICT;
    CREATE INDEX network_priors_by_updated
        ON network_priors(updated_at_ms, user_id, client_class);",
    // v23: monotone cluster-work leases. Release retains the row so an old
    // fence can never become current again after the logical resource is
    // reacquired.
    "CREATE TABLE job_leases (
        resource       TEXT PRIMARY KEY,
        owner_node_id  TEXT NOT NULL,
        fence          INTEGER NOT NULL CHECK (fence > 0),
        revision       INTEGER NOT NULL CHECK (revision > 0),
        expires_at_ms  INTEGER NOT NULL,
        updated_at_ms  INTEGER NOT NULL
    ) STRICT;",
    // v24: candidate generation stays a singleton, while compatible workers
    // claim distinct whole-title speculative transcodes from this durable
    // queue. The row's own fence is the publication authority; it is not the
    // candidate pass's generic scheduler lease.
    "ALTER TABLE transcode_cache_locations ADD COLUMN manifest_digest TEXT;
    ALTER TABLE transcode_cache_locations
        ADD COLUMN scrub_object_index INTEGER NOT NULL DEFAULT 0;

    CREATE TABLE pretranscode_jobs (
        id                TEXT PRIMARY KEY,
        dedupe_key        TEXT NOT NULL,
        file_id           INTEGER NOT NULL,
        source_size       INTEGER NOT NULL,
        source_mtime      INTEGER NOT NULL,
        target_height     INTEGER NOT NULL,
        policy_generation TEXT NOT NULL,
        requirements_json TEXT NOT NULL,
        reason            TEXT NOT NULL CHECK (
                              reason IN ('in_progress', 'next_up', 'recent')),
        priority          INTEGER NOT NULL,
        state             TEXT NOT NULL CHECK (
                              state IN ('queued', 'running', 'ready', 'failed',
                                        'cancelled')),
        owner_node_id     TEXT,
        staging_node_id   TEXT,
        fence             INTEGER NOT NULL DEFAULT 0 CHECK (fence >= 0),
        lease_expires_ms  INTEGER,
        attempts          INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
        not_before_ms     INTEGER NOT NULL,
        last_error_code   TEXT,
        recipe_hash       TEXT,
        storage_id        TEXT,
        relative_dir      TEXT,
        manifest_digest   TEXT,
        created_at_ms     INTEGER NOT NULL,
        updated_at_ms     INTEGER NOT NULL
    ) STRICT;
    CREATE INDEX pretranscode_jobs_due
        ON pretranscode_jobs(state, not_before_ms, priority DESC, created_at_ms, id);
    CREATE INDEX pretranscode_jobs_dedupe
        ON pretranscode_jobs(dedupe_key, state);
    CREATE INDEX pretranscode_jobs_staging
        ON pretranscode_jobs(staging_node_id, state, id);
    CREATE UNIQUE INDEX pretranscode_jobs_active
        ON pretranscode_jobs(dedupe_key)
        WHERE state IN ('queued', 'running');

    CREATE TRIGGER pretranscode_jobs_cancel_source BEFORE DELETE ON files
    BEGIN
        DELETE FROM pretranscode_jobs
         WHERE file_id = OLD.id AND state IN ('ready', 'failed', 'cancelled');
        UPDATE pretranscode_jobs
           SET state = 'cancelled', owner_node_id = NULL, staging_node_id = NULL,
               lease_expires_ms = NULL, policy_generation = '', requirements_json = '{}'
         WHERE file_id = OLD.id AND state IN ('queued', 'running');
    END;",
    // v25: cluster-wide idempotency and owner routing for live HLS sessions.
    // SQLite uses the same tables so one-voter and replicated behavior cannot
    // diverge at the storage boundary.
    "CREATE TABLE media_session_requests (
        user_id             INTEGER NOT NULL,
        request_id          TEXT NOT NULL CHECK (length(request_id) BETWEEN 1 AND 128),
        request_fingerprint TEXT NOT NULL,
        playback_id         TEXT NOT NULL CHECK (length(playback_id) BETWEEN 1 AND 128),
        state               TEXT NOT NULL CHECK (state IN ('starting', 'resolved', 'failed')),
        claim_expires_at_ms INTEGER NOT NULL,
        incarnation_id      TEXT NOT NULL,
        owner_node_id       TEXT,
        response_json       TEXT,
        updated_at_ms       INTEGER NOT NULL,
        PRIMARY KEY (user_id, request_id)
    ) STRICT;
    CREATE INDEX media_session_requests_expiry
        ON media_session_requests(state, claim_expires_at_ms);

    CREATE TABLE media_playback_pointers (
        user_id                INTEGER NOT NULL,
        playback_id            TEXT NOT NULL CHECK (length(playback_id) BETWEEN 1 AND 128),
        current_incarnation_id TEXT NOT NULL UNIQUE,
        updated_at_ms           INTEGER NOT NULL,
        PRIMARY KEY (user_id, playback_id)
    ) STRICT;

    CREATE TABLE media_sessions (
        incarnation_id                TEXT PRIMARY KEY,
        session_id                    TEXT NOT NULL UNIQUE,
        user_id                       INTEGER NOT NULL,
        playback_id                   TEXT NOT NULL,
        request_fingerprint           TEXT NOT NULL,
        owner_node_id                 TEXT NOT NULL,
        owner_epoch                   INTEGER NOT NULL CHECK (owner_epoch > 0),
        lease_expires_at_ms           INTEGER NOT NULL,
        state                         TEXT NOT NULL CHECK (state IN ('starting', 'active', 'ended')),
        recipe_json                   TEXT NOT NULL,
        response_json                 TEXT NOT NULL,
        produced_playable_through_ms  INTEGER NOT NULL DEFAULT 0,
        fetched_through_ms            INTEGER NOT NULL DEFAULT 0,
        media_origin_ms               INTEGER NOT NULL DEFAULT 0,
        media_sequence                INTEGER NOT NULL DEFAULT 0,
        discontinuity_sequence        INTEGER NOT NULL DEFAULT 0,
        updated_at_ms                 INTEGER NOT NULL
    ) STRICT;
    CREATE INDEX media_sessions_owner
        ON media_sessions(owner_node_id, state, lease_expires_at_ms);
    CREATE INDEX media_sessions_user
        ON media_sessions(user_id, state, lease_expires_at_ms);
    CREATE INDEX media_sessions_expiry
        ON media_sessions(state, lease_expires_at_ms, incarnation_id);
    CREATE INDEX media_sessions_retention
        ON media_sessions(state, updated_at_ms, incarnation_id);",
    // v26: storage-keyed shared-cache generations and distributed readers.
    // Keep the producer-keyed columns and primary key for rolling binaries;
    // new shared rows use `node_id = storage_id`, which old nodes cannot
    // mistake for their local cache.
    "ALTER TABLE transcode_cache_locations
        ADD COLUMN storage_id TEXT NOT NULL DEFAULT '';
    ALTER TABLE transcode_cache_locations
        ADD COLUMN generation_id TEXT NOT NULL DEFAULT '';
    UPDATE transcode_cache_locations
       SET storage_id = 'node:' || node_id || ':cache',
           generation_id = relative_dir
     WHERE storage_id = '';
    CREATE UNIQUE INDEX transcode_cache_storage_generation
        ON transcode_cache_locations(recipe_hash, storage_id, generation_id)
        WHERE storage_id <> '' AND generation_id <> '';
    CREATE INDEX transcode_cache_storage_lru
        ON transcode_cache_locations(storage_id, complete, last_used_at);
    CREATE TRIGGER transcode_cache_location_identity_ai
    AFTER INSERT ON transcode_cache_locations
    WHEN new.storage_id = '' AND new.generation_id = '' BEGIN
        UPDATE transcode_cache_locations
           SET storage_id = 'node:' || new.node_id || ':cache',
               generation_id = new.relative_dir
         WHERE recipe_hash = new.recipe_hash
           AND node_id = new.node_id
           AND storage_class = new.storage_class;
    END;
    CREATE TRIGGER transcode_cache_location_identity_au
    AFTER UPDATE OF relative_dir ON transcode_cache_locations
    WHEN new.storage_class = 'local'
     AND new.storage_id = 'node:' || new.node_id || ':cache'
     AND new.generation_id = old.generation_id
     AND new.relative_dir <> old.relative_dir BEGIN
        UPDATE transcode_cache_locations
           SET generation_id = new.relative_dir
         WHERE recipe_hash = new.recipe_hash
           AND node_id = new.node_id
           AND storage_class = new.storage_class;
    END;

    CREATE TABLE cache_storage_members (
        storage_id          TEXT NOT NULL,
        node_id             TEXT NOT NULL,
        storage_class       TEXT NOT NULL CHECK (storage_class IN ('local', 'shared')),
        verified_at_ms      INTEGER NOT NULL,
        verification_state TEXT NOT NULL CHECK (
            verification_state IN ('verified', 'suspect', 'unverified')),
        PRIMARY KEY (storage_id, node_id)
    ) STRICT;
    CREATE INDEX cache_storage_members_node
        ON cache_storage_members(node_id, verification_state, storage_id);

    CREATE TABLE cache_consumer_pins (
        storage_id       TEXT NOT NULL,
        recipe_hash      TEXT NOT NULL,
        generation_id    TEXT NOT NULL,
        consumer_kind    TEXT NOT NULL CHECK (consumer_kind IN (
            'media_session', 'offline_package', 'offline_download')),
        consumer_id      TEXT NOT NULL,
        consumer_epoch   INTEGER NOT NULL CHECK (consumer_epoch > 0),
        expires_at_ms    INTEGER NOT NULL,
        PRIMARY KEY (
            storage_id, recipe_hash, generation_id, consumer_kind, consumer_id)
    ) STRICT;
    CREATE INDEX cache_consumer_pins_expiry
        ON cache_consumer_pins(storage_id, expires_at_ms);",
    // v27: node-local fragment indexes. No foreign key to `files` on purpose:
    // an index outlives a rescan that renumbers nothing, and its own identity
    // columns already refuse to answer for a file that changed. The hiqlite
    // backend carries this exact table in its per-voter sidecar rather than
    // replicating one machine's ffmpeg output through Raft.
    crate::store::fragindex::FRAGMENT_INDEXES_SCHEMA,
    // v28: node-local rendition plans. Separate from the index above because
    // the two are kept for different reasons: an index is a measurement and
    // may be rebuilt, a plan is a decision a client already holds a playlist
    // for. Node-local for the same reason, and additive, so a v27 database
    // upgrades in place.
    crate::store::renditionplan::RENDITION_PLANS_SCHEMA,
    // v29: the promotion inputs every generation's init is built from, and
    // whether the film's clean fragments agree about them (plan §2.2 ruling).
    // Additive, and `parameter_sets_constant` defaults to 0 so an index built
    // before the check existed is rebuilt rather than trusted.
    crate::store::fragindex::FRAGMENT_INDEXES_PROMOTION_COLUMNS,
    // v30: replicated metadata for content-addressed fragment indexes. The
    // blob itself remains outside this database on clustered deployments.
    crate::store::fragment_index_cluster::CLUSTER_FRAGMENT_INDEX_SCHEMA,
    // v31: durable operator analysis requests. The source hash is deliberately
    // resolved by a leased worker after the HTTP request has committed.
    crate::store::fragment_index_cluster::ANALYSIS_REQUESTS_SCHEMA,
    // v32: bounded terminal-control acknowledgements. These outlive ordinary
    // owner cleanup and route settlement just long enough for an exact End
    // retry, then maintenance removes them independently of route retention.
    "CREATE TABLE media_session_terminal_acks (
        incarnation_id   TEXT NOT NULL UNIQUE,
        session_id       TEXT PRIMARY KEY,
        owner_node_id    TEXT NOT NULL,
        owner_epoch      INTEGER NOT NULL CHECK (owner_epoch > 0),
        client_instance_id TEXT NOT NULL,
        sequence         INTEGER NOT NULL CHECK (sequence > 0),
        request_fingerprint TEXT NOT NULL,
        response_json    TEXT NOT NULL,
        expires_at_ms    INTEGER NOT NULL,
        updated_at_ms    INTEGER NOT NULL
    ) STRICT;
    CREATE INDEX media_session_terminal_acks_expiry
        ON media_session_terminal_acks(expires_at_ms, session_id);",
    // v33: query indexes for the server-paginated analysis history and the
    // bounded summary projection polled by Activity and Settings.
    crate::store::fragment_index_cluster::ANALYSIS_HISTORY_INDEX_SCHEMA,
    // v34: retain the first terminal decision with the authoritative route.
    // Fresh databases also run the append-only v25 -> v36 chain, so the
    // original v25 table definition deliberately remains unchanged.
    "ALTER TABLE media_sessions ADD COLUMN terminal_reason TEXT
        CHECK (terminal_reason IN
          ('deleted', 'superseded', 'admin_stop', 'revoked', 'replaced'));",
    // v35: a replacement is active for lease ownership immediately, but its
    // create replay and media capability stay fenced until the prior owner
    // acknowledges supersession or all previously admitted bodies expire.
    "ALTER TABLE media_sessions ADD COLUMN publication_ready_at_ms INTEGER NOT NULL DEFAULT 0
        CHECK (publication_ready_at_ms >= 0);",
    // v36: route confirmation and the starting request's retry deadline are
    // one atomic database mutation on both Store backends.
    super::MEDIA_SESSION_PUBLICATION_CLAIM_TRIGGER_SCHEMA,
    // v37: one fragment index per (file, copy pipeline) instead of one per
    // file. A Dolby Vision title has two byte streams real clients ask for and
    // the old key could hold only one of them, which is why every preserved-DV
    // session fell through to live-HLS recovery (PLAYBACK-CAPS-V2-PLAN §4.7).
    // A table rebuild, because SQLite cannot re-key in place; rows carry over.
    crate::store::fragindex::FRAGMENT_INDEXES_IDENTITY_KEY,
    // v38: the Dolby Vision configuration record as columns, so nothing has to
    // read a profile number back out of a display label
    // (PLAYBACK-CAPS-V2-PLAN §4.3). Two of them — the enhancement layer and
    // the RPU — the label never carried at all, and they decide whether a
    // Profile 7 disc can be converted to single-layer Profile 8.1.
    //
    // Nullable with no default: a row that predates the backfill and a record
    // that genuinely reported zero must stay distinguishable, or the fallback
    // to the label can never know when to stop.
    super::FILES_DOLBY_VISION_COLUMNS_BATCH,
    // v39: replicated, source-versioned semantic markers. These remain
    // separate from the node-local packed fragment index by design.
    crate::store::timeline_annotations::TIMELINE_ANNOTATIONS_SCHEMA,
    // v40: separately fenced administrator corrections. Automatic set
    // replacement never writes this table, so rebuilds cannot erase them.
    crate::store::timeline_annotations::TIMELINE_MANUAL_OVERRIDES_SCHEMA,
    // v41: make semantic skip-marker work a first-class request component.
    crate::store::fragment_index_cluster::ANALYSIS_COMPONENTS_SCHEMA,
    // v42: a successor that exists without being current. `media_sessions`
    // alone cannot express one — it has no `created_at`, and the only thing
    // that says which of a playback's rows is current is
    // `media_playback_pointers`. A staged row therefore holds no pointer and
    // is invisible to `media_session_route_for_playback` for free; this table
    // is what remembers that it exists, whose successor it is, and when it
    // stops being one.
    //
    // `PRIMARY KEY (user_id, playback_id)` is the "one staged successor per
    // playback" rule, enforced by the schema rather than by a read-then-write.
    // `staged_incarnation_id` is globally unique for the same reason the
    // pointer's `current_incarnation_id` is: an incarnation may be staged by
    // at most one playback.
    //
    // `expected_predecessor_incarnation_id` is stored rather than re-derived
    // at commit. Commit must advance the pointer from the exact predecessor
    // the preparation was made against — a pointer that moved underneath it
    // means a newer player generation exists, and the staged successor must
    // abort rather than reap it.
    super::MEDIA_SESSION_PREPARATIONS_SCHEMA,
    // v43: permanent Profile 7 -> 8.1 conversion ledger. The media pipeline
    // never touches the source before this row reaches `verified`; keeping the
    // audit row under the file foreign key also makes a library deletion clean
    // up bookkeeping without following or deleting the renamed original.
    "CREATE TABLE dv_conversions (
        file_id        INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
        state          TEXT NOT NULL,
        el_type        TEXT,
        original_path  TEXT,
        bytes_before   INTEGER,
        bytes_after    INTEGER,
        error          TEXT,
        queued_at_ms   INTEGER NOT NULL,
        finished_at_ms INTEGER
    ) STRICT;
    CREATE INDEX dv_conversions_queue
        ON dv_conversions(state, queued_at_ms, file_id);",
    // v44: a non-cascading, attempt-identified recovery witness. The link on
    // the cascading conversion row makes a deleted-and-reused integer file id
    // unable to adopt an old guard accidentally; the guard itself survives a
    // confirmed scan deletion until bounded filesystem cleanup completes.
    "ALTER TABLE dv_conversions ADD COLUMN recovery_guard_id TEXT CHECK
         (state != 'committed' OR original_path IS NOT NULL
          OR recovery_guard_id IS NOT NULL);
    CREATE UNIQUE INDEX dv_conversions_recovery_guard
        ON dv_conversions(recovery_guard_id) WHERE recovery_guard_id IS NOT NULL;
    CREATE TABLE dv_recovery_guards (
        guard_id       TEXT PRIMARY KEY,
        file_id        INTEGER NOT NULL,
        library_id     INTEGER NOT NULL,
        source_path    TEXT NOT NULL,
        recovery_path  TEXT NOT NULL UNIQUE,
        state          TEXT NOT NULL CHECK
                         (state IN ('intent','active','guard_removed','scratch_removed')),
        created_at_ms  INTEGER NOT NULL,
        updated_at_ms  INTEGER NOT NULL
    ) STRICT;
    CREATE INDEX dv_recovery_guards_file
        ON dv_recovery_guards(file_id, guard_id);",
    // v45: what the indexer learned when it could NOT build a fragment index.
    // Node-local for `fragment_indexes`' reason, keyed the same way, and
    // invalidated by the same mismatch — the two tables answer one question
    // with opposite signs. Without it a truncated or unsupported build was a
    // log line the next library wrap discarded, so the same whole-file read
    // was spent every pass while `vodserve` answered `vod_index_pending`
    // forever for a title that was never going to have an index.
    crate::store::fragindex::FRAGMENT_INDEX_OUTCOMES_SCHEMA,
    // v46: the code each charged fragment-index attempt ended with, oldest
    // first. `last_error_code` is wiped on every claim and overwritten by the
    // terminal `attempt_limit`, so a row that exhausts its budget says only
    // that it did. The operator text in the Analysis view already promised a
    // history — "resolve the underlying error shown in earlier attempts" —
    // that the row did not keep.
    //
    // This entry was v45 on the effort branch and moved when
    // `FRAGMENT_INDEX_OUTCOMES_SCHEMA` reached `main` first. Position in this
    // list *is* the version, so the two are ordered by which one shipped
    // rather than by which was written first.
    crate::store::fragment_index_cluster::ANALYSIS_ATTEMPT_ERRORS_SCHEMA,
    // v47: which copy-video identity a fragment-index request is for. A file
    // has one to three of them and the queue could only ever ask for one,
    // because the first request became the dedup tombstone for the rest — so
    // a Dolby Vision Profile 7 title got its stripped identity and never its
    // converting one. Empty means whichever identity is next, which is what
    // every row written before this column meant.
    crate::store::fragment_index_cluster::ANALYSIS_REQUEST_IDENTITY_SCHEMA,
    // v48: what a viewer has asked for, per playback. Every other row about a
    // playback records what happened; this is the only one that records what
    // was wanted, and it has to be able to exist before the session that
    // satisfies it — which is why it is its own table rather than columns on
    // `media_playback_pointers`, whose `current_incarnation_id` is NOT NULL.
    crate::store::MEDIA_PLAYBACK_DESIRED_SCHEMA,
    // v49: the revision a pointer write was decided against, and the triggers
    // that refuse a write naming the wrong one — or naming none while an ask
    // exists to name, which is the only shape a binary from before this column
    // can produce. Application SQL fences this binary against a stale ask;
    // nothing fenced a process that was already running when the schema moved,
    // because compatibility is checked at open and never again.
    crate::store::MEDIA_PLAYBACK_POINTER_DESIRED_FENCE_SCHEMA,
    // v50: the terminal analysis-history retention trigger runs inside the
    // durable writer transaction. Once the retained window fills, its
    // correlated newer-generation lookup must be an indexed identity search
    // rather than a scan for every terminal candidate.
    crate::store::fragment_index_cluster::ANALYSIS_TERMINAL_IDENTITY_INDEX_SCHEMA,
    // v51: when a predecessor that is being drained on purpose stops being
    // kept. Null on every row that exists, which is what makes this additive:
    // a binary from before the column neither writes nor reads it, and its
    // sessions behave exactly as they do today.
    crate::store::MEDIA_SESSION_DRAIN_DEADLINE_SCHEMA,
    // v52: the decoder-recovery budget, as a durable ledger rather than a
    // counter in a process. An automatic recovery has to survive the thing it
    // is recovering from — the producer dying, the session ending, the node
    // handing the playback to another node — and an in-memory allowance
    // survives none of those. Keyed by the server-owned epoch so that a new
    // request id, a new client session, or a different node cannot present
    // themselves as a fresh playback and be granted a second attempt.
    super::MEDIA_SESSION_PRODUCER_RECOVERY_SCHEMA,
    // v53: the epoch that names a recovery budget, on the session row that
    // owns it. v52 gave the budget a durable ledger keyed by the epoch and
    // nothing could say what a session's epoch was, so the ledger had a key
    // nobody could present. A deliberate new play mints one; every
    // continuation — reopen, seek, track change, handoff — inherits its
    // predecessor's, which is the whole point: none of them may mint a second
    // allowance. Empty means a session that predates the column, which had no
    // epoch and therefore no budget.
    super::MEDIA_SESSION_RECOVERY_EPOCH_SCHEMA,
    // v54: an offline producer claim is a real incarnation fence, and its
    // one automatic decoder recovery is consumed before fallible alternate
    // planning. The trigger keeps already-running older workers from
    // replenishing or replacing a consumed alternate during a rolling
    // upgrade; new workers additionally fence every write by generation.
    "ALTER TABLE offline_packages ADD COLUMN claim_generation INTEGER NOT NULL DEFAULT 0
        CHECK (claim_generation >= 0);
     ALTER TABLE offline_packages ADD COLUMN decoder_recovery_state TEXT NOT NULL DEFAULT 'primary'
        CHECK (decoder_recovery_state IN
            ('primary', 'recovery_pending', 'rehome_pending', 'alternate'));
     ALTER TABLE offline_packages ADD COLUMN alternate_recipe_hash TEXT;
     ALTER TABLE transcode_cache_locations
        ADD COLUMN publication_generation INTEGER NOT NULL DEFAULT 0
        CHECK (publication_generation >= 0);
     CREATE TRIGGER cache_publication_generation_guard
        BEFORE UPDATE OF complete, bytes, manifest_digest, relative_dir, storage_id,
                         generation_id, publication_generation
        ON transcode_cache_locations
        WHEN NEW.complete = 1
             AND (OLD.complete != 1 OR NEW.bytes != OLD.bytes
                  OR NEW.manifest_digest IS NOT OLD.manifest_digest
                  OR NEW.relative_dir != OLD.relative_dir
                  OR NEW.storage_id != OLD.storage_id
                  OR NEW.generation_id != OLD.generation_id)
             AND NEW.publication_generation != OLD.publication_generation + 1
        BEGIN
            SELECT RAISE(ABORT, 'cache publication requires a current-generation writer');
        END;
     CREATE TRIGGER offline_claim_lifecycle_guard
        BEFORE UPDATE OF state, claim_generation ON offline_packages
        WHEN (OLD.state = 'queued' AND NEW.state = 'preparing'
                AND (NEW.claim_generation != OLD.claim_generation + 1
                     OR COALESCE((SELECT value FROM settings
                                  WHERE key = 'offline.enabled'), '1')
                        IN ('0', 'false', 'off', 'no')))
          OR (OLD.state = 'queued' AND NEW.state = 'ready')
          OR (OLD.state = 'preparing' AND NEW.state IN ('queued', 'ready', 'failed')
                AND NEW.claim_generation != OLD.claim_generation + 1)
          OR (OLD.state IN ('queued', 'preparing') AND NEW.node_id != OLD.node_id
                AND NEW.claim_generation != OLD.claim_generation + 1)
        BEGIN
            SELECT RAISE(ABORT, 'invalid offline claim lifecycle transition');
        END;
     CREATE TRIGGER offline_recovery_guard
        BEFORE UPDATE OF recipe_hash, decoder_recovery_state, alternate_recipe_hash
        ON offline_packages
        WHEN (OLD.decoder_recovery_state = 'primary'
                AND NEW.decoder_recovery_state NOT IN ('primary', 'recovery_pending'))
          OR (OLD.decoder_recovery_state = 'recovery_pending'
                AND NEW.decoder_recovery_state NOT IN
                    ('recovery_pending', 'rehome_pending', 'alternate'))
          OR (OLD.decoder_recovery_state = 'alternate'
                AND NEW.decoder_recovery_state NOT IN ('alternate', 'rehome_pending'))
          OR (OLD.decoder_recovery_state = 'rehome_pending'
                AND NEW.decoder_recovery_state NOT IN ('rehome_pending', 'alternate'))
          OR (OLD.decoder_recovery_state IN ('recovery_pending', 'alternate')
                AND NEW.decoder_recovery_state = 'rehome_pending'
                AND (NEW.node_id = OLD.node_id OR NEW.state != 'queued'
                     OR NEW.recipe_hash IS NOT NULL
                     OR NEW.alternate_recipe_hash IS NOT NULL))
          OR (OLD.alternate_recipe_hash IS NOT NULL
                AND NEW.alternate_recipe_hash IS NOT OLD.alternate_recipe_hash
                AND NOT (OLD.decoder_recovery_state = 'alternate'
                         AND NEW.decoder_recovery_state = 'rehome_pending'
                         AND NEW.node_id != OLD.node_id
                         AND NEW.state = 'queued'
                         AND NEW.recipe_hash IS NULL
                         AND NEW.alternate_recipe_hash IS NULL))
          OR (NEW.decoder_recovery_state = 'primary'
                AND NEW.alternate_recipe_hash IS NOT NULL)
          OR (NEW.decoder_recovery_state IN ('recovery_pending', 'rehome_pending')
                AND (NEW.recipe_hash IS NOT NULL OR NEW.alternate_recipe_hash IS NOT NULL))
          OR (NEW.decoder_recovery_state = 'alternate'
                AND (NEW.alternate_recipe_hash IS NULL
                     OR (NEW.recipe_hash IS NOT NULL
                         AND NEW.recipe_hash != NEW.alternate_recipe_hash)))
        BEGIN
            SELECT RAISE(ABORT, 'invalid offline decoder recovery transition');
        END;",
    // v55: immutable, deterministic rotations made from the existing movie
    // and episode catalogue. Item/file identities are intentionally not
    // foreign keys in generation entries: a rescan may remove their current
    // rows, but it must not rewrite the clock viewers on other devices see.
    crate::library_channels::LIBRARY_CHANNELS_SCHEMA,
    // v56: durable build acknowledgement/state for replay-safe channel saves.
    crate::library_channels::LIBRARY_CHANNEL_BUILD_STATE_SCHEMA,
    // v57: recording rules, the airings they schedule, and reminders. One
    // airing is one row for its whole life, so the unique index covers every
    // state rather than only the live ones.
    crate::dvr::DVR_SCHEMA,
    // v58: resumable subject classification and owner-scoped decisions.
    crate::channel_subjects::SCHEMA,
    // v59: bounded DVR lifecycle history and per-user attention review.
    crate::dvr::DVR_EVENT_SCHEMA,
    // v60: the original video's four-character sample-entry label. This is a
    // source-admission fact, not part of any fragment-index byte identity.
    super::FILES_VIDEO_CODEC_TAG_COLUMN,
    // v61: reusable classification metadata and a derived local text index.
    super::classification::SCHEMA,
    // v62: typed index diagnostics, fixed retry-cycle deadlines, identity-
    // aware active request indexes, and durable one-time repair receipts.
    crate::store::fragment_index_cluster::CONTENT_ANALYSIS_REPAIR_SCHEMA,
    // v63: mirror typed retry disposition and diagnostics in the standalone
    // node-local refusal store.
    crate::store::fragindex::FRAGMENT_INDEX_TYPED_OUTCOMES_SCHEMA,
    // v64: selected-video field order retained as a catalogue fact. Existing
    // rows remain null until the bounded stored-probe backfill considers them.
    super::FILES_FIELD_ORDER_COLUMN,
    // v65: retained source luminance facts for deterministic CPU tone maps.
    // S-07 drafted this as v64; S-08's field-order column reached main first,
    // so the luminance batch appends after it.
    super::FILES_LUMINANCE_COLUMNS_BATCH,
    // v66: publication-time proof consumed by metadata-only detail status.
    // Legacy rows default to unverified and are revalidated in bounded,
    // node-local pages; the packed payload never enters Raft. C-05 drafted
    // this as v64; the field-order and luminance columns reached main first,
    // so the validation column appends after them.
    crate::store::fragindex::FRAGMENT_INDEXES_VALIDATION_COLUMN,
    // v67: durable downloaded captions associated with files.
    super::downloaded_subtitles::SCHEMA,
    // v68: one-file external-reader capabilities. Token hashes are durable;
    // plaintext capability values never enter the database.
    super::FILE_GRANTS_SCHEMA,
    // v69: cluster subtitle-source queue constraints and publication metadata.
    crate::store::fragment_index_cluster::SUBTITLE_SOURCE_SCHEMA,
    // v70: K-05 M5 catalogue read indexes (external-id lookup, top-level
    // items in title order). Indexes only: no row changes, and every
    // statement is `IF NOT EXISTS`.
    super::sql_source::ITEM_READ_INDEXES,
    // v71: common durable background work identities and ownership.
    super::background_jobs::SCHEMA,
    // v72: durable library execution binds the existing catalogue lease.
    super::background_jobs_domain::SCHEMA,
    // v73: durable library requests and their independent results.
    super::background_jobs_library::SCHEMA,
    // v74: named storage domains and atomic provider/storage reservations.
    super::background_jobs_resources::SCHEMA,
    // v75: replicated provider request pacing and cooldowns.
    super::background_jobs_provider::SCHEMA,
    // v76: subtitle extraction executes under common queue ownership.
    concat!(include_str!("../background_jobs_subtitle.sql"), "\n", "UPDATE analysis_requests SET state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL, fence = fence + 1 WHERE component = 'subtitle_source' AND state IN ('running','submitted');"),
    // v77: immutable artwork variants and verified holder publications.
    super::background_jobs_artwork::SCHEMA,
    super::background_jobs_transcode::SCHEMA,
    super::background_jobs_predictions::SCHEMA,
    super::background_jobs_embeddings::SCHEMA,
    super::background_jobs_probe::SCHEMA,
    super::background_jobs_integrity::SCHEMA,
    // Distributed Live TV intents, ingest claims, and capture authority.
    // v83: durable cluster tuner admission, capture claims and fenced output.
    crate::live_tv_resource::SCHEMA,
    // v84: preserve published subtitle history when obsolete common work retires.
    super::background_jobs_subtitle::RECONCILE_SCHEMA,
    // v85: settled job history yields to new work instead of filling the bound.
    super::background_jobs::RETENTION_SCHEMA,
    // v86: settled internal receipts yield too, once the waiter table nears its bound.
    super::background_jobs::RECEIPT_PRESSURE_SCHEMA,
    // v87: expiring viewer interests follow exact analysis into fragment work.
    super::background_jobs::VIEWER_ANALYSIS_SCHEMA,
    // v88: bounded fragment retention probes, including empty result keys.
    super::fragment_index_cluster::ANALYSIS_RESULT_TARGET_FORCE_SCHEMA,
    // v89: file/source-indexed preparation status reads.
    super::background_jobs::PREPARATION_INDEX_SCHEMA,
    // v90: distinguish explicit conversion attempts from automatic discovery.
    super::dv_conversion::DV_REQUEST_PROVENANCE_COLUMN,
    // v91: transactional playback planning settings generation.
    super::PLAYBACK_INPUT_SCHEMA,
    // v92: preserve the resolved audio recipe across offline queue retries.
    OFFLINE_AUDIO_RECIPE_COLUMN,
    // v93: independently attributed completed-transfer Link negatives.
    super::telemetry::NETWORK_PRIOR_LINK_COLUMNS,
    // v94: exact candidate-bound node-local Link samples, never legacy inference.
    super::candidate_link::SCHEMA,
    // v95: copy preparation follows the same exact source cancellation guards.
    super::background_jobs::COPY_OUTPUT_SCHEMA,
    // v96: authenticated candidate failures, independent of network priors.
    super::candidate_recovery::SCHEMA,
    // v97: exact encoded preparation shares source-revision cancellation.
    super::background_jobs::ENCODED_OUTPUT_SCHEMA,
    // v98: permanent Jellyfin wire identities and deterministic retirement.
    super::jellyfin_identity::JELLYFIN_IDENTITY_SCHEMA,
    // v99: compatibility-only login scope and transactional replacement.
    super::jellyfin_login::JELLYFIN_LOGIN_SCHEMA,
    // v100: bounded compatibility negotiations and exact native references.
    super::jellyfin_play::SCHEMA,
    // v101: manual-edit revisions and trusted compatibility own-edit continuation.
    super::jellyfin_watch::SCHEMA,
    // v102: independent quality cancellation, without ending the incumbent.
    // The continuous-quality effort drafted this as v92, then v98; main's
    // v92–v101 reached main first, so it appends after them.
    super::quality_cancellation::QUALITY_CANCELLATION_SCHEMA,
    // v103: parent-fenced continuous media facts and dependency reservations
    // (drafted as v93, then v99).
    super::quality_ledger::SCHEMA,
    // v104: directional sharing authority and sealed outbound material.
    super::sharing::SCHEMA,
    // v105: exact Source/receiver outer-ingress custody; preserved across restore.
    super::sharing_ingress_custody::SCHEMA,
    // v106: additive Cinema receiver credentials, with independent replica marker.
    REMOTE_SCHEMA,
    // v107: separately versioned opt-in invitation consent/admission.
    super::invitations::SCHEMA_V1,
    // v108: durable phone revision and broker transport generation fence.
    super::invitations::MIGRATION_V2,
    // v109: explicit phone login rebind invalidates consent atomically.
    super::invitations::MIGRATION_V3,
    // v110: refuse destructive cleanup of incompatible legacy broker references.
    super::invitations::MIGRATION_V4,
    // v111: managed subtitle jobs and media-independent model reservations.
    super::subtitle_managed_schema::SCHEMA,
    // v112: independently recognized progressive translation operations and cues.
    super::subtitle_translation_schema::SCHEMA_V1,
    // v113: item ratings and per-person item lists (watchlist, favorites).
    super::library_annotations_schema::SCHEMA,
    // v114: source-revision title acquisition, independent of ordinary probes.
    super::dv_title_jobs::SCHEMA,
    // v115: one-time playback handoff codes and their file-scoped tokens.
    super::playback_handoffs::SCHEMA,
    // v116: centrally fenced recomputable text subtitle translation slices.
    super::subtitle_text_translation_schema::SCHEMA,
    // v117: grant-bound read-only progressive caption retention.
    super::shared_caption_schema::SCHEMA,
    // v118: exact progressive v1 producer-incarnation trigger upgrade, preserving cached assets and leases.
    super::subtitle_translation_schema::RETARGET_MIGRATION,
    // v119: explicit playback continuity grants captured by exact range claims.
    super::subtitle_translation_schema::CONTINUITY_MIGRATION,
    // v120: artifact-only text model setup does not reserve media source readers.
    super::subtitle_managed_schema::RESOURCE_MIGRATION,
];

/// Highest SQLite schema version this binary can read and migrate.
///
/// M2's import coordinator checks this before it removes abandoned staging
/// state or writes a backup. Keeping the value derived from the append-only
/// migration list prevents the import gate from drifting away from `open`.
pub const SQLITE_SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// Column list matching [`item_from_row`]. Prefix with a table alias via
/// [`item_cols`].
const ITEM_COLS: &str = "id, library_id, kind, parent_id, title, sort_title, year, overview, \
     tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, \
     poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, \
     artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, \
     book_metadata_source";

/// How many columns [`ITEM_COLS`] selects — the offset of the first column a
/// query appends after it. Keep in step with [`ITEM_COLS`].
///
/// `continue_watching`, `next_up`, `recently_added` and `search_items` all
/// select `ITEM_COLS` and then append their own trailing columns, which they
/// read at `ITEM_COL_COUNT + n`. Adding a column here without bumping this
/// makes all four read the *new* column as if it were their first appended
/// one — no error, no type failure when both are TEXT, just a show title that
/// is quietly a JSON array of genres. Add and bump together, always.
const ITEM_COL_COUNT: usize = 28;

/// `ITEM_COLS` qualified with a table alias (e.g. `i.id, i.library_id, ...`).
fn item_cols(alias: &str) -> String {
    ITEM_COLS
        .split(", ")
        .map(|c| format!("{alias}.{}", c.trim()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn conversion_err(index: usize, message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::other(message)),
    )
}

/// Map a row selected with [`ITEM_COLS`] (starting at column `base`).
fn item_from_row(row: &Row<'_>, base: usize) -> rusqlite::Result<Item> {
    let kind_raw: String = row.get(base + 2)?;
    let kind = ItemKind::parse(&kind_raw)
        .ok_or_else(|| conversion_err(base + 2, format!("unknown item kind `{kind_raw}`")))?;
    let tags_json: String = row.get(base + 19)?;
    let genres_json: String = row.get(base + 23)?;
    Ok(Item {
        id: row.get(base)?,
        library_id: row.get(base + 1)?,
        kind,
        parent_id: row.get(base + 3)?,
        title: row.get(base + 4)?,
        sort_title: row.get(base + 5)?,
        year: row.get(base + 6)?,
        overview: row.get(base + 7)?,
        tmdb_id: row.get(base + 8)?,
        imdb_id: row.get(base + 9)?,
        season_number: row.get(base + 10)?,
        episode_number: row.get(base + 11)?,
        air_date: row.get(base + 12)?,
        runtime_ms: row.get(base + 13)?,
        poster_path: row.get(base + 14)?,
        backdrop_path: row.get(base + 15)?,
        added_at: row.get(base + 16)?,
        updated_at: row.get(base + 17)?,
        recorded_at: row.get(base + 18)?,
        tags: serde_json::from_str(&tags_json)
            .map_err(|e| conversion_err(base + 19, format!("tags: {e}")))?,
        nfo_seeded_at: row.get(base + 20)?,
        artwork_attempted_at: row.get(base + 21)?,
        artwork_error: row.get(base + 22)?,
        genres: serde_json::from_str(&genres_json)
            .map_err(|e| conversion_err(base + 23, format!("genres: {e}")))?,
        author: row.get(base + 24)?,
        book_work_id: row.get(base + 25)?,
        book_edition_id: row.get(base + 26)?,
        book_metadata_source: row.get(base + 27)?,
    })
}

// `probe_json IS NOT NULL` is the fingerprint of a probe that succeeded: a
// failure records the file with `ProbeResult::default()`, leaving the raw JSON
// null and every media column empty. Selected as a column so callers can tell
// "we have no details" from "the details say nothing".
const FILE_COLS: &str = "id, item_id, path, size, mtime, duration_ms, container, video_codec, \
     video_profile, width, height, bit_depth, hdr, bitrate, audio_streams, \
     subtitle_streams, scanned_at, hdr_format, audio_offset_ms, \
     (probe_json IS NOT NULL), \
     dv_profile, dv_level, dv_bl_compat_id, dv_el_present, dv_rpu_present, \
     video_codec_tag, field_order, \
     max_cll, max_fall, mastering_max_luminance, luminance_source, downloaded_subtitles";

fn file_from_row(row: &Row<'_>) -> rusqlite::Result<MediaFile> {
    let path: String = row.get(2)?;
    let audio_json: String = row.get(14)?;
    let subs_json: String = row.get(15)?;
    MediaFile {
        downloaded_subtitles: Vec::new(),
        id: row.get(0)?,
        item_id: row.get(1)?,
        path: path.into(),
        size: row.get(3)?,
        mtime: row.get(4)?,
        duration_ms: row.get(5)?,
        container: row.get(6)?,
        video_codec: row.get(7)?,
        video_codec_tag: row.get(25)?,
        field_order: row.get(26)?,
        video_profile: row.get(8)?,
        width: row.get(9)?,
        height: row.get(10)?,
        bit_depth: row.get(11)?,
        hdr: row.get(12)?,
        bitrate: row.get(13)?,
        audio_streams: serde_json::from_str(&audio_json)
            .map_err(|e| conversion_err(14, format!("audio_streams: {e}")))?,
        subtitle_streams: serde_json::from_str(&subs_json)
            .map_err(|e| conversion_err(15, format!("subtitle_streams: {e}")))?,
        scanned_at: row.get(16)?,
        hdr_format: row.get(17)?,
        audio_offset_ms: row.get(18)?,
        probed: row.get::<_, i64>(19)? != 0,
        dolby_vision: DolbyVisionFacts {
            profile: row.get(20)?,
            level: row.get(21)?,
            bl_compat_id: row.get(22)?,
            el_present: row.get::<_, Option<i64>>(23)?.map(|value| value != 0),
            rpu_present: row.get::<_, Option<i64>>(24)?.map(|value| value != 0),
        },
        max_cll: row.get(27)?,
        max_fall: row.get(28)?,
        mastering_max_luminance: row.get(29)?,
        luminance_source: row.get(30)?,
    }
    .with_downloaded_subtitles(&row.get::<_, String>(31)?)
    .map_err(|e| conversion_err(31, format!("downloaded_subtitles: {e}")))
}

const REMOTE_SCHEMA: &str = r#"CREATE TABLE remote_schema (singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=1)) STRICT;
CREATE TABLE remote_receivers (id TEXT PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, name TEXT NOT NULL, platform TEXT NOT NULL, secret_hash TEXT NOT NULL, created_at INTEGER NOT NULL, revoked_at INTEGER) STRICT;
CREATE TABLE remote_grants (id TEXT PRIMARY KEY, receiver_id TEXT NOT NULL REFERENCES remote_receivers(id) ON DELETE CASCADE, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, name TEXT NOT NULL, secret_hash TEXT NOT NULL, created_at INTEGER NOT NULL, revoked_at INTEGER) STRICT;
CREATE INDEX remote_grants_receiver ON remote_grants(receiver_id, revoked_at);
CREATE TABLE remote_claim_budget (user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE, window_started INTEGER NOT NULL, attempts INTEGER NOT NULL) STRICT;
INSERT INTO remote_schema VALUES(1,1);"#;

const USER_COLS: &str = "id, username, password_hash, is_admin, created_at";

fn user_from_row(row: &Row<'_>) -> rusqlite::Result<User> {
    Ok(User {
        id: row.get(0)?,
        username: row.get(1)?,
        password_hash: row.get(2)?,
        is_admin: row.get::<_, i64>(3)? != 0,
        created_at: row.get(4)?,
    })
}

pub struct SqliteStore {
    conn: Arc<Mutex<Connection>>,
    /// Dedicated read-only connections, for file-backed stores.
    ///
    /// Serializing writes through the one guarded connection is right;
    /// making every read-only settings and metadata lookup queue behind it
    /// is not — with WAL on, readers and the writer are concurrent by
    /// design, and the hottest control paths (flow-control settings, the
    /// per-request file/item lookups) were paying writer-lock latency for
    /// no isolation benefit (review §3.2). `None` for in-memory stores,
    /// where a second connection would be a different, empty database;
    /// their reads take the writer connection exactly as before.
    reads: Option<Arc<ReadPool>>,
    /// Admits one `last_seen_at` refresh per token per activity window in
    /// this process, so a burst of requests from one due session queues one
    /// write on the writer instead of one each (K-05 section 3.2).
    token_activity: Arc<users::TokenActivityGate>,
    /// The database file, so a connection a panic left unusable can be
    /// replaced (`housekeeping::lock_or_recover`). `None` in memory.
    path: Option<Arc<PathBuf>>,
    /// Every settings-bearing read this handle served, in order, as its key
    /// list. Test builds only (the dev-only `fixtures` feature): plurxd's
    /// start-path tests assert how many linearizable settings statements one
    /// Play costs, and a wrapper delegating the whole `Store` surface to count
    /// four methods is not a thing anyone should maintain.
    #[cfg(feature = "fixtures")]
    settings_reads: Arc<Mutex<Vec<Vec<String>>>>,
}

pub use housekeeping::prometheus_sqlite_health;

/// A few read-only connections picked round-robin. Opened READ_ONLY so a
/// routing mistake is a loud error instead of a write sneaking around the
/// writer's serialization.
struct ReadPool {
    conns: Vec<Mutex<Connection>>,
    next: AtomicUsize,
}

/// Two: enough that a settings read and a metadata read can overlap, few
/// enough to be nothing on any box this runs on.
const READ_CONNS: usize = 2;

/// The SQL a hot read is about to run, interpolations resolved.
///
/// K-05 section 3.4 takes every query plan on the statement *as executed*,
/// not on a hand copy of the source text that can drift from it: with
/// `RUST_LOG=plurx_core::store::sqlite=trace` each instrumented method logs
/// its statement name and SQL here, and the `query_plans` example reads the
/// events back to run `EXPLAIN QUERY PLAN` on exactly those strings. The
/// macro evaluates nothing unless TRACE is enabled for this target.
pub(crate) fn trace_statement(statement: &'static str, sql: &str) {
    tracing::trace!(target: "plurx_core::store::sqlite", statement, sql, "sqlite statement");
}

impl SqliteStore {
    /// Open (creating if necessary) the database at `path` and migrate it.
    ///
    /// The file is checked with `quick_check(1)` before anything else touches
    /// it (K-05 section 3.9): a corrupt database refuses to open with its
    /// recovery named, and a check that outlives its budget lets startup
    /// continue and runs the full check in the background.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::open_with_read_connections(path, READ_CONNS)
    }

    /// [`open`](Self::open) with `read_connections` read connections instead
    /// of the default, for the K-05 section 3.3 read-pool bench.
    #[doc(hidden)]
    pub fn open_with_read_connections(
        path: &Path,
        read_connections: usize,
    ) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        housekeeping::boot_integrity_check(&conn, path, housekeeping::BOOT_CHECK_BUDGET)?;
        let mut store = Self::init(conn)?;
        store.path = Some(Arc::new(path.to_owned()));
        // After init: the writer has migrated, so the schema the readers see
        // is the one this binary expects.
        let mut conns = Vec::with_capacity(read_connections.max(1));
        for _ in 0..read_connections.max(1) {
            conns.push(Mutex::new(housekeeping::open_reader(path)?));
        }
        store.reads = Some(Arc::new(ReadPool {
            conns,
            next: AtomicUsize::new(0),
        }));
        Ok(store)
    }

    /// In-memory store for tests.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    /// Count local cache/offline rows still keyed by a legacy instance id.
    /// M0 uses this before accepting a distinct node id so rollback or a
    /// restored database cannot silently strand and later delete those bytes.
    pub(crate) async fn local_ownership_rows(&self, node_id: &str) -> Result<u64, StoreError> {
        let node_id = node_id.to_owned();
        self.with_read(move |conn| {
            let count: i64 = conn.query_row(
                "SELECT \
                    (SELECT COUNT(*) FROM transcode_cache_locations WHERE node_id = ?1) + \
                    (SELECT COUNT(*) FROM offline_packages WHERE node_id = ?1)",
                params![node_id],
                |row| row.get(0),
            )?;
            u64::try_from(count).map_err(|error| StoreError::Task(error.to_string()))
        })
        .await
    }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        housekeeping::configure_writer(&conn)?;
        Self::migrate(&conn)?;
        Self::backfill_hdr_format(&conn)?;
        Ok(SqliteStore {
            conn: Arc::new(Mutex::new(conn)),
            reads: None,
            token_activity: Arc::default(),
            path: None,
            #[cfg(feature = "fixtures")]
            settings_reads: Arc::default(),
        })
    }

    #[cfg(feature = "fixtures")]
    pub(crate) fn note_settings_read(&self, keys: &[&str]) {
        self.settings_reads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(keys.iter().map(|key| (*key).to_owned()).collect());
    }

    #[cfg(not(feature = "fixtures"))]
    #[inline(always)]
    pub(crate) fn note_settings_read(&self, _keys: &[&str]) {}

    /// The settings-bearing reads since the last call, oldest first; each is
    /// the key list of one statement (`["*"]` for a whole-table snapshot).
    #[cfg(feature = "fixtures")]
    pub fn take_settings_reads(&self) -> Vec<Vec<String>> {
        std::mem::take(
            &mut *self
                .settings_reads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// One-time backfill of `hdr_format` from the probe JSON already stored for
    /// each file. The incremental scanner skips unchanged files, so without this
    /// an existing library would never show the new HDR/Dolby-Vision detail
    /// short of a destructive re-add. Gated by a settings flag so it runs once.
    fn backfill_hdr_format(conn: &Connection) -> Result<(), StoreError> {
        const FLAG: &str = "hdr_format_backfilled_v1";
        let done: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![FLAG],
                |r| r.get(0),
            )
            .optional()?;
        if done.is_some() {
            return Ok(());
        }
        let rows: Vec<(i64, String)> = {
            let mut stmt = conn.prepare(
                "SELECT id, probe_json FROM files \
                 WHERE hdr_format IS NULL AND probe_json IS NOT NULL",
            )?;
            let mapped =
                stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
            mapped.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut updated = 0usize;
        for (id, json) in rows {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&json) {
                if let Some(fmt) = crate::scan::probe::parse_probe_json(&value).hdr_format {
                    conn.execute(
                        "UPDATE files SET hdr_format = ?1 WHERE id = ?2",
                        params![fmt, id],
                    )?;
                    updated += 1;
                }
            }
        }
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, '1')",
            params![FLAG],
        )?;
        if updated > 0 {
            tracing::info!(updated, "backfilled HDR detail from stored probe data");
        }
        Ok(())
    }

    /// Whether v46's attempt-history column is already installed.
    fn attempt_errors_column_exists(conn: &Connection) -> Result<bool, StoreError> {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('cluster_fragment_index_jobs')
              WHERE name = 'attempt_errors'",
            [],
            |row| row.get(0),
        )?;
        Ok(count == 1)
    }

    /// Whether v47's request-identity column is already installed.
    fn video_identity_column_exists(conn: &Connection) -> Result<bool, StoreError> {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('analysis_requests')
              WHERE name = 'video_identity'",
            [],
            |row| row.get(0),
        )?;
        Ok(count == 1)
    }

    /// Whether v51's drain deadline column is already installed.
    fn drain_deadline_column_exists(conn: &Connection) -> Result<bool, StoreError> {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('media_sessions')
              WHERE name = 'drain_deadline_ms'",
            [],
            |row| row.get(0),
        )?;
        Ok(count == 1)
    }

    /// Whether v45's negative fragment index is already installed.
    ///
    /// A replay guard is normally about a crash between a migration's commit
    /// and its `user_version` bump. This one is also about a renumbering: the
    /// attempt-history column shipped as v45 on an effort branch and became
    /// v46 when the negative index reached `main` first. A database written
    /// by one of those pre-merge builds sits at v45 with the *column* and
    /// without the *table*, so a guard keyed only on `current` would skip
    /// straight to v46 and leave `fragment_index_outcomes` missing forever —
    /// and every outcome read then errors, which the background pass treats
    /// as a reason to stop building fragment indexes at all. Guarding on the
    /// object rather than on the counter is what makes the version number a
    /// hint instead of a promise.
    fn fragment_index_outcomes_table_exists(conn: &Connection) -> Result<bool, StoreError> {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master
              WHERE type = 'table' AND name = 'fragment_index_outcomes'",
            [],
            |row| row.get(0),
        )?;
        Ok(count == 1)
    }

    fn dv_request_provenance_column_exists(conn: &Connection) -> Result<bool, StoreError> {
        let shape = conn
            .query_row(
                r#"SELECT type, "notnull", dflt_value FROM pragma_table_info('dv_conversions')
               WHERE name = 'requested_manually'"#,
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, bool>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        match shape {
            None => Ok(false),
            Some((kind, true, Some(default))) if kind == "INTEGER" && default == "0" => Ok(true),
            Some(_) => Err(StoreError::Migration(
                "invalid Dolby Vision request provenance column".into(),
            )),
        }
    }

    /// Apply one migration step and stamp its `user_version` in the same
    /// transaction. Dropping the uncommitted transaction on any error rolls
    /// the DDL and the marker back together. A step whose shape a published
    /// build already committed (`already_applied`) only advances the marker.
    fn apply_migration_step(
        conn: &Connection,
        version: i64,
        sql: &str,
        already_applied: bool,
    ) -> Result<(), StoreError> {
        let tx = conn.unchecked_transaction()?;
        if !already_applied {
            if version == super::subtitle_managed_schema::RESOURCE_MIGRATION_VERSION {
                super::subtitle_managed_schema::verify_resource_predecessor(&tx)?;
            }
            tx.execute_batch(sql)
                .map_err(|e| StoreError::Migration(format!("migrating to v{version}: {e}")))?;
        }
        // Foreign keys are OFF for the step (see the caller), so integrity is
        // checked here, before the step can commit.
        let dangling: i64 =
            tx.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })?;
        if dangling > 0 {
            return Err(StoreError::Migration(format!(
                "migrating to v{version} left {dangling} dangling foreign key \
                 reference(s) — refusing to continue"
            )));
        }
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
        Ok(())
    }

    fn migrate(conn: &Connection) -> Result<(), StoreError> {
        let current: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let target = SQLITE_SCHEMA_VERSION;
        if current > target {
            return Err(StoreError::Migration(format!(
                "database schema is v{current}, but this binary only knows v{target} — \
                refusing to open a database from a newer plurx"
            )));
        }
        if current < 104 && conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name IN ('sharing_identity','sharing_source_schema_installation','sharing_source_session_bindings','sharing_ingress_custody'))",
            [], |row| row.get::<_,bool>(0))? {
            return Err(StoreError::Migration("private sharing SQLite lineage collides with published main migrations; refusing ambiguous ordinal interpretation".into()));
        }
        super::schema_lineage::bridge_sqlite(conn, current)?;
        let current: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        // A database written by a pre-merge effort build sits at v45 with the
        // attempt-history column and without the negative fragment index,
        // because those two migrations swapped numbers when they met. Rewind
        // the counter to just below v45 so the loop offers that migration
        // again; its own guard below makes the replay a no-op wherever the
        // table is genuinely there.
        let current = if current == 45 && !Self::fragment_index_outcomes_table_exists(conn)? {
            44
        } else {
            current
        };
        for (index, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
            let version = index as i64 + 1;
            // Foreign keys are off for the duration of a migration. A table
            // rebuild (v6) has to DROP a table its children reference, and
            // `PRAGMA foreign_keys` is a no-op *inside* a transaction — which
            // is where the migration itself runs — so the toggle has to happen
            // out here. Integrity is re-checked below instead of enforced
            // statement by statement.
            conn.pragma_update(None, "foreign_keys", "OFF")?;
            // The DDL, the integrity check and the `user_version` bump commit
            // as one transaction. `PRAGMA user_version` writes the database
            // header through the pager, so it rolls back with the DDL when it
            // shares the transaction (the telemetry sidecar relies on the same
            // property). A crash can therefore never leave a step's shape
            // applied with the marker behind it, which matters because
            // `ADD COLUMN` is not idempotent and the schema-lineage bridge
            // refuses a marker whose objects belong to a later step.
            //
            // Published builds committed the DDL first and bumped the marker
            // separately, so databases they tore in that window still exist;
            // the shape guards below keep those replays a no-op (v41 has
            // carried its guard since it landed).
            let already_applied = (version == 41
                && Self::analysis_component_schema_is_current(conn)?)
                || (version == 45 && Self::fragment_index_outcomes_table_exists(conn)?)
                || (version == 46 && Self::attempt_errors_column_exists(conn)?)
                || (version == 47 && Self::video_identity_column_exists(conn)?)
                || (version == 51 && Self::drain_deadline_column_exists(conn)?)
                || (version == 90 && Self::dv_request_provenance_column_exists(conn)?)
                || (version == 118
                    && super::subtitle_translation_schema::retarget_already_applied(conn)?)
                || (version == 119
                    && super::subtitle_translation_schema::continuity_already_applied(conn)?);
            let applied = Self::apply_migration_step(conn, version, sql, already_applied);
            conn.pragma_update(None, "foreign_keys", "ON")?;
            applied?;
            tracing::info!(version, "applied schema migration");
        }

        super::subtitle_managed_schema::verify_sqlite(conn)?;
        super::subtitle_translation_schema::verify_sqlite(conn)?;
        super::library_annotations_schema::verify_sqlite(conn)?;
        super::resource_settings_schema::install_sqlite(conn)?;
        super::adaptive_resource_attempts::schema::install_sqlite_checked(conn)?;
        super::dv_title_jobs_schema::verify_sqlite(conn)?;
        super::subtitle_text_translation_schema::verify_sqlite(conn)?;
        super::shared_caption_schema::verify_sqlite(conn)?;
        super::playback_handoffs::verify_sqlite(conn)?;

        // First startup: mint the permanent instance id.
        let existing: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![keys::INSTANCE_ID],
                |row| row.get(0),
            )
            .optional()?;
        if existing.is_none() {
            let id = uuid::Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)",
                params![keys::INSTANCE_ID, id],
            )?;
            tracing::info!(instance_id = %id, "generated new instance id");
        }
        Ok(())
    }

    /// Synthetic source-bound old lineage, never a historical capture.
    #[cfg(feature = "hiqlite-contract-tests")]
    #[doc(hidden)]
    pub fn validation_seed_schema_lineage(
        path: &Path,
        marker: i64,
        private: bool,
    ) -> Result<(), StoreError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        if private {
            if !(88..=93).contains(&marker) {
                return Err(StoreError::Migration(
                    "invalid synthetic private SQLite marker".to_owned(),
                ));
            }
            for sql in MIGRATIONS
                .iter()
                .take(87)
                .chain(MIGRATIONS.iter().skip(91).take((marker - 87) as usize))
            {
                conn.execute_batch(sql)?;
            }
        } else {
            if !(88..=91).contains(&marker) {
                return Err(StoreError::Migration(
                    "invalid synthetic published SQLite marker".to_owned(),
                ));
            }
            for sql in MIGRATIONS.iter().take(marker as usize) {
                conn.execute_batch(sql)?;
            }
        }
        conn.pragma_update(None, "user_version", marker)?;
        Ok(())
    }

    /// Detect a fully committed v41 schema whose `user_version` marker was
    /// not advanced before the process stopped. The v41 migration contains
    /// non-idempotent column additions and table rebuilds, so replaying it
    /// would turn a successful commit into a permanent startup failure.
    fn analysis_component_schema_is_current(conn: &Connection) -> Result<bool, StoreError> {
        let installed: i64 = conn.query_row(
            "SELECT
                (SELECT COUNT(*) FROM pragma_table_info('cluster_fragment_index_jobs')
                  WHERE name IN ('priority','trigger','target_node_id'))
              + (SELECT COUNT(*) FROM pragma_table_info('cluster_fragment_index_jobs')
                  WHERE (name = 'cache_key' AND pk = 1)
                     OR (name = 'target_node_id' AND pk = 2))
              + (SELECT COUNT(*) FROM pragma_table_info('timeline_annotation_sets')
                  WHERE name = 'publication_priority')
              + (SELECT COUNT(*) FROM pragma_table_info('analysis_requests')
                  WHERE name IN ('pipeline_version','requested_generation',
                                 'expected_predecessor_generation','priority','trigger',
                                 'cancel_requested'))
              + (SELECT COUNT(*) FROM sqlite_master
                  WHERE type = 'table' AND name IN
                    ('analysis_attempts','cluster_fragment_index_heads',
                     'analysis_lifecycle_counters'))",
            [],
            |row| row.get(0),
        )?;
        Ok(installed == 15)
    }

    async fn with_conn<T, F>(&self, f: F) -> Result<T, StoreError>
    where
        F: FnOnce(&Connection) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let guard = housekeeping::lock_or_recover(
                &conn,
                housekeeping::Pool::Writer,
                path.as_deref().map(PathBuf::as_path),
            )?;
            f(&guard)
        })
        .await
        .map_err(|e| StoreError::Task(e.to_string()))?
    }

    /// Execute a durable job publication only while its exact lease token is
    /// current. The check and caller mutation share one SQLite transaction.
    async fn with_fenced_conn<T, F>(
        &self,
        lease: &Lease,
        replacement: &Lease,
        f: F,
    ) -> Result<T, StoreError>
    where
        F: FnOnce(&Connection) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static,
    {
        let lease = lease.clone();
        let replacement = replacement.clone();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let execution_time_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| {
                    StoreError::Task(format!("system clock precedes unix epoch: {error}"))
                })?
                .as_millis()
                .min(i64::MAX as u128) as i64;
            let replacement_valid = replacement.resource == lease.resource
                && replacement.owner_node_id == lease.owner_node_id
                && replacement.fence == lease.fence
                && replacement.revision == lease.revision.saturating_add(1)
                && replacement.expires_at_unix_ms > lease.expires_at_unix_ms
                && replacement.expires_at_unix_ms > execution_time_ms;
            if !replacement_valid {
                return Err(StoreError::Task(
                    "invalid atomic publication lease replacement".to_owned(),
                ));
            }
            let renewed = tx.execute(
                "UPDATE job_leases
                    SET revision = ?6, expires_at_ms = ?7, updated_at_ms = ?8
                  WHERE resource = ?1 AND owner_node_id = ?2
                    AND fence = ?3 AND revision = ?4
                    AND expires_at_ms = ?5 AND expires_at_ms > ?8
                    AND NOT EXISTS (SELECT 1 FROM background_job_domain_leases binding
                        WHERE binding.resource = job_leases.resource AND binding.domain_fence = job_leases.fence
                        AND NOT EXISTS (SELECT 1 FROM background_jobs job WHERE job.id = binding.job_id
                            AND job.fence = binding.job_fence AND job.owner_node_id = binding.node_id
                            AND job.owner_boot_id = binding.boot_id AND job.claim_id = binding.claim_id
                            AND job.state = 'running' AND job.lease_expires_ms > ?8
            AND NOT EXISTS (SELECT 1 FROM background_job_required_resources required
                WHERE required.job_id = job.id AND NOT EXISTS (
                    SELECT 1 FROM background_job_reservations held
                    WHERE held.job_id = job.id AND held.fence = job.fence
                        AND held.resource_key = required.resource_key
                        AND held.expires_at_ms >= job.lease_expires_ms))
                            AND NOT EXISTS (SELECT 1 FROM settings WHERE key =
                                'internal.cluster_job_owner_removed.' || job.owner_node_id)))",
                params![
                    &lease.resource,
                    &lease.owner_node_id,
                    i64::try_from(lease.fence).map_err(|error| {
                        StoreError::Database(format!("lease fence is out of range: {error}"))
                    })?,
                    i64::try_from(lease.revision).map_err(|error| {
                        StoreError::Database(format!("lease revision is out of range: {error}"))
                    })?,
                    lease.expires_at_unix_ms,
                    i64::try_from(replacement.revision).map_err(|error| {
                        StoreError::Database(format!("lease revision is out of range: {error}"))
                    })?,
                    replacement.expires_at_unix_ms,
                    execution_time_ms,
                ],
            )?;
            if renewed != 1 {
                return Err(StoreError::FenceRejected {
                    resource: lease.resource,
                    owner_node_id: lease.owner_node_id,
                    fence: lease.fence,
                });
            }
            let value = f(&tx)?;
            tx.commit()?;
            Ok(value)
        })
        .await
    }

    /// [`with_read`](Self::with_read) inside one read transaction, for a
    /// closure whose statements must agree with each other.
    ///
    /// A read connection in WAL mode gives each statement its own snapshot
    /// (every implicit transaction starts at the current end mark), so a
    /// count and a page read by two statements can straddle a commit: the
    /// grid says 50 items and shows 51. `BEGIN DEFERRED` takes the snapshot at
    /// the first read and holds it until `COMMIT`, which is what the writer
    /// mutex used to give these closures by excluding every other writer.
    /// The guard rolls back on drop, so an error or a panic inside `f` never
    /// leaves the connection mid-transaction. On an in-memory store this runs
    /// on the writer connection, where the mutex still provides the same.
    async fn with_read_txn<T, F>(&self, f: F) -> Result<T, StoreError>
    where
        F: FnOnce(&Connection) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static,
    {
        self.with_read(move |conn| {
            let snapshot = conn.unchecked_transaction()?;
            let value = f(&snapshot)?;
            snapshot.commit()?;
            Ok(value)
        })
        .await
    }

    /// Like [`with_conn`](Self::with_conn), on a read connection when the
    /// store has them. Only for closures that read: the pool's connections
    /// are opened READ_ONLY, so a write through here fails loudly rather
    /// than dodging the writer's serialization.
    pub(crate) async fn with_read<T, F>(&self, f: F) -> Result<T, StoreError>
    where
        F: FnOnce(&Connection) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static,
    {
        let Some(pool) = self.reads.clone() else {
            return self.with_conn(f).await;
        };
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let idx = pool.next.fetch_add(1, Ordering::Relaxed) % pool.conns.len();
            let guard = housekeeping::lock_or_recover(
                &pool.conns[idx],
                housekeeping::Pool::Read,
                path.as_deref().map(PathBuf::as_path),
            )?;
            f(&guard)
        })
        .await
        .map_err(|e| StoreError::Task(e.to_string()))?
    }
}

#[async_trait]
impl MetricsStore for SqliteStore {
    async fn prometheus_store_snapshot(
        &self,
        node_id: &str,
        now: i64,
    ) -> Result<PrometheusStoreSnapshot, StoreError> {
        let node_id = node_id.to_owned();
        self.with_read(move |conn| {
            conn.query_row(
                "SELECT
                    (SELECT COUNT(*) FROM libraries),
                    (SELECT COUNT(*) FROM users),
                    COALESCE(SUM(state = 'queued'), 0),
                    COALESCE(SUM(state = 'preparing'), 0),
                    COALESCE(SUM(state = 'ready'), 0),
                    COALESCE(SUM(state = 'failed'), 0),
                    COALESCE(SUM(CASE WHEN state = 'queued'
                        THEN COALESCE(actual_bytes, reserved_bytes) ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN state = 'preparing'
                        THEN COALESCE(actual_bytes, reserved_bytes) ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN state = 'ready'
                        THEN COALESCE(actual_bytes, reserved_bytes) ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN state = 'failed'
                        THEN COALESCE(actual_bytes, reserved_bytes) ELSE 0 END), 0),
                    (SELECT COUNT(*) FROM offline_package_leases lease
                     JOIN offline_packages active ON active.id = lease.package_id
                     WHERE active.node_id = ?1 AND active.state = 'ready'
                       AND lease.expires_at > ?2),
                    (SELECT COALESCE(SUM(location.bytes), 0)
                     FROM transcode_cache_locations location
                     WHERE location.node_id = ?1
                       AND location.storage_class = 'local'
                       AND location.complete = 1
                       AND EXISTS (
                           SELECT 1 FROM offline_packages pinned
                           WHERE pinned.node_id = location.node_id
                             AND pinned.recipe_hash = location.recipe_hash
                             AND pinned.state IN ('queued', 'preparing', 'ready')
                       )),
                    (SELECT COALESCE(SUM(status = 'pending'), 0) FROM watched_outbox),
                    (SELECT COALESCE(SUM(status = 'ok'), 0) FROM watched_outbox),
                    (SELECT COALESCE(SUM(status = 'failed'), 0) FROM watched_outbox),
                    (SELECT COALESCE(json_group_array(json_object(
                        'component', grouped.component, 'state', grouped.durable_state,
                        'priority', grouped.priority, 'trigger', grouped.trigger,
                        'count', grouped.depth,
                        'oldest', grouped.oldest_age_seconds)), '[]')
                       FROM (SELECT component,
                              CASE
                                WHEN last_error_code = 'source_superseded' THEN 'stale'
                                WHEN state = 'queued' AND not_before_ms > ?2 THEN 'retry_wait'
                                WHEN state = 'running' THEN 'claimed'
                                WHEN state = 'submitted' THEN 'staged'
                                WHEN state = 'ready' THEN 'published'
                                WHEN state = 'cancelled' THEN 'canceled'
                                ELSE state END AS durable_state,
                              priority, trigger,
                              COUNT(*) AS depth,
                              CASE WHEN ?2 > MIN(created_at_ms)
                                THEN (?2 - MIN(created_at_ms)) / 1000 ELSE 0 END AS oldest_age_seconds
                         FROM (
                           SELECT component, state, last_error_code, not_before_ms,
                                  priority, trigger, created_at_ms
                             FROM analysis_requests
                            WHERE component <> 'fragment_index'
                               OR NOT EXISTS (
                                 SELECT 1 FROM cluster_fragment_index_jobs job
                                  WHERE job.cache_key = analysis_requests.result_cache_key
                                    AND job.target_node_id = analysis_requests.target_node_id)
                           UNION ALL
                           SELECT 'fragment_index', job.state, job.last_error_code,
                                  job.not_before_ms, job.priority, job.trigger, job.created_at_ms
                             FROM cluster_fragment_index_jobs job
                         ) durable_analysis
                        GROUP BY component, durable_state, priority, trigger) grouped),
                    (SELECT COALESCE(json_group_array(json_object(
                        'kind', grouped.kind, 'provenance', grouped.provenance,
                        'confidence', grouped.confidence, 'count', grouped.count)), '[]')
                       FROM (SELECT kind, provenance, confidence, COUNT(*) AS count
                               FROM (SELECT json_extract(marker.value, '$.kind') AS kind,
                                            json_extract(marker.value, '$.provenance') AS provenance,
                                            CASE
                                              WHEN CAST(COALESCE(json_extract(marker.value, '$.confidence_millis'), 0) AS INTEGER) < 500 THEN 'low'
                                              WHEN CAST(COALESCE(json_extract(marker.value, '$.confidence_millis'), 0) AS INTEGER) < 900 THEN 'medium'
                                              ELSE 'high' END AS confidence
                                       FROM timeline_annotation_sets annotation_set,
                                            json_each(annotation_set.annotations_json) marker
                                      JOIN files current_file ON current_file.id = annotation_set.file_id
                                       AND current_file.size = annotation_set.source_size
                                       AND current_file.mtime = annotation_set.source_mtime
                                      WHERE NOT EXISTS (
                                        SELECT 1 FROM timeline_manual_overrides manual
                                         WHERE manual.file_id = annotation_set.file_id
                                           AND manual.kind = json_extract(marker.value, '$.kind')
                                           AND manual.source_size = annotation_set.source_size
                                           AND manual.source_mtime = annotation_set.source_mtime)
                                      UNION ALL
                                     SELECT kind, 'manual', 'high'
                                       FROM timeline_manual_overrides manual
                                       JOIN files current_file ON current_file.id = manual.file_id
                                        AND current_file.size = manual.source_size
                                        AND current_file.mtime = manual.source_mtime)
                              GROUP BY kind, provenance, confidence) grouped),
                    (SELECT COALESCE(json_group_array(json_object(
                        'event', counter.event, 'reason', counter.reason,
                        'count', counter.count)), '[]')
                       FROM analysis_lifecycle_counters counter),
                    -- What the queue has actually produced lately. `?2` is
                    -- Unix seconds here; the jobs table keeps milliseconds.
                    (SELECT COUNT(*) FROM cluster_fragment_index_jobs
                      WHERE state = 'ready' AND updated_at_ms >= (?2 - 86400) * 1000),
                    (SELECT COUNT(*) FROM cluster_fragment_index_jobs
                      WHERE state = 'failed' AND last_error_code = 'attempt_limit'
                        AND updated_at_ms >= (?2 - 86400) * 1000),
                    -- `attempts` is charged on claim, so any row with one was
                    -- picked up. Scoped to this table on purpose: the shared
                    -- `('claim','all')` counter also carries skip-marker work.
                    (SELECT COUNT(*) FROM cluster_fragment_index_jobs
                      WHERE attempts > 0
                        AND updated_at_ms >= (?2 - 86400) * 1000),
                    -- Work the queue could claim right now and has not.
                    (SELECT COUNT(*) FROM cluster_fragment_index_jobs
                      WHERE state = 'queued' AND not_before_ms <= ?2 * 1000),
                    (SELECT COUNT(*) FROM cluster_fragment_index_jobs
                      WHERE state = 'running'
                        AND COALESCE(lease_expires_ms, 0) < ?2 * 1000),
                    (SELECT COALESCE(MAX(updated_at_ms), 0)
                       FROM cluster_fragment_index_jobs WHERE state = 'ready'),
                    (SELECT json_object('jobs', json((SELECT COALESCE(json_group_array(json_object(
                        'kind', grouped.kind, 'state', grouped.state, 'count', grouped.count,
                        'oldest_age_ms', MAX(0, ?2 * 1000 - grouped.created))), '[]')
                        FROM (SELECT kind, state, COUNT(*) AS count, MIN(created_at_ms) AS created
                            FROM background_jobs GROUP BY kind, state LIMIT 128) grouped)),
                        'source_io_reservations', ((SELECT COUNT(*) FROM background_job_reservations
                            WHERE (resource_key = 'source_io' OR resource_key GLOB 'source_io:*') AND expires_at_ms > ?2 * 1000)
                            + (SELECT COUNT(*) FROM analysis_source_reservations
                            WHERE (resource_key = 'source_io' OR resource_key GLOB 'source_io:*') AND expires_at_ms > ?2 * 1000)),
                        'legacy_pending', (SELECT COUNT(*) FROM background_job_legacy WHERE state = 'awaiting_import')))
                 FROM offline_packages WHERE node_id = ?1",
                params![node_id, now],
                |row| {
                    Ok(PrometheusStoreSnapshot {
                        background_jobs: super::background_jobs_observation::background_job_metrics(&row.get::<_, String>(24)?)
                            .map_err(|error| rusqlite::Error::FromSqlConversionFailure(24, rusqlite::types::Type::Text, Box::new(error)))?,
                        libraries: row.get(0)?,
                        users: row.get(1)?,
                        offline: OfflinePackageStats {
                            queued: row.get(2)?,
                            preparing: row.get(3)?,
                            ready: row.get(4)?,
                            failed: row.get(5)?,
                            queued_bytes: row.get(6)?,
                            preparing_bytes: row.get(7)?,
                            ready_bytes: row.get(8)?,
                            failed_bytes: row.get(9)?,
                            active_leases: row.get(10)?,
                            pinned_bytes: row.get(11)?,
                        },
                        watched_outbox: (row.get(12)?, row.get(13)?, row.get(14)?),
                        analysis: super::analysis_store_metrics(
                            &row.get::<_, String>(15)?,
                            &row.get::<_, String>(16)?,
                            &row.get::<_, String>(17)?,
                            super::AnalysisQueueHealth {
                                ready_24h: row.get(18)?,
                                attempt_limit_24h: row.get(19)?,
                                claimed_24h: row.get(20)?,
                                claimable: row.get(21)?,
                                running_past_lease: row.get(22)?,
                                last_ready_at_ms: row.get(23)?,
                            },
                        ),
                    })
                },
            )
            .map_err(StoreError::from)
        })
        .await
    }
}

#[async_trait]
impl SettingsStore for SqliteStore {
    async fn resource_capacity_snapshot(
        &self,
    ) -> Result<Option<crate::resource_settings::CapacitySnapshot>, StoreError> {
        self.with_read_txn(super::resource_settings_schema::sqlite_snapshot)
            .await
    }

    async fn put_resource_capacity(
        &self,
        update: &crate::resource_settings::CapacityUpdate,
    ) -> Result<bool, StoreError> {
        let update = update.clone();
        self.with_conn(move |conn| {
            use super::resource_settings_schema as schema;
            let tx = conn.unchecked_transaction()?;
            let snapshot = schema::sqlite_snapshot(&tx)?.ok_or_else(|| {
                StoreError::Migration("adaptive resource settings schema unavailable".into())
            })?;
            if snapshot.revision != update.expected_revision {
                return Ok(false);
            }
            let writes = update.writes(&snapshot)?;
            let claim = uuid::Uuid::new_v4().to_string();
            let now: i64 = tx.query_row("SELECT unixepoch()", [], |row| row.get(0))?;
            if tx.execute(schema::CLAIM_SQL, params![claim, update.expected_revision])? != 1 {
                return Ok(false);
            }
            for (key, value) in writes {
                if tx.execute(schema::WRITE_SQL, params![key, value, now, claim])? != 1 {
                    return Err(StoreError::Database(
                        "resource settings guarded write did not apply".into(),
                    ));
                }
            }
            if tx.execute(schema::CLEAR_CLAIM_SQL, [claim])? != 1 {
                return Err(StoreError::Database(
                    "resource settings claim did not settle".into(),
                ));
            }
            tx.commit()?;
            Ok(true)
        })
        .await
    }

    async fn ping(&self) -> Result<(), StoreError> {
        self.with_conn(|conn| {
            conn.query_row("SELECT 1", [], |_| Ok(()))?;
            Ok(())
        })
        .await
    }

    async fn get_setting(&self, key: &str) -> Result<Option<String>, StoreError> {
        self.note_settings_read(&[key]);
        let key = key.to_owned();
        self.with_read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT value FROM settings WHERE key = ?1",
                    params![key],
                    |row| row.get(0),
                )
                .optional()?)
        })
        .await
    }

    async fn get_or_init_setting(&self, key: &str, seed: &str) -> Result<String, StoreError> {
        crate::resource_settings::validate_legacy_keys([key])?;
        let key = key.to_owned();
        let seed = seed.to_owned();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, unixepoch()) \
                 ON CONFLICT(key) DO NOTHING",
                params![key, seed],
            )?;
            Ok(conn.query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )?)
        })
        .await
    }

    async fn get_setting_pair(
        &self,
        first: &str,
        second: &str,
    ) -> Result<(Option<String>, Option<String>), StoreError> {
        self.note_settings_read(&[first, second]);
        let first = first.to_owned();
        let second = second.to_owned();
        self.with_read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT key, value FROM settings WHERE key = ?1 OR key = ?2 ORDER BY key",
            )?;
            let rows = stmt.query_map(params![first, second], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut pair = (None, None);
            for row in rows {
                let (key, value) = row?;
                if key == first {
                    pair.0 = Some(value.clone());
                }
                if key == second {
                    pair.1 = Some(value);
                }
            }
            Ok(pair)
        })
        .await
    }

    async fn get_settings(
        &self,
        keys: &[&str],
    ) -> Result<std::collections::BTreeMap<String, String>, StoreError> {
        self.note_settings_read(keys);
        let keys = super::selected_settings_json(keys)?;
        self.with_read(move |conn| {
            // Keep the SQL and its binding in one statement so the placeholder
            // census proves their arity instead of growing the unchecked set.
            conn.prepare("SELECT key, value FROM settings WHERE key IN (SELECT value FROM json_each(?1)) ORDER BY key")?
                .query_map(params![keys], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
                .collect::<Result<_, _>>().map_err(StoreError::from)
        }).await
    }

    async fn local_setting_hint(&self, key: &str) -> Result<Option<String>, StoreError> {
        // One database, so this node's copy is the copy.
        self.get_setting(key).await
    }

    async fn settings_snapshot(
        &self,
    ) -> Result<std::collections::BTreeMap<String, String>, StoreError> {
        self.note_settings_read(&["*"]);
        self.with_read(move |conn| {
            let mut stmt = conn.prepare("SELECT key, value FROM settings ORDER BY key")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut settings = std::collections::BTreeMap::new();
            for row in rows {
                let (key, value) = row?;
                settings.insert(key, value);
            }
            Ok(settings)
        })
        .await
    }

    async fn put_setting(&self, key: &str, value: &str) -> Result<(), StoreError> {
        crate::resource_settings::validate_legacy_keys([key])?;
        let key = key.to_owned();
        let value = value.to_owned();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO settings (key, value, updated_at)
                 VALUES (?1, ?2, unixepoch())
                 ON CONFLICT(key) DO UPDATE
                    SET value = excluded.value, updated_at = unixepoch()",
                params![key, value],
            )?;
            Ok(())
        })
        .await
    }

    async fn put_setting_if_absent(&self, key: &str, value: &str) -> Result<bool, StoreError> {
        crate::resource_settings::validate_legacy_keys([key])?;
        let key = key.to_owned();
        let value = value.to_owned();
        self.with_conn(move |conn| {
            let changed = conn.execute(
                "INSERT INTO settings (key, value, updated_at)
                 VALUES (?1, ?2, unixepoch())
                 ON CONFLICT(key) DO NOTHING",
                params![key, value],
            )?;
            Ok(changed == 1)
        })
        .await
    }

    async fn put_setting_if_absent_if_artwork_repair_current(
        &self,
        key: &str,
        value: &str,
        expected_item_id: i64,
        fence: &ArtworkRepairFence,
    ) -> Result<bool, StoreError> {
        crate::resource_settings::validate_legacy_keys([key])?;
        let key = key.to_owned();
        let value = value.to_owned();
        let fence = fence.clone();
        self.with_conn(move |conn| {
            let changed = conn.execute(
                "INSERT INTO settings (key, value, updated_at)
                 SELECT ?1, ?2, unixepoch()
                 WHERE ?3 = ?4 AND EXISTS (
                   SELECT 1 FROM cluster_artwork_repairs
                   WHERE item_id = ?4 AND owner_node_id = ?5 AND leader_term = ?6
                     AND generation = ?7)
                 ON CONFLICT(key) DO NOTHING",
                params![
                    key,
                    value,
                    expected_item_id,
                    fence.item_id,
                    fence.owner_node_id,
                    fence.leader_term,
                    fence.generation,
                ],
            )?;
            Ok(changed == 1)
        })
        .await
    }

    async fn prune_unreferenced_book_cover_origins(
        &self,
        filename: &str,
    ) -> Result<usize, StoreError> {
        let filename = filename.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "DELETE FROM settings
                  WHERE substr(key, 1, 27) = 'internal.book_cover_origin.'
                    AND json_extract(CASE WHEN json_valid(value) THEN value ELSE '{}' END,
                                     '$.filename') = ?1
                    AND NOT EXISTS (
                        SELECT 1 FROM items WHERE poster_path = ?1 OR backdrop_path = ?1)",
                params![filename],
            )?)
        })
        .await
    }

    async fn put_settings(&self, values: &[(&str, &str)]) -> Result<(), StoreError> {
        crate::resource_settings::validate_legacy_keys(values.iter().map(|(key, _)| *key))?;
        let values = values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<Vec<_>>();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            for (key, value) in values {
                tx.execute(
                    "INSERT INTO settings (key, value, updated_at)
                     VALUES (?1, ?2, unixepoch())
                     ON CONFLICT(key) DO UPDATE
                        SET value = excluded.value, updated_at = unixepoch()",
                    params![key, value],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    async fn put_settings_if_generation(
        &self,
        generation_key: &str,
        expected_generation: i64,
        values: &[(&str, &str)],
    ) -> Result<bool, StoreError> {
        validate_generated_settings(generation_key, expected_generation, values)?;
        let generation_key = generation_key.to_owned();
        let values = values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<Vec<_>>();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let current = tx
                .query_row(
                    "SELECT value FROM settings WHERE key = ?1",
                    params![generation_key],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .map(|value| value.parse::<i64>())
                .transpose()
                .map_err(|error| {
                    rusqlite::Error::InvalidParameterName(format!(
                        "invalid settings generation: {error}"
                    ))
                })?
                .unwrap_or(0);
            if current != expected_generation {
                return Ok(false);
            }
            for (key, value) in values {
                tx.execute(
                    "INSERT INTO settings (key, value, updated_at)
                     VALUES (?1, ?2, unixepoch())
                     ON CONFLICT(key) DO UPDATE
                        SET value = excluded.value, updated_at = unixepoch()",
                    params![key, value],
                )?;
            }
            tx.commit()?;
            Ok(true)
        })
        .await
    }

    async fn instance_id(&self) -> Result<String, StoreError> {
        self.get_setting(keys::INSTANCE_ID).await?.ok_or_else(|| {
            StoreError::Database("instance.id missing — migration invariant broken".to_owned())
        })
    }
}

#[cfg(all(test, feature = "hiqlite-store"))]
pub(crate) use sessions::census_local_principal_fragment;

// SQLite-only: the migration chain fixtures depend on nothing replicated, and
// SQLite-backend tests call them in builds without `hiqlite-store`.
#[cfg(test)]
impl SqliteStore {
    pub(crate) fn apply_next_migration_for_test(conn: &Connection) -> Result<(), StoreError> {
        let current: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let index =
            usize::try_from(current).map_err(|error| StoreError::Migration(error.to_string()))?;
        let sql = MIGRATIONS
            .get(index)
            .ok_or_else(|| StoreError::Migration("no next migration".into()))?;
        Self::apply_migration_step(conn, current + 1, sql, false)
    }

    pub(crate) fn apply_migrations_for_test(
        conn: &Connection,
        through_version: i64,
    ) -> rusqlite::Result<()> {
        let through_version = usize::try_from(through_version)
            .map_err(|error| rusqlite::Error::InvalidParameterName(error.to_string()))?;
        if through_version > MIGRATIONS.len() {
            return Err(rusqlite::Error::InvalidParameterName(
                through_version.to_string(),
            ));
        }
        for sql in MIGRATIONS.iter().take(through_version) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))?;
        }
        conn.pragma_update(None, "user_version", through_version as i64)
    }
}

#[cfg(test)]
mod tests;
