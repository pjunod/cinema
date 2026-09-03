//! Single-node SQLite backend for the [`Store`](super::Store) trait family.
//!
//! rusqlite is synchronous, so all access hops onto the blocking pool via
//! `spawn_blocking` around one mutex-guarded connection. That is plenty for
//! Phase 0–2 write rates (see ARCHITECTURE §2.2); read-heavy paths can grow a
//! read pool later without touching the traits.
//!
//! Implementation is split by domain area: `users`, `library`, `media`,
//! `watch` — this file owns open/migrate, shared row mappers, and settings.

mod apikeys;
mod cache;
mod coordination;
mod dv_conversion;
mod fragindex;
mod fragment_index_cluster;
mod library;
mod media;
mod offline;
mod outbox;
mod pretranscode;
mod publication;
mod reading;
mod sessions;
mod shared_cache;
mod telemetry;
mod timeline_annotations;
mod trakt;
mod users;
mod watch;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension, Row};

use super::{keys, ArtworkRepairFence, MetricsStore, PrometheusStoreSnapshot, SettingsStore};
use crate::cluster::coordination::Lease;
use crate::domain::{DolbyVisionFacts, Item, ItemKind, MediaFile, OfflinePackageStats, User};
use crate::error::StoreError;
use crate::store::telemetry::{NETWORK_PRIORS_V2_SCHEMA, PLAYBACK_EVENTS_SCHEMA};

/// Ordered, append-only migration list. `PRAGMA user_version` tracks the last
/// applied index + 1. Never edit an entry that has shipped — append instead.
const MIGRATIONS: &[&str] = &[
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
     dv_profile, dv_level, dv_bl_compat_id, dv_el_present, dv_rpu_present";

fn file_from_row(row: &Row<'_>) -> rusqlite::Result<MediaFile> {
    let path: String = row.get(2)?;
    let audio_json: String = row.get(14)?;
    let subs_json: String = row.get(15)?;
    Ok(MediaFile {
        id: row.get(0)?,
        item_id: row.get(1)?,
        path: path.into(),
        size: row.get(3)?,
        mtime: row.get(4)?,
        duration_ms: row.get(5)?,
        container: row.get(6)?,
        video_codec: row.get(7)?,
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
    })
}

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
}

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

impl SqliteStore {
    /// Open (creating if necessary) the database at `path` and migrate it.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let mut store = Self::init(Connection::open(path)?)?;
        // After init: the writer has migrated, so the schema the readers see
        // is the one this binary expects.
        let mut conns = Vec::with_capacity(READ_CONNS);
        for _ in 0..READ_CONNS {
            let conn = Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_URI
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            conn.pragma_update(None, "busy_timeout", 5000)?;
            conns.push(Mutex::new(conn));
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
        // WAL for concurrent-reader friendliness on real files; in-memory
        // databases report their own journal mode, which is fine.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::migrate(&conn)?;
        Self::backfill_hdr_format(&conn)?;
        Ok(SqliteStore {
            conn: Arc::new(Mutex::new(conn)),
            reads: None,
        })
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

    /// Whether v45's attempt-history column is already installed.
    fn attempt_errors_column_exists(conn: &Connection) -> Result<bool, StoreError> {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('cluster_fragment_index_jobs')
              WHERE name = 'attempt_errors'",
            [],
            |row| row.get(0),
        )?;
        Ok(count == 1)
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
        for (index, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
            let version = index as i64 + 1;
            // Foreign keys are off for the duration of a migration. A table
            // rebuild (v6) has to DROP a table its children reference, and
            // `PRAGMA foreign_keys` is a no-op *inside* a transaction — which
            // is where the migration itself runs — so the toggle has to happen
            // out here. Integrity is re-checked below instead of enforced
            // statement by statement.
            conn.pragma_update(None, "foreign_keys", "OFF")?;
            // A migration commits its own transaction and only then bumps
            // `user_version`, so a crash in that window leaves the shape
            // applied and the version behind. `ADD COLUMN` is not idempotent,
            // so the replay would fail on a column that is already there —
            // permanently. v41 has carried this guard since it landed.
            let applied = if (version == 41 && Self::analysis_component_schema_is_current(conn)?)
                || (version == 46 && Self::attempt_errors_column_exists(conn)?)
            {
                Ok(())
            } else {
                conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                    .map_err(|e| StoreError::Migration(format!("migrating to v{version}: {e}")))
            };
            conn.pragma_update(None, "foreign_keys", "ON")?;
            applied?;
            let dangling: i64 =
                conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                    row.get(0)
                })?;
            if dangling > 0 {
                return Err(StoreError::Migration(format!(
                    "migrating to v{version} left {dangling} dangling foreign key \
                     reference(s) — refusing to continue"
                )));
            }
            conn.pragma_update(None, "user_version", version)?;
            tracing::info!(version, "applied schema migration");
        }

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
        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| StoreError::Task("sqlite connection mutex poisoned".to_owned()))?;
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
                    AND expires_at_ms = ?5 AND expires_at_ms > ?8",
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
        tokio::task::spawn_blocking(move || {
            let idx = pool.next.fetch_add(1, Ordering::Relaxed) % pool.conns.len();
            let guard = pool.conns[idx]
                .lock()
                .map_err(|_| StoreError::Task("sqlite read mutex poisoned".to_owned()))?;
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
                       FROM cluster_fragment_index_jobs WHERE state = 'ready')
                 FROM offline_packages WHERE node_id = ?1",
                params![node_id, now],
                |row| {
                    Ok(PrometheusStoreSnapshot {
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
    async fn ping(&self) -> Result<(), StoreError> {
        self.with_conn(|conn| {
            conn.query_row("SELECT 1", [], |_| Ok(()))?;
            Ok(())
        })
        .await
    }

    async fn get_setting(&self, key: &str) -> Result<Option<String>, StoreError> {
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

    async fn settings_snapshot(
        &self,
    ) -> Result<std::collections::BTreeMap<String, String>, StoreError> {
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

    async fn instance_id(&self) -> Result<String, StoreError> {
        self.get_setting(keys::INSTANCE_ID).await?.ok_or_else(|| {
            StoreError::Database("instance.id missing — migration invariant broken".to_owned())
        })
    }
}

#[cfg(test)]
#[cfg(feature = "hiqlite-store")]
impl SqliteStore {
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
mod tests {
    use super::*;
    use crate::domain::{MetadataPatch, ReadingStateWrite};
    use crate::store::{
        MediaStore, PlaybackTelemetryStore, ReadingStore, SettingsStore, WatchStore,
    };

    /// The §3.2 pair: a write on the writer connection is visible to the
    /// read pool at once (WAL read-your-writes across connections), and a
    /// busy writer no longer makes readers queue — the read completes while
    /// the writer connection is deliberately held hostage.
    #[tokio::test]
    async fn reads_see_writes_and_do_not_queue_behind_the_writer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(SqliteStore::open(&dir.path().join("plurx.db")).expect("open"));

        // Visibility across connections: put on the writer, get on a reader.
        store.put_setting("k", "v").await.expect("put");
        assert_eq!(
            store.get_setting("k").await.expect("get"),
            Some("v".to_owned()),
            "a committed write is visible to the read pool immediately"
        );

        // Park the writer connection: the closure holds its mutex until
        // released, which is exactly what a slow write or a scan batch does.
        let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = {
            let store = Arc::clone(&store);
            tokio::spawn(async move {
                store
                    .with_conn(move |_conn| {
                        held_tx.send(()).ok();
                        release_rx.recv().ok();
                        Ok(())
                    })
                    .await
            })
        };
        tokio::task::spawn_blocking(move || held_rx.recv())
            .await
            .expect("join")
            .expect("the holder took the writer connection");

        // The read must complete while the writer is held. Generous bound:
        // it passes in microseconds, and only a regression that routes reads
        // back through the writer mutex can spend it.
        tokio::time::timeout(std::time::Duration::from_secs(10), store.get_setting("k"))
            .await
            .expect("a read must not queue behind the writer")
            .expect("get");

        release_tx.send(()).expect("release");
        holder.await.expect("join").expect("holder");
    }

    #[tokio::test]
    async fn settings_roundtrip_and_upsert() {
        let store = SqliteStore::open_in_memory().expect("open");
        assert_eq!(store.get_setting("k").await.expect("get"), None);
        store.put_setting("k", "v1").await.expect("put");
        store.put_setting("k", "v2").await.expect("upsert");
        assert_eq!(
            store.get_setting("k").await.expect("get"),
            Some("v2".to_owned())
        );
    }

    #[tokio::test]
    async fn related_settings_publish_as_one_group() {
        let store = SqliteStore::open_in_memory().expect("open");
        store
            .put_settings(&[
                ("transcode.quality", "22"),
                ("transcode.rate_mode", "quality"),
            ])
            .await
            .expect("put group");
        assert_eq!(
            store.get_setting("transcode.quality").await.expect("get"),
            Some("22".to_owned())
        );
        assert_eq!(
            store.get_setting("transcode.rate_mode").await.expect("get"),
            Some("quality".to_owned())
        );
        assert_eq!(
            store
                .get_setting_pair("transcode.rate_mode", "transcode.quality")
                .await
                .expect("get pair"),
            (Some("quality".to_owned()), Some("22".to_owned()))
        );
    }

    #[tokio::test]
    async fn instance_id_is_a_uuid_and_survives_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("plurx.db");

        let first = {
            let store = SqliteStore::open(&db).expect("open");
            store.instance_id().await.expect("instance id")
        };
        uuid::Uuid::parse_str(&first).expect("instance id is a uuid");

        let store = SqliteStore::open(&db).expect("reopen");
        let second = store.instance_id().await.expect("instance id");
        assert_eq!(
            first, second,
            "instance id must be immutable across restarts"
        );
    }

    /// Build a database at the pre-home-video schema (v5) and fill it with one
    /// row of every shape v6's table rebuild has to carry across.
    fn seed_v5(db: &Path) {
        let conn = Connection::open(db).expect("raw open");
        conn.pragma_update(None, "foreign_keys", "ON").expect("fk");
        for (index, sql) in MIGRATIONS.iter().enumerate().take(5) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|e| panic!("v{}: {e}", index + 1));
        }
        conn.pragma_update(None, "user_version", 5)
            .expect("version");
        conn.execute_batch(
            "INSERT INTO settings (key, value) VALUES ('instance.id', 'fixture-instance');
             INSERT INTO users (id, username, password_hash, is_admin)
                 VALUES (1, 'paul', 'hash', 1);
             INSERT INTO libraries (id, name, kind, paths, anime)
                 VALUES (1, 'Movies', 'movies', '[\"/media/movies\"]', 0),
                        (2, 'TV', 'shows', '[\"/media/tv\"]', 0);
             INSERT INTO items (id, library_id, kind, parent_id, title, sort_title, year, overview)
                 VALUES (10, 1, 'movie', NULL, 'Blade Runner', 'blade runner', 1982, 'Replicants.'),
                        (20, 2, 'show', NULL, 'Severance', 'severance', 2022, NULL),
                        (21, 2, 'season', 20, 'Season 1', 'season 1', NULL, NULL),
                        (22, 2, 'episode', 21, 'Good News', 'good news', NULL, NULL);
             INSERT INTO files (id, item_id, path, size, mtime, container, audio_offset_ms)
                 VALUES (100, 10, '/media/movies/blade-runner.mkv', 42, 7, 'mkv', -250);
             INSERT INTO watch_state (user_id, item_id, position_ms, watched)
                 VALUES (1, 10, 61000, 0);",
        )
        .expect("seed");
    }

    /// v9 turns "has an id" into "a provider has answered". The upgrade must
    /// carry the old meaning across for everything already matched — the
    /// failure mode is silent and expensive: every library in the world
    /// re-fetching itself from TMDB the first time it starts on the new
    /// version, one HTTP call per item, into a rate limit.
    #[tokio::test]
    async fn v9_backfills_already_matched_items_so_no_library_re_enriches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("plurx.db");
        {
            let conn = Connection::open(&db).expect("raw open");
            for (index, sql) in MIGRATIONS.iter().enumerate().take(8) {
                conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                    .unwrap_or_else(|e| panic!("v{}: {e}", index + 1));
            }
            conn.pragma_update(None, "user_version", 8)
                .expect("version");
            conn.execute_batch(
                "INSERT INTO libraries (id, name, kind, paths, anime)
                     VALUES (1, 'Movies', 'movies', '[\"/media/movies\"]', 0);
                 INSERT INTO items (id, library_id, kind, title, sort_title, tmdb_id, updated_at)
                     VALUES (10, 1, 'movie', 'Blade Runner', 'blade runner', 78, 1700000000),
                            (11, 1, 'movie', 'Unmatched', 'unmatched', NULL, 1700000000);",
            )
            .expect("seed v8");
        }

        let store = SqliteStore::open(&db).expect("migrate");
        let needing = store
            .items_needing_metadata(None, false, None)
            .await
            .expect("needing");
        assert_eq!(
            needing.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![11],
            "the matched item is left alone; only the unmatched one is queued"
        );

        let conn = Connection::open(&db).expect("raw reopen");
        let stamp: Option<i64> = conn
            .query_row("SELECT metadata_at FROM items WHERE id = 10", [], |r| {
                r.get(0)
            })
            .expect("stamp");
        assert_eq!(stamp, Some(1700000000), "backfilled from updated_at");
    }

    /// The single most important test in the home-video feature: v6 rebuilds
    /// both `libraries` and `items` (SQLite can't alter a CHECK), and a
    /// mistake there eats a real library.
    #[tokio::test]
    async fn v6_rebuild_preserves_everything() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("plurx.db");
        seed_v5(&db);

        // Open through the real binary path — this applies v6.
        let store = SqliteStore::open(&db).expect("migrate");
        assert_eq!(
            store.instance_id().await.expect("instance id"),
            "fixture-instance",
            "the instance id must survive the rebuild"
        );

        let conn = Connection::open(&db).expect("raw reopen");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("version");
        assert_eq!(version, MIGRATIONS.len() as i64);
        assert_eq!(
            version, 45,
            "a new migration must be a deliberate bump, not a surprise — \
             the list is append-only and every entry is one somebody shipped"
        );
        let files: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'files'",
                [],
                |r| r.get(0),
            )
            .expect("files table");
        // The two spellings of the same migration must not drift. The
        // replicated backend runs one statement per transaction entry and
        // rusqlite refuses a multi-statement `execute`, so the DDL exists as a
        // list and as a batch; the single-node list is append-only, which
        // makes a divergent edit unfixable once shipped.
        assert_eq!(
            super::super::FILES_DOLBY_VISION_COLUMNS
                .iter()
                .map(|statement| format!("{statement};"))
                .collect::<Vec<_>>()
                .join("\n"),
            super::super::FILES_DOLBY_VISION_COLUMNS_BATCH.trim(),
            "the per-statement and batch spellings of the Dolby Vision \
             migration have drifted"
        );
        for column in [
            "dv_profile",
            "dv_level",
            "dv_bl_compat_id",
            "dv_el_present",
            "dv_rpu_present",
        ] {
            assert!(
                files.contains(column),
                "v38 carries the Dolby Vision configuration record as columns \
                 so nothing has to read a profile number back out of a display \
                 label: {files}"
            );
        }
        let fragment_indexes: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master
                  WHERE type = 'table' AND name = 'fragment_indexes'",
                [],
                |r| r.get(0),
            )
            .expect("fragment index table");
        assert!(
            fragment_indexes.contains("PRIMARY KEY (file_id, argv_fingerprint)"),
            "v37 re-keys the table on the copy pipeline as well as the file, \
             or a Dolby Vision title's second identity silently overwrites its \
             first: {fragment_indexes}"
        );

        let preparations: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master
                  WHERE type = 'table' AND name = 'media_session_preparations'",
                [],
                |r| r.get(0),
            )
            .expect("preparation ledger");
        assert!(
            preparations.contains("PRIMARY KEY (user_id, playback_id)"),
            "v42 enforces one staged successor per playback in the schema \
             rather than in a read-then-write, which is the only place a \
             concurrent second prepare cannot slip past: {preparations}"
        );
        assert!(
            preparations.contains("staged_incarnation_id              TEXT NOT NULL UNIQUE"),
            "an incarnation may be staged by at most one playback, for the \
             same reason a pointer's current_incarnation_id is unique: \
             {preparations}"
        );
        assert!(
            preparations.contains("expected_predecessor_incarnation_id TEXT NOT NULL"),
            "the predecessor is recorded at prepare time and never re-derived \
             at commit — a pointer that moved means a newer generation exists \
             and the staged successor must abort rather than reap it: \
             {preparations}"
        );

        // Every row survives, values identical.
        let dangling: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .expect("fk check");
        assert_eq!(dangling, 0, "migration left dangling foreign keys");

        let (name, kind, anime): (String, String, i64) = conn
            .query_row(
                "SELECT name, kind, anime FROM libraries WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("library");
        assert_eq!(
            (name.as_str(), kind.as_str(), anime),
            ("Movies", "movies", 0)
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM libraries", [], |r| r.get::<_, i64>(0))
                .expect("count"),
            2
        );

        let (title, year, overview, parent): (String, i64, String, Option<i64>) = conn
            .query_row(
                "SELECT title, year, overview, parent_id FROM items WHERE id = 10",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("item");
        assert_eq!(
            (title.as_str(), year, overview.as_str(), parent),
            ("Blade Runner", 1982, "Replicants.", None)
        );
        assert_eq!(
            conn.query_row("SELECT parent_id FROM items WHERE id = 22", [], |r| r
                .get::<_, i64>(0))
                .expect("episode parent"),
            21,
            "the show hierarchy must survive"
        );
        // New columns get their defaults, not NULL-vs-missing surprises.
        let (recorded, tags, seeded): (Option<String>, String, Option<i64>) = conn
            .query_row(
                "SELECT recorded_at, tags, nfo_seeded_at FROM items WHERE id = 10",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("new columns");
        assert_eq!((recorded, tags.as_str(), seeded), (None, "[]", None));

        // Files and watch state are untouched (audio_offset_ms included).
        let (path, offset): (String, i64) = conn
            .query_row(
                "SELECT path, audio_offset_ms FROM files WHERE id = 100",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("file");
        assert_eq!(
            (path.as_str(), offset),
            ("/media/movies/blade-runner.mkv", -250)
        );
        assert_eq!(
            conn.query_row(
                "SELECT position_ms FROM watch_state WHERE user_id = 1 AND item_id = 10",
                [],
                |r| r.get::<_, i64>(0)
            )
            .expect("watch state"),
            61000
        );

        // The FTS index was recreated and rebuilt: a pre-migration title is
        // still findable, and searching it doesn't error on the new column.
        let hits = store.search_items("blade", 10).await.expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].item.title, "Blade Runner");

        // And the whole point: the new kinds insert cleanly now that the
        // CHECK constraints are gone.
        conn.execute(
            "INSERT INTO libraries (id, name, kind, paths, anime)
             VALUES (3, 'Home', 'home', '[\"/media/home\"]', 0)",
            [],
        )
        .expect("home library");
        conn.execute(
            "INSERT INTO items (id, library_id, kind, parent_id, title, sort_title)
             VALUES (30, 3, 'folder', NULL, '2019', '2019'),
                    (31, 3, 'video', 30, 'Beach', 'beach'),
                    (32, 3, 'photo', 30, 'IMG_4021', 'img_4021')",
            [],
        )
        .expect("home items");
    }

    #[tokio::test]
    async fn v17_adds_playback_telemetry_without_touching_v16_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("plurx.db");
        {
            let conn = Connection::open(&db).expect("raw open");
            for (index, sql) in MIGRATIONS.iter().enumerate().take(16) {
                conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                    .unwrap_or_else(|e| panic!("v{}: {e}", index + 1));
            }
            conn.pragma_update(None, "user_version", 16)
                .expect("version");
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('migration.proof', 'survives')",
                [],
            )
            .expect("seed v16 row");
        }

        let store = SqliteStore::open(&db).expect("migrate v16 to v17");
        assert_eq!(
            store
                .get_setting("migration.proof")
                .await
                .expect("read proof")
                .as_deref(),
            Some("survives")
        );
        let event_id = store
            .record_playback_event(&crate::domain::PlaybackEvent {
                at_unix_ms: 1_700_000_000_000,
                event: "migration_proof".to_owned(),
                ..crate::domain::PlaybackEvent::default()
            })
            .await
            .expect("write v17 row");
        assert!(event_id > 0);

        let conn = Connection::open(&db).expect("raw reopen");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, SQLITE_SCHEMA_VERSION);
        for index in ["playback_events_by_event", "playback_events_by_file"] {
            let present: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                    [index],
                    |row| row.get(0),
                )
                .expect("index lookup");
            assert_eq!(present, 1, "missing {index}");
        }
    }

    #[test]
    fn v18_backfills_offline_rate_control_without_retargeting_packages() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("plurx.db");
        {
            let conn = Connection::open(&db).expect("raw open");
            for (index, sql) in MIGRATIONS.iter().enumerate().take(17) {
                conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                    .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
            }
            conn.pragma_update(None, "user_version", 17)
                .expect("version");
            conn.execute(
                "INSERT INTO users (id, username, password_hash) VALUES (1, 'paul', 'hash')",
                [],
            )
            .expect("seed user");
            conn.execute(
                "INSERT INTO offline_packages
                    (id, request_id, user_id, file_id, node_id, source_path, source_size,
                     source_mtime, target_height, subtitle_mode, state, phase, expires_at)
                 VALUES ('package', 'request', 1, 9, 'node', '/media/movie.mkv', 1000,
                         7, 720, 'none', 'queued', 'waiting_for_encoder', 10000)",
                [],
            )
            .expect("seed v17 package");
        }

        SqliteStore::open(&db).expect("migrate v17 to v18");
        let conn = Connection::open(&db).expect("raw reopen");
        assert_eq!(
            conn.query_row(
                "SELECT effective_rate_control FROM offline_packages WHERE id = 'package'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("backfilled snapshot"),
            "vbr"
        );
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("version"),
            SQLITE_SCHEMA_VERSION
        );
        assert!(conn
            .execute(
                "UPDATE offline_packages SET effective_rate_control = 'cbr' WHERE id = 'package'",
                [],
            )
            .is_err());
        for invalid in ["qvbr:", "qvbr:21junk", "qvbr:256", "qvbr:021"] {
            assert!(
                conn.execute(
                    "UPDATE offline_packages SET effective_rate_control = ?1 WHERE id = 'package'",
                    [invalid],
                )
                .is_err(),
                "constraint accepted {invalid}"
            );
        }
    }

    #[tokio::test]
    async fn v19_adds_node_local_network_priors_without_touching_v18_rows() {
        use crate::domain::{CredentialGeneration, NetworkPriorObservation};
        use crate::store::NetworkPriorStore;

        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("plurx.db");
        {
            let conn = Connection::open(&db).expect("raw open");
            for (index, sql) in MIGRATIONS.iter().enumerate().take(18) {
                conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                    .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
            }
            conn.pragma_update(None, "user_version", 18)
                .expect("version");
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('migration.proof', 'survives')",
                [],
            )
            .expect("seed v18 row");
        }

        let store = SqliteStore::open(&db).expect("migrate v18 to v19");
        assert_eq!(
            store
                .get_setting("migration.proof")
                .await
                .expect("read proof")
                .as_deref(),
            Some("survives")
        );
        let prior = store
            .observe_network_prior(&NetworkPriorObservation {
                credential_generation: CredentialGeneration::from("v19-test-gen".to_owned()),
                client_class: "chrome".to_owned(),
                network_fingerprint: "192.0.2.0/24".to_owned(),
                throughput_kbps: Some(6_000),
                observed_at_ms: 1_700_000_000_000,
                ..NetworkPriorObservation::default()
            })
            .await
            .expect("write v19 prior");
        assert_eq!(prior.sustained_kbps, Some(6_000));

        let conn = Connection::open(&db).expect("raw reopen");
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("version"),
            SQLITE_SCHEMA_VERSION
        );
        let index: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'network_priors_by_updated'",
                [],
                |row| row.get(0),
            )
            .expect("index lookup");
        assert_eq!(index, 1);
    }

    #[tokio::test]
    async fn v20_adds_revision_bound_reading_state_without_touching_v19_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("plurx.db");
        {
            let conn = Connection::open(&db).expect("raw open");
            for (index, sql) in MIGRATIONS.iter().enumerate().take(19) {
                conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                    .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
            }
            conn.pragma_update(None, "user_version", 19)
                .expect("version");
            conn.execute_batch(
                "INSERT INTO settings (key, value) VALUES ('migration.proof', 'survives');
                 INSERT INTO users (id, username, password_hash, is_admin)
                     VALUES (1, 'reader', 'hash', 0);
                 INSERT INTO libraries (id, name, kind, paths, anime)
                     VALUES (1, 'Books', 'books', '[\"/books\"]', 0);
                 INSERT INTO items (id, library_id, kind, title, sort_title)
                     VALUES (10, 1, 'book', 'Migration Book', 'migration book');
                 INSERT INTO files (id, item_id, path, size, mtime)
                     VALUES (100, 10, '/books/migration.epub', 4096, 77);",
            )
            .expect("seed v19 rows");
        }

        let store = SqliteStore::open(&db).expect("migrate v19 to v20");
        assert_eq!(
            store
                .get_setting("migration.proof")
                .await
                .expect("read proof")
                .as_deref(),
            Some("survives")
        );
        let state = store
            .put_reading_state(
                1,
                10,
                &ReadingStateWrite {
                    file_id: 100,
                    file_size: 4096,
                    file_mtime: 77,
                    locator_json: r#"{"version":1,"href":"chapter.xhtml"}"#.into(),
                    progression_millis: 500_000,
                    completed: false,
                    recorded_at: Some(123),
                },
            )
            .await
            .expect("write v20 reading state");
        assert_eq!(state.progression_millis, 500_000);
        assert_eq!(
            store
                .current_reading_state(1, 10)
                .await
                .expect("read v20 state")
                .expect("state"),
            state
        );

        let conn = Connection::open(&db).expect("raw reopen");
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("version"),
            SQLITE_SCHEMA_VERSION
        );
        assert!(conn
            .execute(
                "UPDATE reading_state SET progression_millis = 1000001 \
                 WHERE user_id = 1 AND item_id = 10 AND file_id = 100",
                [],
            )
            .is_err());
        assert!(conn
            .execute(
                "UPDATE reading_state SET completed = 2 \
                 WHERE user_id = 1 AND item_id = 10 AND file_id = 100",
                [],
            )
            .is_err());
    }

    #[tokio::test]
    async fn v21_adds_book_facts_without_touching_v20_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("plurx.db");
        {
            let conn = Connection::open(&db).expect("raw open");
            for (index, sql) in MIGRATIONS.iter().enumerate().take(20) {
                conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                    .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
            }
            conn.pragma_update(None, "user_version", 20)
                .expect("version");
            conn.execute_batch(
                "INSERT INTO libraries (id, name, kind, paths, anime)
                     VALUES (1, 'Books', 'books', '[\"/books\"]', 0);
                 INSERT INTO items (id, library_id, kind, title, sort_title)
                     VALUES (10, 1, 'book', 'Migration Book', 'migration book');
                 INSERT INTO files (id, item_id, path, size, mtime)
                     VALUES (100, 10, '/books/migration.epub', 4096, 77);",
            )
            .expect("seed v20 rows");
        }

        let store = SqliteStore::open(&db).expect("migrate v20 to current");
        let item = store
            .get_item(10)
            .await
            .expect("read migrated item")
            .expect("migrated item");
        assert_eq!(item.title, "Migration Book");
        assert_eq!(item.author, None);
        assert_eq!(item.book_work_id, None);
        assert_eq!(item.book_edition_id, None);
        assert_eq!(item.book_metadata_source, None);
        assert_eq!(
            store.files_for_item(10).await.expect("read migrated file")[0].path,
            std::path::PathBuf::from("/books/migration.epub")
        );

        let conn = Connection::open(&db).expect("raw reopen");
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("version"),
            SQLITE_SCHEMA_VERSION
        );
        let index: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_items_book_work'",
                [],
                |row| row.get(0),
            )
            .expect("book work index");
        assert_eq!(index, 1);
    }

    #[tokio::test]
    async fn v22_replaces_user_id_key_with_credential_generation() {
        use crate::domain::{CredentialGeneration, NetworkPriorObservation};
        use crate::store::NetworkPriorStore;

        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("plurx.db");
        {
            let conn = Connection::open(&db).expect("raw open");
            for (index, sql) in MIGRATIONS.iter().enumerate().take(20) {
                conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                    .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
            }
            conn.execute_batch(
                "INSERT INTO network_priors
                     (user_id, client_class, network_fingerprint, sample_count, updated_at_ms)
                 VALUES (1, 'chrome', '192.0.2.0/24', 5, 1000);
                 INSERT INTO settings (key, value)
                 VALUES ('migration.proof', 'survives-v21');",
            )
            .expect("seed v20 rows");
            conn.pragma_update(None, "user_version", 21)
                .expect("version");
        }

        let store = SqliteStore::open(&db).expect("migrate v21 to current");
        assert_eq!(
            store
                .get_setting("migration.proof")
                .await
                .expect("read proof")
                .as_deref(),
            Some("survives-v21")
        );
        let prior = store
            .observe_network_prior(&NetworkPriorObservation {
                credential_generation: CredentialGeneration::from("v22-test-gen".to_owned()),
                client_class: "safari".to_owned(),
                network_fingerprint: "10.0.0.0/24".to_owned(),
                throughput_kbps: Some(8_000),
                observed_at_ms: 2_000_000_000_000,
                ..NetworkPriorObservation::default()
            })
            .await
            .expect("write v22 prior");
        assert_eq!(prior.sustained_kbps, Some(8_000));
        assert_eq!(prior.credential_generation.as_str(), "v22-test-gen");

        let conn = Connection::open(&db).expect("raw reopen");
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("version"),
            SQLITE_SCHEMA_VERSION
        );
        let old_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM network_priors", [], |row| row.get(0))
            .expect("count new-format rows");
        assert_eq!(old_rows, 1, "legacy numeric-key rows must be dropped");
    }

    #[tokio::test]
    async fn v23_adds_monotone_job_leases_without_losing_v22_state() {
        use crate::cluster::coordination::LeaseClaim;
        use crate::store::{CoordinationStore, SettingsStore};

        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("plurx.db");
        {
            let conn = Connection::open(&db).expect("raw open");
            for (index, sql) in MIGRATIONS.iter().enumerate().take(22) {
                conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                    .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
            }
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('migration.proof', 'survives-v22')",
                [],
            )
            .expect("seed v22 row");
            conn.pragma_update(None, "user_version", 22)
                .expect("version");
        }

        let store = SqliteStore::open(&db).expect("migrate v22 to v23");
        assert_eq!(
            store
                .get_setting("migration.proof")
                .await
                .expect("read v22 proof")
                .as_deref(),
            Some("survives-v22")
        );
        let lease = store
            .acquire_lease("migration-proof", "node-a", 100, 200)
            .await
            .expect("acquire migrated lease");
        assert!(matches!(lease, LeaseClaim::Acquired(lease) if lease.fence == 1));

        let conn = Connection::open(&db).expect("raw reopen");
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("version"),
            SQLITE_SCHEMA_VERSION
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('job_leases')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("job lease columns"),
            6
        );
    }

    #[tokio::test]
    async fn v24_adds_the_pretranscode_queue_without_losing_v23_state() {
        use crate::store::SettingsStore;

        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("plurx.db");
        {
            let conn = Connection::open(&db).expect("raw open");
            for (index, sql) in MIGRATIONS.iter().enumerate().take(23) {
                conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                    .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
            }
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('migration.proof', 'survives-v23')",
                [],
            )
            .expect("seed v23 row");
            conn.pragma_update(None, "user_version", 23)
                .expect("version");
        }

        let store = SqliteStore::open(&db).expect("migrate v23 to v24");
        assert_eq!(
            store
                .get_setting("migration.proof")
                .await
                .expect("read v23 proof")
                .as_deref(),
            Some("survives-v23")
        );
        let conn = Connection::open(&db).expect("raw reopen");
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("version"),
            SQLITE_SCHEMA_VERSION
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('pretranscode_jobs')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("queue columns"),
            24
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('transcode_cache_locations')
                  WHERE name = 'manifest_digest'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("cache manifest column"),
            1
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('transcode_cache_locations')
                  WHERE name = 'scrub_object_index'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("cache scrub cursor column"),
            1
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master
                  WHERE type = 'trigger' AND name = 'pretranscode_jobs_cancel_source'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("source trigger"),
            1
        );
    }

    #[tokio::test]
    async fn v25_adds_media_session_routing_without_losing_v24_state() {
        use crate::store::SettingsStore;

        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("plurx.db");
        {
            let conn = Connection::open(&db).expect("raw open");
            for (index, sql) in MIGRATIONS.iter().enumerate().take(24) {
                conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                    .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
            }
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('migration.session.proof', 'survives-v24')",
                [],
            )
            .expect("seed v24 row");
            conn.pragma_update(None, "user_version", 24)
                .expect("version");
        }

        let store = SqliteStore::open(&db).expect("migrate v24 to v25");
        assert_eq!(
            store
                .get_setting("migration.session.proof")
                .await
                .expect("read v24 proof")
                .as_deref(),
            Some("survives-v24")
        );
        let conn = Connection::open(&db).expect("raw reopen");
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("version"),
            SQLITE_SCHEMA_VERSION
        );
        for (table, columns) in [
            ("media_session_requests", 10),
            ("media_playback_pointers", 4),
            ("media_sessions", 19),
        ] {
            assert_eq!(
                conn.query_row(
                    &format!("SELECT COUNT(*) FROM pragma_table_info('{table}')"),
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap_or_else(|error| panic!("inspect {table}: {error}")),
                columns,
                "{table} schema"
            );
        }
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index'
                   AND name IN ('media_session_requests_expiry', 'media_sessions_owner',
                                'media_sessions_user', 'media_sessions_expiry',
                                'media_sessions_retention')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("inspect media-session indexes"),
            5
        );
    }

    #[test]
    fn v26_backfills_storage_identity_and_repairs_rolling_legacy_writes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("plurx.db");
        {
            let conn = Connection::open(&db).expect("raw open");
            conn.pragma_update(None, "foreign_keys", "ON").expect("fk");
            for (index, sql) in MIGRATIONS.iter().enumerate().take(25) {
                conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                    .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
            }
            conn.execute_batch(
                "INSERT INTO settings (key, value) VALUES ('instance.id', 'migration-v26');
                 INSERT INTO users (id, username, password_hash, is_admin)
                    VALUES (1, 'migration', 'hash', 1);
                 INSERT INTO libraries (id, name, kind, paths, anime)
                    VALUES (1, 'Migration', 'movies', '[]', 0);
                 INSERT INTO items (id, library_id, kind, title, sort_title)
                    VALUES (1, 1, 'movie', 'Migration', 'migration');
                 INSERT INTO files (id, item_id, path, size, mtime)
                    VALUES (1, 1, '/migration.mkv', 10, 20);
                 INSERT INTO transcode_cache_recipes
                    (recipe_hash, file_id, recipe_version)
                    VALUES ('legacy-recipe', 1, 1);
                 INSERT INTO transcode_cache_locations
                    (recipe_hash, node_id, storage_class, relative_dir, bytes, complete,
                     manifest_digest, scrub_object_index, last_used_at, last_seen_at)
                    VALUES ('legacy-recipe', 'node-a', 'local', 'legacy-generation',
                            100, 1, NULL, 0, 30, 30);",
            )
            .expect("seed exact v25 location");
            conn.pragma_update(None, "user_version", 25)
                .expect("version");
        }

        drop(SqliteStore::open(&db).expect("migrate v25 to v26"));
        let conn = Connection::open(&db).expect("raw reopen");
        assert_eq!(
            conn.query_row(
                "SELECT storage_id, generation_id FROM transcode_cache_locations
                  WHERE recipe_hash = 'legacy-recipe'",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .expect("backfilled identity"),
            (
                "node:node-a:cache".to_owned(),
                "legacy-generation".to_owned()
            )
        );
        conn.execute(
            "INSERT INTO transcode_cache_recipes (recipe_hash, file_id, recipe_version)
             VALUES ('rolling-recipe', 1, 1)",
            [],
        )
        .expect("seed rolling recipe");
        conn.execute(
            "INSERT INTO transcode_cache_locations
                (recipe_hash, node_id, storage_class, relative_dir, bytes, complete,
                 manifest_digest, scrub_object_index, last_used_at, last_seen_at)
             VALUES ('rolling-recipe', 'node-b', 'local', 'rolling-generation',
                     200, 1, NULL, 0, 40, 40)",
            [],
        )
        .expect("legacy binary write on v26 schema");
        assert_eq!(
            conn.query_row(
                "SELECT storage_id, generation_id FROM transcode_cache_locations
                  WHERE recipe_hash = 'rolling-recipe'",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .expect("trigger repaired identity"),
            (
                "node:node-b:cache".to_owned(),
                "rolling-generation".to_owned()
            )
        );
        conn.execute(
            "INSERT INTO transcode_cache_locations
                (recipe_hash, node_id, storage_class, relative_dir, bytes, complete,
                 manifest_digest, scrub_object_index, last_used_at, last_seen_at)
             VALUES ('rolling-recipe', 'node-b', 'local', 'rolling-generation-2',
                     0, 0, NULL, 0, 50, 50)
             ON CONFLICT(recipe_hash, node_id, storage_class) DO UPDATE SET
                relative_dir = excluded.relative_dir,
                bytes = excluded.bytes,
                complete = excluded.complete,
                last_used_at = excluded.last_used_at,
                last_seen_at = excluded.last_seen_at",
            [],
        )
        .expect("legacy binary upsert on v26 schema");
        assert_eq!(
            conn.query_row(
                "SELECT storage_id, generation_id FROM transcode_cache_locations
                  WHERE recipe_hash = 'rolling-recipe'",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .expect("update trigger repaired generation identity"),
            (
                "node:node-b:cache".to_owned(),
                "rolling-generation-2".to_owned(),
            )
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'
                   AND name IN ('cache_storage_members', 'cache_consumer_pins')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("shared cache tables"),
            2
        );
        assert!(conn
            .query_row(
                "SELECT sql FROM sqlite_master
                  WHERE type = 'index' AND name = 'transcode_cache_storage_generation'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("storage generation index")
            .contains("WHERE storage_id <> '' AND generation_id <> ''"));
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger'
                   AND name IN ('transcode_cache_location_identity_ai',
                                'transcode_cache_location_identity_au')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("rolling identity triggers"),
            2
        );
    }

    /// v13 adds a column to `items`, which is the migration shape with a
    /// silent failure mode: `ITEM_COLS` and `ITEM_COL_COUNT` are positional,
    /// and four queries select `ITEM_COLS` and then read their own trailing
    /// columns at `ITEM_COL_COUNT + n`. Get the count wrong and none of them
    /// errors — `search_items` simply starts reporting a JSON array of genres
    /// as the show title, because both are TEXT.
    ///
    /// So this checks the upgrade twice over: the row survives with the new
    /// column defaulted, and every consumer of the positional offsets still
    /// reads what it meant to.
    #[tokio::test]
    async fn v13_adds_genres_without_shifting_the_positional_readers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("plurx.db");
        {
            let conn = Connection::open(&db).expect("raw open");
            for (index, sql) in MIGRATIONS.iter().enumerate().take(12) {
                conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                    .unwrap_or_else(|e| panic!("v{}: {e}", index + 1));
            }
            conn.pragma_update(None, "user_version", 12)
                .expect("version");
            conn.execute_batch(
                "INSERT INTO users (id, username, password_hash, is_admin)
                     VALUES (1, 'paul', 'hash', 1);
                 INSERT INTO libraries (id, name, kind, paths, anime)
                     VALUES (1, 'TV', 'shows', '[\"/media/tv\"]', 0);
                 INSERT INTO items (id, library_id, kind, parent_id, title, sort_title,
                                    tags, metadata_at, poster_path)
                     VALUES (10, 1, 'show', NULL, 'Severance', 'severance', '[\"x\"]', 1, NULL),
                            (11, 1, 'season', 10, 'Season 1', 'season 1', '[]', 1, 'season.jpg'),
                            (12, 1, 'episode', 11, 'Good News', 'good news', '[]', 1, NULL);
                 INSERT INTO files (id, item_id, path, size, mtime)
                     VALUES (100, 12, '/media/tv/s01e01.mkv', 42, 7);
                 INSERT INTO watch_state (user_id, item_id, position_ms, watched)
                     VALUES (1, 12, 61000, 0);",
            )
            .expect("seed v12");
        }

        // Open through the real binary path — this applies v13.
        let store = SqliteStore::open(&db).expect("migrate");
        let conn = Connection::open(&db).expect("raw reopen");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("version");
        assert_eq!(version, MIGRATIONS.len() as i64, "all migrations applied");

        // The new column defaults, and the neighbouring JSON list is untouched.
        let (genres, tags): (String, String) = conn
            .query_row("SELECT genres, tags FROM items WHERE id = 10", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .expect("columns");
        assert_eq!((genres.as_str(), tags.as_str()), ("[]", "[\"x\"]"));

        let item = store.get_item(10).await.expect("get").expect("show");
        assert!(item.genres.is_empty(), "an upgraded row has no genres yet");
        assert_eq!(item.tags, vec!["x".to_owned()]);

        // The four positional readers. Each appends its own columns after
        // `ITEM_COLS`; a stale `ITEM_COL_COUNT` makes them read the genres
        // column instead, with no error to notice.
        store
            .apply_metadata(
                10,
                &MetadataPatch {
                    genres: Some(vec!["Drama".into(), "Mystery".into()]),
                    ..Default::default()
                },
            )
            .await
            .expect("patch genres");

        let recent = store.recently_added(Some(1), 10).await.expect("recent");
        let episode = recent
            .iter()
            .find(|r| r.item.id == 12)
            .expect("the episode is on the rail");
        assert_eq!(
            episode.show_title.as_deref(),
            Some("Severance"),
            "recently_added must read the show title, not the genres column"
        );
        assert_eq!(episode.season_poster.as_deref(), Some("season.jpg"));

        let hits = store.search_items("severance", 10).await.expect("search");
        assert!(!hits.is_empty(), "the show is still findable");
        assert!(
            hits.iter()
                .all(|h| h.show_title.is_none() || h.show_title.as_deref() == Some("Severance")),
            "search_items must read the show title, not the genres column: {:?}",
            hits.iter()
                .map(|h| h.show_title.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            hits.iter()
                .find(|h| h.item.id == 10)
                .map(|h| h.item.genres.clone()),
            Some(vec!["Drama".to_owned(), "Mystery".to_owned()]),
            "and it must read the genres themselves as genres"
        );

        let in_progress = store.continue_watching(1, 10).await.expect("continue");
        let entry = in_progress
            .iter()
            .find(|r| r.item.id == 12)
            .expect("the part-watched episode");
        assert_eq!(
            entry.show_title.as_deref(),
            Some("Severance"),
            "continue_watching must read the show title, not the genres column"
        );
        assert_eq!(
            entry.state.position_ms, 61000,
            "and its watch state after that"
        );
        assert_eq!(entry.season_poster.as_deref(), Some("season.jpg"));

        let next = store.next_up(1, 10).await.expect("next up");
        for r in &next {
            assert!(
                r.show_title.is_none() || r.show_title.as_deref() == Some("Severance"),
                "next_up must read the show title, not the genres column: {:?}",
                r.show_title
            );
        }
    }

    #[tokio::test]
    async fn v14_offline_packages_do_not_cascade_with_rescanned_files() {
        let store = SqliteStore::open_in_memory().expect("store");
        let conn = store.conn.lock().expect("connection");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, MIGRATIONS.len() as i64);
        assert!(
            version >= 14,
            "the offline-package migration must be present"
        );

        let file_foreign_keys: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_foreign_key_list('offline_packages') \
                 WHERE \"table\" = 'files'",
                [],
                |row| row.get(0),
            )
            .expect("foreign keys");
        assert_eq!(
            file_foreign_keys, 0,
            "a rescan must not cascade-delete package state"
        );
        let package_id_unique: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_index_list('offline_package_leases') \
                 WHERE \"unique\" = 1",
                [],
                |row| row.get(0),
            )
            .expect("lease indexes");
        assert!(package_id_unique > 0, "one stable lease per package");
    }

    #[tokio::test]
    async fn refuses_databases_from_the_future() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("plurx.db");
        SqliteStore::open(&db).expect("create");

        {
            let conn = Connection::open(&db).expect("raw open");
            conn.pragma_update(None, "user_version", 9999)
                .expect("bump");
        }

        match SqliteStore::open(&db).map(|_| ()) {
            Err(StoreError::Migration(msg)) => assert!(msg.contains("newer")),
            other => panic!("expected migration error, got {other:?}"),
        }
    }

    #[test]
    fn v31_adds_analysis_requests_to_a_real_v30_shape() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("plurx.db");
        {
            let conn = Connection::open(&db).expect("raw open");
            for (index, sql) in MIGRATIONS.iter().enumerate().take(30) {
                conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                    .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
            }
            conn.pragma_update(None, "user_version", 30)
                .expect("v30 marker");
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                      WHERE type = 'table' AND name = 'analysis_requests'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("v30 shape"),
                0,
                "the historical v30 fixture must not contain the v31 table"
            );
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('migration.proof', 'survives')",
                [],
            )
            .expect("seed v30 state");
        }

        SqliteStore::open(&db).expect("migrate v30 to v31");
        let conn = Connection::open(&db).expect("raw reopen");
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("version"),
            SQLITE_SCHEMA_VERSION
        );
        assert_eq!(
            conn.query_row(
                "SELECT value FROM settings WHERE key = 'migration.proof'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("migration proof"),
            "survives"
        );
        for object in [
            "analysis_requests",
            "analysis_requests_due",
            "analysis_requests_status",
            "analysis_requests_one_active_source",
            "analysis_requests_cancel_source",
            "analysis_requests_supersede_source",
            "analysis_requests_bound_terminal_history",
        ] {
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                    [object],
                    |row| row.get::<_, i64>(0),
                )
                .expect("schema object"),
                1,
                "missing v31 schema object {object}"
            );
        }
    }

    #[test]
    fn v32_adds_terminal_control_acknowledgements_without_losing_v31_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("plurx.db");
        {
            let conn = Connection::open(&db).expect("raw open");
            for (index, sql) in MIGRATIONS.iter().enumerate().take(31) {
                conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                    .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
            }
            conn.pragma_update(None, "user_version", 31)
                .expect("v31 marker");
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('migration.proof', 'survives-v31')",
                [],
            )
            .expect("seed v31 state");
        }

        SqliteStore::open(&db).expect("migrate v31 to v32");
        let conn = Connection::open(&db).expect("raw reopen");
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("version"),
            SQLITE_SCHEMA_VERSION
        );
        assert_eq!(
            conn.query_row(
                "SELECT value FROM settings WHERE key = 'migration.proof'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("migration proof"),
            "survives-v31"
        );
        for object in [
            "media_session_terminal_acks",
            "media_session_terminal_acks_expiry",
        ] {
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                    [object],
                    |row| row.get::<_, i64>(0),
                )
                .expect("schema object"),
                1,
                "missing v32 schema object {object}"
            );
        }
    }

    #[test]
    fn v39_to_v41_add_annotations_and_widen_analysis_without_losing_requests() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("plurx.db");
        {
            let conn = Connection::open(&db).expect("raw open");
            for (index, sql) in MIGRATIONS.iter().enumerate().take(38) {
                conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                    .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
            }
            conn.pragma_update(None, "user_version", 38)
                .expect("v38 marker");
            conn.execute(
                "INSERT INTO analysis_requests
                    (request_id, file_id, source_size, source_mtime, component,
                     force_rebuild, target_node_id, state, not_before_ms,
                     created_at_ms, updated_at_ms)
                 VALUES ('preserved-v38', 7, 100, 10, 'fragment_index', 0,
                         'node-a', 'queued', 20, 20, 20)",
                [],
            )
            .expect("seed v38 analysis request");
        }

        SqliteStore::open(&db).expect("migrate v38 through annotations and component widening");
        let conn = Connection::open(&db).expect("raw reopen");
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("version"),
            SQLITE_SCHEMA_VERSION
        );
        assert_eq!(
            conn.query_row(
                "SELECT component FROM analysis_requests WHERE request_id = 'preserved-v38'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("preserved request"),
            "fragment_index"
        );
        conn.execute(
            "INSERT INTO analysis_requests
                (request_id, file_id, source_size, source_mtime, component,
                 force_rebuild, target_node_id, state, not_before_ms,
                 created_at_ms, updated_at_ms)
             VALUES ('semantic-v41', 8, 100, 10, 'skip_markers', 0,
                     'node-a', 'queued', 21, 21, 21)",
            [],
        )
        .expect("widened component accepts semantic work");
        for table in [
            "timeline_annotation_sets",
            "timeline_manual_overrides",
            "analysis_lifecycle_counters",
        ] {
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get::<_, i64>(0),
                )
                .expect("timeline table"),
                1,
                "missing {table}"
            );
        }
    }

    #[test]
    fn v41_schema_commit_with_a_stale_marker_recovers_without_replaying_ddl() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("plurx.db");
        {
            let conn = Connection::open(&db).expect("raw open");
            for (index, sql) in MIGRATIONS.iter().enumerate().take(41) {
                conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                    .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
            }
            conn.pragma_update(None, "user_version", 40)
                .expect("simulate interruption after the v41 schema commit");
        }

        SqliteStore::open(&db).expect("settle the committed v41 migration");
        let conn = Connection::open(&db).expect("raw reopen");
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("version"),
            SQLITE_SCHEMA_VERSION
        );
        assert!(SqliteStore::analysis_component_schema_is_current(&conn)
            .expect("inspect current analysis schema"));
    }

    #[test]
    fn v32_upgrades_through_analysis_indexes_terminal_reason_and_publication_fence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("plurx.db");
        {
            let conn = Connection::open(&db).expect("raw open");
            for (index, sql) in MIGRATIONS.iter().enumerate().take(32) {
                conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                    .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
            }
            conn.pragma_update(None, "user_version", 32)
                .expect("v32 marker");
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('migration.proof', 'survives-v32')",
                [],
            )
            .expect("seed v32 state");
        }

        SqliteStore::open(&db).expect("migrate v32 to current");
        let conn = Connection::open(&db).expect("raw reopen");
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("version"),
            SQLITE_SCHEMA_VERSION
        );
        assert_eq!(
            conn.query_row(
                "SELECT value FROM settings WHERE key = 'migration.proof'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("migration proof"),
            "survives-v32"
        );
        for object in [
            "analysis_requests_result_history",
            "cluster_fragment_index_jobs_status_history",
        ] {
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                    [object],
                    |row| row.get::<_, i64>(0),
                )
                .expect("schema object"),
                1,
                "missing v33 schema object {object}"
            );
        }
        let mut plan = conn
            .prepare(
                "EXPLAIN QUERY PLAN
                 SELECT cache_key FROM (
                   SELECT cache_key, updated_at_ms FROM (
                     SELECT cache_key, updated_at_ms FROM cluster_fragment_index_jobs
                      WHERE state = 'ready'
                      ORDER BY updated_at_ms DESC, cache_key LIMIT 8192)
                   UNION ALL
                   SELECT cache_key, updated_at_ms FROM (
                     SELECT cache_key, updated_at_ms FROM cluster_fragment_index_jobs
                      WHERE state = 'failed'
                      ORDER BY updated_at_ms DESC, cache_key LIMIT 8192)
                   UNION ALL
                   SELECT cache_key, updated_at_ms FROM (
                     SELECT cache_key, updated_at_ms FROM cluster_fragment_index_jobs
                      WHERE state = 'cancelled'
                      ORDER BY updated_at_ms DESC, cache_key LIMIT 8192)
                   ORDER BY updated_at_ms DESC, cache_key LIMIT 8192)",
            )
            .expect("prepare summary terminal-window plan");
        let details = plan
            .query_map([], |row| row.get::<_, String>(3))
            .expect("query summary terminal-window plan")
            .collect::<Result<Vec<_>, _>>()
            .expect("summary terminal-window plan rows");
        assert_eq!(
            details
                .iter()
                .filter(|detail| { detail.contains("cluster_fragment_index_jobs_status_history") })
                .count(),
            3,
            "each terminal state must have its own bounded newest-first index walk: {details:?}"
        );
        let columns = conn
            .prepare("PRAGMA table_info(media_sessions)")
            .expect("prepare media-session columns")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("read media-session columns")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect media-session columns");
        assert!(columns.iter().any(|column| column == "terminal_reason"));
        assert!(columns
            .iter()
            .any(|column| column == "publication_ready_at_ms"));
    }
}
