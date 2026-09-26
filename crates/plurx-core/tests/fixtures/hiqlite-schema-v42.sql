-- Frozen replicated Store schema at cluster schema 42.
--
-- Captured once, on 2026-09-26, from `HiqliteAuthStore::bootstrap` at main
-- commit 58e752603 (the last main commit whose AUTH_SCHEMA_VERSION was 42),
-- run against a three-voter contract cluster: every sqlite_master object in
-- creation order (tables, then indexes, then triggers), then every row that
-- bootstrap wrote. Removed: hiqlite's own `_metadata` table, and the FTS5
-- shadow tables and their rows, which `CREATE VIRTUAL TABLE` recreates.
--
-- DO NOT REGENERATE OR EDIT. The store contract
-- `fresh_bootstrap_matches_the_migration_chain_from_a_frozen_v42_tree`
-- migrates this tree through the real replicated chain and compares the
-- result with a fresh bootstrap. Rebuilding this file from newer code would
-- turn that into a comparison of bootstrap with itself.

CREATE TABLE cluster_meta (
    singleton        INTEGER PRIMARY KEY CHECK (singleton = 1),
    schema_version   INTEGER NOT NULL,
    protocol_min     INTEGER NOT NULL,
    protocol_max     INTEGER NOT NULL,
    migrated_at      INTEGER NOT NULL
) STRICT;
CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
CREATE TABLE users (
    id            INTEGER PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT NOT NULL,
    is_admin      INTEGER NOT NULL,
    created_at    INTEGER NOT NULL
) STRICT;
CREATE TABLE tokens (
    token_hash   TEXT PRIMARY KEY,
    user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device       TEXT,
    created_at   INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL
) STRICT;
CREATE TABLE cluster_credential_mutation_intents (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1)
) STRICT;
CREATE TABLE cluster_credential_guard_activation (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1)
) STRICT;
CREATE TABLE api_keys (
    id           INTEGER PRIMARY KEY,
    name         TEXT NOT NULL,
    key_hash     TEXT NOT NULL UNIQUE,
    scopes       TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER,
    disabled     INTEGER NOT NULL
) STRICT;
CREATE TABLE job_leases (
    resource       TEXT PRIMARY KEY,
    owner_node_id  TEXT NOT NULL,
    fence          INTEGER NOT NULL CHECK (fence > 0),
    revision       INTEGER NOT NULL CHECK (revision > 0),
    expires_at_ms  INTEGER NOT NULL,
    updated_at_ms  INTEGER NOT NULL
) STRICT;
CREATE TABLE libraries (
    id                    INTEGER PRIMARY KEY,
    name                  TEXT NOT NULL UNIQUE,
    kind                  TEXT NOT NULL,
    paths                 TEXT NOT NULL,
    anime                 INTEGER NOT NULL DEFAULT 0,
    created_at            INTEGER NOT NULL,
    scan_interval_mins    INTEGER NOT NULL DEFAULT 0,
    refresh_interval_mins INTEGER NOT NULL DEFAULT 0,
    last_scan_at          INTEGER,
    last_refresh_at       INTEGER
) STRICT;
CREATE TABLE items (
    id                   INTEGER PRIMARY KEY,
    library_id           INTEGER NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    kind                 TEXT NOT NULL,
    parent_id            INTEGER REFERENCES items(id) ON DELETE CASCADE,
    title                TEXT NOT NULL,
    sort_title           TEXT NOT NULL,
    year                 INTEGER,
    overview             TEXT,
    tmdb_id              INTEGER,
    imdb_id              TEXT,
    season_number        INTEGER,
    episode_number       INTEGER,
    air_date             TEXT,
    runtime_ms           INTEGER,
    poster_path          TEXT,
    backdrop_path        TEXT,
    added_at             INTEGER NOT NULL,
    updated_at           INTEGER NOT NULL,
    recorded_at          TEXT,
    tags                 TEXT NOT NULL DEFAULT '[]',
    nfo_seeded_at        INTEGER,
    metadata_at          INTEGER,
    artwork_attempted_at INTEGER,
    artwork_error        TEXT,
    genres               TEXT NOT NULL DEFAULT '[]',
    author               TEXT,
    book_work_id         TEXT,
    book_edition_id      TEXT,
    book_metadata_source TEXT CHECK (book_metadata_source IN ('epub', 'curator'))
) STRICT;
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
    scanned_at       INTEGER NOT NULL,
    hdr_format       TEXT,
    audio_offset_ms  INTEGER NOT NULL DEFAULT 0,
    dv_profile       INTEGER,
    dv_level         INTEGER,
    dv_bl_compat_id  INTEGER,
    dv_el_present    INTEGER,
    dv_rpu_present   INTEGER,
    video_codec_tag  TEXT
) STRICT;
CREATE TABLE watch_state (
    user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_id     INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    position_ms INTEGER NOT NULL DEFAULT 0,
    duration_ms INTEGER,
    watched     INTEGER NOT NULL DEFAULT 0,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (user_id, item_id)
) STRICT;
CREATE TABLE library_roots (
    library_id  INTEGER PRIMARY KEY REFERENCES libraries(id) ON DELETE CASCADE,
    fingerprint TEXT NOT NULL
) STRICT;
CREATE TABLE scan_reconcile_guards (
    library_id INTEGER PRIMARY KEY REFERENCES libraries(id) ON DELETE CASCADE
) STRICT;
CREATE TABLE scan_reconcile_items (
    library_id INTEGER NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    item_id    INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    PRIMARY KEY (library_id, item_id)
) STRICT;
CREATE VIRTUAL TABLE items_fts USING fts5(
    title, overview, tags, content='', contentless_delete=1
);
CREATE VIRTUAL TABLE items_fts_vocab USING fts5vocab(items_fts, 'instance');
CREATE TABLE reading_state (
    user_id            INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_id            INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    file_id            INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    file_size          INTEGER NOT NULL,
    file_mtime         INTEGER NOT NULL,
    locator_json       TEXT NOT NULL,
    progression_millis INTEGER NOT NULL CHECK (progression_millis BETWEEN 0 AND 1000000),
    completed          INTEGER NOT NULL CHECK (completed IN (0, 1)),
    updated_at         INTEGER NOT NULL,
    PRIMARY KEY (user_id, item_id, file_id)
) STRICT;
CREATE TABLE trakt_auth (
    user_id         INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    access_token    TEXT NOT NULL,
    refresh_token   TEXT NOT NULL,
    expires_at      INTEGER NOT NULL,
    trakt_username  TEXT,
    connected_at    INTEGER NOT NULL,
    last_sync_at    INTEGER NOT NULL,
    last_activities TEXT
) STRICT;
CREATE TABLE watched_outbox (
    id         INTEGER PRIMARY KEY,
    payload    TEXT NOT NULL,
    attempts   INTEGER NOT NULL,
    last_error TEXT NOT NULL,
    status     TEXT NOT NULL CHECK (status IN ('pending', 'ok', 'failed')),
    next_at    INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    claim_until INTEGER NOT NULL
) STRICT;
CREATE TABLE transcode_cache_recipes (
    recipe_hash    TEXT PRIMARY KEY,
    file_id        INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    recipe_version INTEGER NOT NULL,
    created_at     INTEGER NOT NULL
) STRICT;
CREATE TABLE transcode_cache_locations (
    recipe_hash   TEXT NOT NULL REFERENCES transcode_cache_recipes(recipe_hash)
                               ON DELETE CASCADE,
    node_id       TEXT NOT NULL,
    storage_class TEXT NOT NULL CHECK (storage_class IN ('local', 'shared')),
    relative_dir  TEXT NOT NULL,
    storage_id    TEXT NOT NULL DEFAULT '',
    generation_id TEXT NOT NULL DEFAULT '',
    bytes         INTEGER NOT NULL,
    complete      INTEGER NOT NULL,
    manifest_digest TEXT,
    scrub_object_index INTEGER NOT NULL DEFAULT 0,
    publication_generation INTEGER NOT NULL DEFAULT 0 CHECK (publication_generation >= 0),
    last_used_at  INTEGER NOT NULL,
    last_seen_at  INTEGER NOT NULL,
    PRIMARY KEY (recipe_hash, node_id, storage_class)
) STRICT;
CREATE TABLE offline_packages (
    id                TEXT PRIMARY KEY,
    request_id        TEXT NOT NULL,
    user_id           INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    file_id           INTEGER NOT NULL,
    node_id           TEXT NOT NULL,
    source_path       TEXT NOT NULL,
    source_size       INTEGER NOT NULL,
    source_mtime      INTEGER NOT NULL,
    recipe_hash       TEXT,
    claim_generation  INTEGER NOT NULL DEFAULT 0 CHECK (claim_generation >= 0),
    decoder_recovery_state TEXT NOT NULL DEFAULT 'primary'
        CHECK (decoder_recovery_state IN
            ('primary', 'recovery_pending', 'rehome_pending', 'alternate')),
    alternate_recipe_hash TEXT,
    effective_rate_control TEXT NOT NULL DEFAULT 'vbr'
        CHECK (effective_rate_control = 'vbr'
               OR (effective_rate_control GLOB 'qvbr:[0-9]*'
                   AND substr(effective_rate_control, 6) NOT GLOB '*[^0-9]*'
                   AND length(substr(effective_rate_control, 6)) BETWEEN 1 AND 3
                   AND CAST(substr(effective_rate_control, 6) AS INTEGER) BETWEEN 0 AND 255
                   AND printf('%d', CAST(substr(effective_rate_control, 6) AS INTEGER)) =
                       substr(effective_rate_control, 6))),
    target_height     INTEGER NOT NULL,
    output_width      INTEGER,
    output_height     INTEGER,
    audio_index       INTEGER,
    audio_offset_ms   INTEGER NOT NULL,
    subtitle_index    INTEGER,
    subtitle_language TEXT,
    subtitle_mode     TEXT NOT NULL CHECK (subtitle_mode IN ('none', 'native', 'burned')),
    state             TEXT NOT NULL CHECK (state IN ('queued', 'preparing', 'ready', 'failed')),
    phase             TEXT NOT NULL,
    progress_millis   INTEGER NOT NULL,
    estimated_bytes   INTEGER NOT NULL,
    reserved_bytes    INTEGER NOT NULL,
    actual_bytes      INTEGER,
    duration_ms       INTEGER,
    error_code        TEXT,
    error_message     TEXT,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    last_access_at    INTEGER NOT NULL,
    expires_at        INTEGER NOT NULL,
    UNIQUE (user_id, request_id)
) STRICT;
CREATE TABLE offline_package_leases (
    token_hash     TEXT PRIMARY KEY,
    package_id     TEXT NOT NULL UNIQUE REFERENCES offline_packages(id) ON DELETE CASCADE,
    created_at     INTEGER NOT NULL,
    last_access_at INTEGER NOT NULL,
    expires_at     INTEGER NOT NULL
) STRICT;
CREATE TABLE offline_lease_guards (
    package_id TEXT PRIMARY KEY REFERENCES offline_packages(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL,
    had_lease  INTEGER NOT NULL
) STRICT;
CREATE TABLE offline_source_probes (
    package_id   TEXT NOT NULL REFERENCES offline_packages(id) ON DELETE CASCADE,
    node_id      TEXT NOT NULL,
    requested_at INTEGER NOT NULL,
    answered_at  INTEGER,
    readable     INTEGER CHECK (readable IN (0, 1)),
    PRIMARY KEY (package_id, node_id)
) STRICT;
CREATE TABLE dv_conversions (
    file_id        INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    state          TEXT NOT NULL,
    el_type        TEXT,
    original_path  TEXT,
    bytes_before   INTEGER,
    bytes_after    INTEGER,
    error          TEXT,
    queued_at_ms   INTEGER NOT NULL,
    finished_at_ms INTEGER,
    recovery_guard_id TEXT CHECK
                        (state != 'committed' OR original_path IS NOT NULL
                         OR recovery_guard_id IS NOT NULL)
) STRICT;
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
CREATE TABLE pretranscode_jobs (
    id                TEXT PRIMARY KEY,
    dedupe_key        TEXT NOT NULL,
    file_id           INTEGER NOT NULL,
    source_size       INTEGER NOT NULL,
    source_mtime      INTEGER NOT NULL,
    target_height     INTEGER NOT NULL,
    policy_generation TEXT NOT NULL,
    requirements_json TEXT NOT NULL,
    reason            TEXT NOT NULL CHECK (reason IN ('in_progress', 'next_up', 'recent')),
    priority          INTEGER NOT NULL,
    state             TEXT NOT NULL CHECK (
                          state IN ('queued', 'running', 'ready', 'failed', 'cancelled')),
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
CREATE TABLE media_session_requests (
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
CREATE TABLE media_playback_pointers (
    user_id                INTEGER NOT NULL,
    playback_id            TEXT NOT NULL CHECK (length(playback_id) BETWEEN 1 AND 128),
    current_incarnation_id TEXT NOT NULL UNIQUE,
    updated_at_ms          INTEGER NOT NULL,
    -- The ask this pointer was written against. Nullable, and the null means
    -- the playback had no recorded ask -- see the fence triggers, which read a
    -- null on a playback that *does* have one as a writer from before this
    -- column. A fresh install declares it here; an upgrade adds it by `ALTER`,
    -- and the two have to end in the same shape.
    desired_revision       INTEGER,
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
    terminal_reason               TEXT CHECK (terminal_reason IN
                                      ('deleted', 'superseded', 'admin_stop', 'revoked', 'replaced')),
    publication_ready_at_ms       INTEGER NOT NULL DEFAULT 0 CHECK (publication_ready_at_ms >= 0),
    recipe_json                   TEXT NOT NULL,
    response_json                 TEXT NOT NULL,
    produced_playable_through_ms  INTEGER NOT NULL DEFAULT 0,
    fetched_through_ms            INTEGER NOT NULL DEFAULT 0,
    media_origin_ms               INTEGER NOT NULL DEFAULT 0,
    media_sequence                INTEGER NOT NULL DEFAULT 0,
    discontinuity_sequence        INTEGER NOT NULL DEFAULT 0,
    updated_at_ms                 INTEGER NOT NULL,
    recovery_epoch                TEXT NOT NULL DEFAULT '',
    -- When a predecessor being drained on purpose stops being kept. Null means
    -- not draining, which is what every session starts as. A fresh cluster
    -- declares it here; an upgrade adds it by `ALTER`, and the migration has
    -- to ask which of the two it is looking at before it tries.
    drain_deadline_ms             INTEGER
) STRICT;
CREATE TABLE media_session_terminal_acks (
    incarnation_id     TEXT NOT NULL UNIQUE,
    session_id         TEXT PRIMARY KEY,
    owner_node_id      TEXT NOT NULL,
    owner_epoch        INTEGER NOT NULL CHECK (owner_epoch > 0),
    client_instance_id TEXT NOT NULL,
    sequence           INTEGER NOT NULL CHECK (sequence > 0),
    request_fingerprint TEXT NOT NULL,
    response_json      TEXT NOT NULL,
    expires_at_ms      INTEGER NOT NULL,
    updated_at_ms      INTEGER NOT NULL
) STRICT;
CREATE TABLE media_session_preparations (
        user_id                            INTEGER NOT NULL,
        playback_id                        TEXT NOT NULL
            CHECK (length(playback_id) BETWEEN 1 AND 128),
        staged_incarnation_id              TEXT NOT NULL UNIQUE,
        expected_predecessor_incarnation_id TEXT NOT NULL
            CHECK (length(expected_predecessor_incarnation_id) BETWEEN 1 AND 128),
        deadline_ms                        INTEGER NOT NULL,
        created_at_ms                      INTEGER NOT NULL,
        updated_at_ms                      INTEGER NOT NULL,
        PRIMARY KEY (user_id, playback_id)
    ) STRICT;
CREATE TABLE media_session_producer_recovery (
        user_id                 INTEGER NOT NULL,
        playback_id             TEXT NOT NULL
            CHECK (length(playback_id) BETWEEN 1 AND 128),
        recovery_epoch          TEXT NOT NULL
            CHECK (length(recovery_epoch) BETWEEN 1 AND 128),
        failed_incarnation_id   TEXT NOT NULL,
        failed_producer_attempt INTEGER NOT NULL CHECK (failed_producer_attempt > 0),
        decision_sequence       INTEGER NOT NULL CHECK (decision_sequence > 0),
        failed_plan_digest      TEXT NOT NULL CHECK (length(failed_plan_digest) = 64),
        alternate_plan_digest   TEXT NOT NULL CHECK (length(alternate_plan_digest) = 64),
        decode_restriction      TEXT
            CHECK (decode_restriction IS NULL OR
                   length(CAST(decode_restriction AS BLOB)) <= 4096),
        state                   TEXT NOT NULL
            CHECK (state IN ('reserved', 'installed', 'exhausted')),
        created_at_ms           INTEGER NOT NULL,
        updated_at_ms           INTEGER NOT NULL,
        PRIMARY KEY (user_id, playback_id, recovery_epoch)
    ) STRICT;
CREATE TABLE media_playback_desired (
        user_id        INTEGER NOT NULL,
        playback_id    TEXT NOT NULL
            CHECK (length(playback_id) BETWEEN 1 AND 128),
        revision       INTEGER NOT NULL CHECK (revision > 0),
        digest         TEXT NOT NULL CHECK (length(digest) = 64),
        canonical_form TEXT NOT NULL
            CHECK (length(canonical_form) BETWEEN 1 AND 512),
        updated_at_ms  INTEGER NOT NULL,
        PRIMARY KEY (user_id, playback_id)
    ) STRICT;
CREATE TABLE cache_storage_members (
    storage_id          TEXT NOT NULL,
    node_id             TEXT NOT NULL,
    storage_class       TEXT NOT NULL CHECK (storage_class IN ('local', 'shared')),
    verified_at_ms      INTEGER NOT NULL,
    verification_state TEXT NOT NULL CHECK (
        verification_state IN ('verified', 'suspect', 'unverified')),
    PRIMARY KEY (storage_id, node_id)
) STRICT;
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
CREATE TABLE timeline_annotation_sets (
    file_id           INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    source_size       INTEGER NOT NULL CHECK (source_size >= 0),
    source_mtime      INTEGER NOT NULL,
    argv_fingerprint  TEXT NOT NULL,
    generation_id     TEXT NOT NULL,
    version           INTEGER NOT NULL CHECK (version > 0),
    annotations_json  TEXT NOT NULL,
    updated_at_ms     INTEGER NOT NULL
, publication_priority TEXT NOT NULL DEFAULT 'normal' CHECK (publication_priority IN ('normal','forced'))) STRICT;
CREATE TABLE timeline_manual_overrides (
    file_id           INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    kind              TEXT NOT NULL CHECK (kind IN ('intro','recap','credits','preview')),
    source_size       INTEGER NOT NULL CHECK (source_size >= 0),
    source_mtime      INTEGER NOT NULL,
    argv_fingerprint  TEXT NOT NULL,
    start_ticks       INTEGER NOT NULL CHECK (start_ticks >= 0),
    end_ticks         INTEGER NOT NULL CHECK (end_ticks > start_ticks),
    timescale         INTEGER NOT NULL CHECK (timescale > 0),
    start_ms          INTEGER NOT NULL CHECK (start_ms >= 0),
    end_ms            INTEGER NOT NULL CHECK (end_ms > start_ms),
    revision          INTEGER NOT NULL CHECK (revision > 0),
    generation_id     TEXT NOT NULL,
    updated_at_ms     INTEGER NOT NULL,
    PRIMARY KEY (file_id, kind)
) STRICT;
CREATE TABLE cluster_fragment_index_sources (
        node_id          TEXT NOT NULL,
        file_id          INTEGER NOT NULL,
        object_version   TEXT NOT NULL,
        source_size      INTEGER NOT NULL,
        source_mtime     INTEGER NOT NULL,
        source_sha256    TEXT NOT NULL,
        observed_at_ms   INTEGER NOT NULL,
        PRIMARY KEY (node_id, file_id)
    ) STRICT;
CREATE TABLE cluster_fragment_index_artifacts (
        cache_key         TEXT PRIMARY KEY,
        file_id           INTEGER NOT NULL,
        source_size       INTEGER NOT NULL,
        source_mtime      INTEGER NOT NULL,
        source_sha256     TEXT NOT NULL,
        pipeline_sha256   TEXT NOT NULL,
        blob_sha256       TEXT NOT NULL,
        bytes             INTEGER NOT NULL CHECK (bytes > 0),
        built_by_node_id  TEXT NOT NULL,
        built_at_ms       INTEGER NOT NULL
    ) STRICT;
CREATE TABLE cluster_fragment_index_locations (
        cache_key         TEXT NOT NULL,
        node_id           TEXT NOT NULL,
        bytes             INTEGER NOT NULL CHECK (bytes > 0),
        verified_at_ms    INTEGER NOT NULL,
        last_seen_at_ms   INTEGER NOT NULL,
        PRIMARY KEY (cache_key, node_id)
    ) STRICT;
CREATE TABLE cluster_fragment_index_jobs (
        cache_key         TEXT NOT NULL,
        file_id           INTEGER NOT NULL,
        source_size       INTEGER NOT NULL,
        source_mtime      INTEGER NOT NULL,
        source_sha256     TEXT NOT NULL,
        pipeline_sha256   TEXT NOT NULL,
        priority          TEXT NOT NULL DEFAULT 'normal'
          CHECK (priority IN ('normal','forced','foreground')),
        trigger           TEXT NOT NULL DEFAULT 'background'
          CHECK (trigger IN ('admin','background','foreground')),
        target_node_id    TEXT NOT NULL DEFAULT '',
        state             TEXT NOT NULL CHECK (
            state IN ('queued', 'running', 'ready', 'failed', 'cancelled')),
        owner_node_id     TEXT,
        fence             INTEGER NOT NULL DEFAULT 0 CHECK (fence >= 0),
        lease_expires_ms  INTEGER,
        attempts          INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
        not_before_ms     INTEGER NOT NULL,
        last_error_code   TEXT,
        created_at_ms     INTEGER NOT NULL,
        updated_at_ms     INTEGER NOT NULL, attempt_errors TEXT NOT NULL DEFAULT '', index_retry_deadline_ms INTEGER NOT NULL DEFAULT 0, index_diagnostic_json TEXT NOT NULL DEFAULT '',
        PRIMARY KEY (cache_key, target_node_id)
    ) STRICT;
CREATE TABLE analysis_requests (
        request_id         TEXT PRIMARY KEY,
        file_id            INTEGER NOT NULL,
        source_size        INTEGER NOT NULL,
        source_mtime       INTEGER NOT NULL,
        component          TEXT NOT NULL CHECK (component IN ('fragment_index','skip_markers')),
        pipeline_version   TEXT NOT NULL DEFAULT 'legacy-fragment-index',
        requested_generation TEXT NOT NULL DEFAULT 'legacy-generation',
        expected_predecessor_generation TEXT NOT NULL DEFAULT '',
        priority           TEXT NOT NULL DEFAULT 'normal' CHECK (priority IN ('normal','forced')),
        trigger            TEXT NOT NULL DEFAULT 'admin' CHECK (trigger IN ('admin','background')),
        force_rebuild      INTEGER NOT NULL CHECK (force_rebuild IN (0, 1)),
        target_node_id     TEXT NOT NULL,
        state              TEXT NOT NULL CHECK (
            state IN ('queued', 'running', 'submitted', 'ready', 'failed', 'cancelled')),
        owner_node_id      TEXT,
        fence              INTEGER NOT NULL DEFAULT 0 CHECK (fence >= 0),
        lease_expires_ms   INTEGER,
        attempts           INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
        not_before_ms      INTEGER NOT NULL,
        result_cache_key   TEXT,
        last_error_code    TEXT,
        cancel_requested   INTEGER NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1)),
        created_at_ms      INTEGER NOT NULL,
        updated_at_ms      INTEGER NOT NULL
    , video_identity TEXT NOT NULL DEFAULT '') STRICT;
CREATE TABLE analysis_attempts (
        request_id          TEXT NOT NULL REFERENCES analysis_requests(request_id) ON DELETE CASCADE,
        attempt             INTEGER NOT NULL CHECK (attempt > 0),
        claim_node_id       TEXT NOT NULL,
        claim_epoch         INTEGER NOT NULL CHECK (claim_epoch > 0),
        claim_expires_at_ms INTEGER NOT NULL,
        phase               TEXT NOT NULL CHECK (
            phase IN ('claimed','source_probe','hashing','staged','running','publishing','retry_wait','published','failed','canceled','stale')),
        started_at_ms       INTEGER NOT NULL,
        phase_updated_at_ms INTEGER NOT NULL,
        terminal_code       TEXT,
        PRIMARY KEY (request_id, claim_epoch)
    ) STRICT;
CREATE TABLE cluster_fragment_index_heads (
        logical_cache_key    TEXT PRIMARY KEY,
        generation_cache_key TEXT NOT NULL,
        request_id           TEXT NOT NULL,
        updated_at_ms        INTEGER NOT NULL
    ) STRICT;
CREATE TABLE analysis_lifecycle_counters (
        event  TEXT NOT NULL CHECK (event IN ('claim','lease_loss','retry','cancel','stale','failure','publication')),
        reason TEXT NOT NULL CHECK (reason IN (
          'all','lease_expired','source_catalog_read_failed','source_probe_timeout',
          'source_unavailable','source_attestation_failed','foreground_preempted',
          'source_attestation_timeout','source_record_failed','queue_write_failed',
          'queue_full_or_busy','pipeline_version_unavailable','admin_cancelled',
          'source_deleted','source_identity_changed','attempt_limit','stored_probe_invalid',
          'source_duration_missing','invalid_cache_identity','unsupported','other','validated')),
        count INTEGER NOT NULL DEFAULT 0 CHECK (count >= 0),
        PRIMARY KEY (event, reason)
    ) STRICT;
CREATE TABLE analysis_index_repairs (
        repair_revision       TEXT NOT NULL,
        file_id               INTEGER NOT NULL,
        source_size           INTEGER NOT NULL,
        source_mtime          INTEGER NOT NULL,
        source_sha256         TEXT NOT NULL,
        pipeline_sha256       TEXT NOT NULL,
        target_node_id        TEXT NOT NULL,
        video_identity        TEXT NOT NULL,
        predecessor_cache_key TEXT NOT NULL,
        predecessor_fence     INTEGER NOT NULL,
        successor_request_id  TEXT NOT NULL,
        created_at_ms         INTEGER NOT NULL,
        PRIMARY KEY (
          repair_revision, file_id, source_size, source_mtime, source_sha256,
          pipeline_sha256, target_node_id
        )
    ) STRICT;
CREATE TABLE library_channels (
    id                    TEXT PRIMARY KEY,
    owner_user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name                  TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 320),
    description           TEXT NOT NULL CHECK (length(description) <= 2000),
    visibility            TEXT NOT NULL CHECK (visibility IN ('personal','shared')),
    enabled               INTEGER NOT NULL CHECK (enabled IN (0,1)),
    definition_revision   INTEGER NOT NULL CHECK (definition_revision > 0),
    recipe_json           TEXT NOT NULL,
    seed                  BLOB NOT NULL CHECK (length(seed) = 32),
    active_generation_id  TEXT,
    active_epoch_ms       INTEGER,
    pending_generation_id TEXT,
    pending_epoch_ms      INTEGER,
    build_state           TEXT NOT NULL DEFAULT 'draft'
                          CHECK (build_state IN ('draft','queued','building','ready','failed')),
    build_error_code      TEXT,
    build_error_message   TEXT,
    build_candidate_count INTEGER NOT NULL DEFAULT 0 CHECK (build_candidate_count >= 0),
    build_entry_count     INTEGER NOT NULL DEFAULT 0 CHECK (build_entry_count >= 0),
    build_last_attempt_ms INTEGER,
    build_last_success_ms INTEGER,
    last_auto_build_ms    INTEGER,
    created_at_ms         INTEGER NOT NULL,
    updated_at_ms         INTEGER NOT NULL
) STRICT;
CREATE TABLE library_channel_generations (
    id                     TEXT PRIMARY KEY,
    channel_id             TEXT NOT NULL REFERENCES library_channels(id) ON DELETE CASCADE,
    definition_revision    INTEGER NOT NULL CHECK (definition_revision > 0),
    algorithm_version      INTEGER NOT NULL CHECK (algorithm_version > 0),
    content_digest         TEXT NOT NULL CHECK (length(content_digest) = 64),
    state                  TEXT NOT NULL CHECK (state IN ('building','ready','failed')),
    entry_count            INTEGER NOT NULL DEFAULT 0 CHECK (entry_count >= 0),
    loop_duration_ms       INTEGER NOT NULL DEFAULT 0 CHECK (loop_duration_ms >= 0),
    build_claim_id         TEXT,
    build_claim_expires_ms INTEGER,
    created_at_ms          INTEGER NOT NULL
) STRICT;
CREATE TABLE library_channel_entries (
    generation_id       TEXT NOT NULL REFERENCES library_channel_generations(id) ON DELETE CASCADE,
    ordinal             INTEGER NOT NULL CHECK (ordinal >= 0),
    item_id             INTEGER NOT NULL CHECK (item_id > 0),
    file_id             INTEGER NOT NULL CHECK (file_id > 0),
    file_size           INTEGER NOT NULL CHECK (file_size >= 0),
    file_mtime          INTEGER NOT NULL CHECK (file_mtime >= 0),
    duration_ms         INTEGER NOT NULL CHECK (duration_ms > 0 AND duration_ms <= 86400000),
    cumulative_start_ms INTEGER NOT NULL CHECK (cumulative_start_ms >= 0),
    show_id             INTEGER,
    PRIMARY KEY (generation_id, ordinal),
    UNIQUE (generation_id, item_id)
) STRICT;
CREATE TABLE library_channel_favourites (
    user_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel_id    TEXT NOT NULL REFERENCES library_channels(id) ON DELETE CASCADE,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (user_id, channel_id)
) STRICT;
CREATE TABLE library_channel_requests (
    user_id        INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    request_id     TEXT NOT NULL,
    operation_hash TEXT NOT NULL CHECK (length(operation_hash) = 64),
    channel_id     TEXT NOT NULL,
    result_revision INTEGER NOT NULL CHECK (result_revision >= 0),
    created_at_ms  INTEGER NOT NULL,
    expires_at_ms  INTEGER NOT NULL,
    PRIMARY KEY (user_id, request_id)
) STRICT;
CREATE TABLE library_channel_session_recipes (
    user_id         INTEGER NOT NULL,
    request_id      TEXT NOT NULL,
    incarnation_id  TEXT NOT NULL UNIQUE,
    recipe_json     TEXT NOT NULL CHECK (length(recipe_json) BETWEEN 2 AND 32768),
    created_at_ms   INTEGER NOT NULL,
    PRIMARY KEY (user_id, request_id)
) STRICT;
CREATE TABLE dvr_rules (
    id               TEXT PRIMARY KEY,
    owner_user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    priority         INTEGER NOT NULL,
    name             TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 80),
    match_mode       TEXT NOT NULL CHECK (match_mode IN ('series_id', 'title')),
    match_value      TEXT NOT NULL CHECK (length(match_value) BETWEEN 1 AND 200),
    channel_id       TEXT,
    new_only         INTEGER NOT NULL DEFAULT 1 CHECK (new_only IN (0,1)),
    keep_mode        TEXT NOT NULL CHECK (keep_mode IN ('all', 'last_n', 'until_watched', 'days')),
    keep_value       INTEGER NOT NULL DEFAULT 0 CHECK (keep_value >= 0),
    pad_start_s      INTEGER NOT NULL CHECK (pad_start_s BETWEEN 0 AND 3600),
    pad_end_s        INTEGER NOT NULL CHECK (pad_end_s BETWEEN 0 AND 3600),
    enabled          INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    created_at_ms    INTEGER NOT NULL,
    updated_at_ms    INTEGER NOT NULL
) STRICT;
CREATE TABLE dvr_recordings (
    id                        TEXT PRIMARY KEY,
    origin                    TEXT NOT NULL CHECK (origin IN ('manual', 'rule')),
    rule_id                   TEXT REFERENCES dvr_rules(id) ON DELETE SET NULL,
    requested_by_user_id      INTEGER REFERENCES users(id) ON DELETE SET NULL,
    channel_id                TEXT NOT NULL,
    guide_number              TEXT NOT NULL,
    channel_name              TEXT NOT NULL,
    airing_start              INTEGER NOT NULL,
    airing_end                INTEGER NOT NULL,
    capture_start             INTEGER NOT NULL,
    capture_end               INTEGER NOT NULL,
    title                     TEXT NOT NULL,
    episode_title             TEXT,
    episode                   TEXT,
    synopsis                  TEXT,
    image_url                 TEXT,
    original_air_date         TEXT,
    series_id                 TEXT,
    programme_id              TEXT,
    state                     TEXT NOT NULL CHECK (state IN
                                ('scheduled','conflict','withdrawn','stale','recording',
                                 'done','partial','failed','missed','cancelled','deleted')),
    state_reason              TEXT,
    attempt                   INTEGER NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    gap_s                     INTEGER NOT NULL DEFAULT 0 CHECK (gap_s >= 0),
    late_start_s              INTEGER NOT NULL DEFAULT 0 CHECK (late_start_s >= 0),
    tuner_owner_node_id       TEXT,
    path                      TEXT,
    bytes                     INTEGER NOT NULL DEFAULT 0 CHECK (bytes >= 0),
    last_progress_ms          INTEGER,
    stop_requested_at_ms      INTEGER,
    stop_requested_by_user_id INTEGER,
    item_id                   INTEGER,
    file_id                   INTEGER,
    started_at_ms             INTEGER,
    finished_at_ms            INTEGER,
    stopped_by_user_id        INTEGER,
    created_at_ms             INTEGER NOT NULL,
    updated_at_ms             INTEGER NOT NULL
) STRICT;
CREATE TABLE dvr_reminders (
    id             TEXT PRIMARY KEY,
    user_id        INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel_id     TEXT NOT NULL,
    guide_number   TEXT NOT NULL,
    airing_start   INTEGER NOT NULL,
    airing_end     INTEGER NOT NULL,
    title          TEXT NOT NULL,
    lead_s         INTEGER NOT NULL CHECK (lead_s BETWEEN 0 AND 3600),
    state          TEXT NOT NULL CHECK (state IN ('armed', 'fired', 'acked', 'expired', 'moved')),
    fired_at_ms    INTEGER,
    acked_at_ms    INTEGER,
    created_at_ms  INTEGER NOT NULL,
    updated_at_ms  INTEGER NOT NULL
) STRICT;
CREATE TABLE dvr_event_heads (
    recording_id TEXT PRIMARY KEY REFERENCES dvr_recordings(id) ON DELETE CASCADE,
    next_sequence INTEGER NOT NULL CHECK (next_sequence >= 1),
    latest_attention_sequence INTEGER NOT NULL DEFAULT 0,
    latest_attention_at_ms INTEGER,
    history_started_at_ms INTEGER NOT NULL,
    history_has_gap INTEGER NOT NULL DEFAULT 0 CHECK (history_has_gap IN (0,1)),
    pruned_through_sequence INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE TABLE dvr_events (
    recording_id TEXT NOT NULL REFERENCES dvr_recordings(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL CHECK (sequence >= 1),
    event_id TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    occurred_at_ms INTEGER NOT NULL,
    attempt INTEGER,
    actor_user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
    reason_code TEXT,
    facts_json TEXT NOT NULL CHECK (length(facts_json) <= 4096),
    PRIMARY KEY (recording_id, sequence)
) STRICT;
CREATE TABLE dvr_attention_acks (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    recording_id TEXT NOT NULL REFERENCES dvr_recordings(id) ON DELETE CASCADE,
    through_sequence INTEGER NOT NULL CHECK (through_sequence >= 0),
    acknowledged_at_ms INTEGER NOT NULL,
    PRIMARY KEY (user_id, recording_id)
) STRICT;
CREATE TABLE media_classifications (
 item_id INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
 source_json TEXT NOT NULL,
 payload TEXT NOT NULL,
 overrides TEXT NOT NULL DEFAULT '{"include":[],"exclude":[]}',
 terms TEXT NOT NULL,
 revision INTEGER NOT NULL DEFAULT 1
) STRICT;
CREATE VIRTUAL TABLE classification_fts USING fts5(terms);
CREATE TABLE library_channel_subject_jobs (
 id TEXT PRIMARY KEY,
 owner_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 channel_id TEXT REFERENCES library_channels(id) ON DELETE CASCADE,
 channel_revision INTEGER,
 identity TEXT NOT NULL,
 state TEXT NOT NULL,
 payload TEXT NOT NULL,
 claim_id TEXT,
 claim_expires_ms INTEGER NOT NULL DEFAULT 0,
 available_ms INTEGER NOT NULL,
 updated_ms INTEGER NOT NULL,
 expires_ms INTEGER NOT NULL
) STRICT;
CREATE TABLE library_channel_subject_decisions (
 owner_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 subject_digest TEXT NOT NULL,
 item_id INTEGER NOT NULL,
 metadata_digest TEXT NOT NULL,
 classifier_profile TEXT NOT NULL,
 payload TEXT NOT NULL,
 last_used_ms INTEGER NOT NULL,
 PRIMARY KEY(owner_user_id, subject_digest, item_id, metadata_digest, classifier_profile)
) STRICT;
CREATE INDEX idx_items_library_kind ON items(library_id, kind);
CREATE INDEX idx_items_parent ON items(parent_id);
CREATE INDEX idx_items_added ON items(added_at DESC);
CREATE INDEX idx_items_missing_artwork ON items(artwork_attempted_at)
    WHERE poster_path IS NULL;
CREATE INDEX idx_items_book_work ON items(book_work_id)
    WHERE book_work_id IS NOT NULL;
CREATE INDEX idx_files_item ON files(item_id);
CREATE INDEX idx_watch_updated ON watch_state(user_id, updated_at DESC);
CREATE INDEX idx_reading_updated
    ON reading_state(user_id, updated_at DESC);
CREATE INDEX watched_outbox_due
    ON watched_outbox(status, next_at, claim_until);
CREATE INDEX transcode_cache_recipes_file
    ON transcode_cache_recipes(file_id);
CREATE INDEX transcode_cache_lru
    ON transcode_cache_locations(node_id, complete, last_used_at);
CREATE INDEX offline_packages_queue
    ON offline_packages(node_id, state, created_at);
CREATE INDEX offline_packages_recipe
    ON offline_packages(node_id, recipe_hash, state);
CREATE INDEX offline_packages_user_state
    ON offline_packages(user_id, state, updated_at);
CREATE INDEX offline_package_leases_expiry
    ON offline_package_leases(expires_at);
CREATE INDEX offline_source_probes_pending
    ON offline_source_probes(node_id, answered_at, requested_at);
CREATE INDEX dv_conversions_queue
        ON dv_conversions(state, queued_at_ms, file_id);
CREATE UNIQUE INDEX dv_conversions_recovery_guard
        ON dv_conversions(recovery_guard_id) WHERE recovery_guard_id IS NOT NULL;
CREATE INDEX dv_recovery_guards_file
        ON dv_recovery_guards(file_id, guard_id);
CREATE INDEX pretranscode_jobs_due
        ON pretranscode_jobs(state, not_before_ms, priority DESC, created_at_ms, id);
CREATE INDEX pretranscode_jobs_dedupe
        ON pretranscode_jobs(dedupe_key, state);
CREATE INDEX pretranscode_jobs_staging
        ON pretranscode_jobs(staging_node_id, state, id);
CREATE UNIQUE INDEX pretranscode_jobs_active
        ON pretranscode_jobs(dedupe_key) WHERE state IN ('queued', 'running');
CREATE INDEX media_session_requests_expiry
        ON media_session_requests(state, claim_expires_at_ms);
CREATE INDEX media_sessions_owner
        ON media_sessions(owner_node_id, state, lease_expires_at_ms);
CREATE INDEX media_sessions_user
        ON media_sessions(user_id, state, lease_expires_at_ms);
CREATE INDEX media_sessions_expiry
        ON media_sessions(state, lease_expires_at_ms, incarnation_id);
CREATE INDEX media_sessions_retention
        ON media_sessions(state, updated_at_ms, incarnation_id);
CREATE INDEX media_session_terminal_acks_expiry
        ON media_session_terminal_acks(expires_at_ms, session_id);
CREATE UNIQUE INDEX transcode_cache_storage_generation
    ON transcode_cache_locations(recipe_hash, storage_id, generation_id)
    WHERE storage_id <> '' AND generation_id <> '';
CREATE INDEX transcode_cache_storage_lru
    ON transcode_cache_locations(storage_id, complete, last_used_at);
CREATE INDEX cache_storage_members_node
    ON cache_storage_members(node_id, verification_state, storage_id);
CREATE INDEX cache_consumer_pins_expiry
    ON cache_consumer_pins(storage_id, expires_at_ms);
CREATE INDEX cluster_fragment_index_artifacts_file
        ON cluster_fragment_index_artifacts(file_id, source_size, source_mtime, pipeline_sha256);
CREATE INDEX cluster_fragment_index_locations_node
        ON cluster_fragment_index_locations(node_id, last_seen_at_ms, cache_key);
CREATE INDEX cluster_fragment_index_jobs_due
        ON cluster_fragment_index_jobs(state, not_before_ms, created_at_ms, cache_key, target_node_id);
CREATE INDEX cluster_fragment_index_jobs_status_history
        ON cluster_fragment_index_jobs(state, updated_at_ms DESC, cache_key, target_node_id);
CREATE INDEX analysis_requests_due
        ON analysis_requests(target_node_id, state, not_before_ms, created_at_ms, request_id);
CREATE INDEX analysis_requests_status
        ON analysis_requests(state, updated_at_ms DESC, request_id);
CREATE INDEX analysis_requests_result_history
        ON analysis_requests(result_cache_key, updated_at_ms DESC, request_id DESC)
        WHERE result_cache_key IS NOT NULL AND result_cache_key <> '';
CREATE INDEX analysis_attempts_recent
        ON analysis_attempts(request_id, claim_epoch DESC);
CREATE INDEX analysis_requests_terminal_identity
    ON analysis_requests(file_id, source_size, source_mtime, component,
                         pipeline_version, requested_generation, target_node_id,
                         updated_at_ms, request_id)
    WHERE state IN ('ready', 'failed', 'cancelled') AND force_rebuild = 0;
CREATE INDEX analysis_index_repairs_successor ON analysis_index_repairs(successor_request_id);
CREATE UNIQUE INDEX analysis_requests_one_active_source
        ON analysis_requests(file_id, source_size, source_mtime, component,
                             pipeline_version, video_identity,
                             requested_generation, target_node_id)
        WHERE state IN ('queued', 'running', 'submitted');
CREATE UNIQUE INDEX analysis_requests_one_active_forced_skip_successor
        ON analysis_requests(file_id, source_size, source_mtime, component)
        WHERE component = 'skip_markers' AND force_rebuild = 1
          AND state IN ('queued', 'running', 'submitted');
CREATE UNIQUE INDEX analysis_requests_one_active_forced_fragment_successor
        ON analysis_requests(file_id, source_size, source_mtime, component,
                             pipeline_version, video_identity)
        WHERE component = 'fragment_index' AND force_rebuild = 1
          AND state IN ('queued', 'running', 'submitted');
CREATE INDEX library_channels_owner
    ON library_channels(owner_user_id, id);
CREATE INDEX library_channels_shared
    ON library_channels(visibility, enabled, id);
CREATE INDEX library_channels_build_state
    ON library_channels(build_state, build_last_attempt_ms, id);
CREATE INDEX library_channel_generations_channel
    ON library_channel_generations(channel_id, created_at_ms DESC);
CREATE INDEX library_channel_requests_expiry
    ON library_channel_requests(expires_at_ms, user_id);
CREATE UNIQUE INDEX dvr_rules_priority ON dvr_rules(priority);
CREATE INDEX dvr_rules_owner ON dvr_rules(owner_user_id, id);
CREATE UNIQUE INDEX dvr_recordings_airing
    ON dvr_recordings(channel_id, airing_start);
CREATE INDEX dvr_recordings_state
    ON dvr_recordings(state, capture_start);
CREATE INDEX dvr_recordings_rule
    ON dvr_recordings(rule_id, state);
CREATE UNIQUE INDEX dvr_reminders_user_airing
    ON dvr_reminders(user_id, channel_id, airing_start);
CREATE INDEX dvr_reminders_due ON dvr_reminders(state, airing_start);
CREATE INDEX dvr_events_recent
    ON dvr_events(occurred_at_ms DESC, event_id DESC);
CREATE INDEX channel_subject_job_queue ON library_channel_subject_jobs(state, available_ms);
CREATE INDEX channel_subject_job_identity ON library_channel_subject_jobs(owner_user_id, identity);
CREATE INDEX channel_subject_decision_expiry ON library_channel_subject_decisions(last_used_ms);
CREATE TRIGGER library_roots_paths_au AFTER UPDATE OF paths ON libraries
WHEN old.paths <> new.paths BEGIN
    DELETE FROM library_roots WHERE library_id = new.id;
END;
CREATE TRIGGER items_fts_ai AFTER INSERT ON items BEGIN
    INSERT INTO items_fts(rowid, title, overview, tags)
    VALUES (new.id, new.title, new.overview, new.tags);
END;
CREATE TRIGGER items_fts_ad AFTER DELETE ON items BEGIN
    DELETE FROM items_fts WHERE rowid = old.id;
END;
CREATE TRIGGER items_fts_au AFTER UPDATE OF title, overview, tags ON items BEGIN
    DELETE FROM items_fts WHERE rowid = old.id;
    INSERT INTO items_fts(rowid, title, overview, tags)
    VALUES (new.id, new.title, new.overview, new.tags);
END;
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
CREATE TRIGGER dv_queue_admission_settings_ai
     AFTER INSERT ON settings
     WHEN NEW.key GLOB '__plurx_internal.dv_queue_admission.*' BEGIN
       INSERT INTO dv_conversions (file_id, state, queued_at_ms)
       SELECT f.id,
              'queued',
              CAST(json_extract(NEW.value, '$.requested_queued_at_ms') AS INTEGER)
         FROM files f
         JOIN items i ON i.id = f.item_id
         JOIN settings mode_setting ON mode_setting.key = 'library.dv_disk_convert'
        WHERE f.id = CAST(json_extract(NEW.value, '$.requested_file_id') AS INTEGER)
          AND json_extract(NEW.value, '$.request_kind') = 'single'
          AND json_extract(NEW.value, '$.outcome') = 'queued'
          AND LOWER(f.container) = 'mkv' AND f.dv_profile = 7
          AND f.dv_bl_compat_id IN (1, 6)
          AND f.dv_el_present = 1 AND f.dv_rpu_present = 1
          AND json_valid(mode_setting.value)
          AND json_type(mode_setting.value) = 'object'
          AND json_extract(
                mode_setting.value, '$."' || i.library_id || '"')
              IN ('manual', 'auto')
       ON CONFLICT(file_id) DO UPDATE SET
         state = 'queued', el_type = NULL, original_path = NULL,
         bytes_before = NULL, bytes_after = NULL, error = NULL,
         queued_at_ms = excluded.queued_at_ms, finished_at_ms = NULL,
         recovery_guard_id = NULL
       WHERE dv_conversions.state = 'failed';
       INSERT INTO dv_conversions (file_id, state, queued_at_ms)
       SELECT f.id,
              'queued',
              CAST(json_extract(NEW.value, '$.requested_queued_at_ms') AS INTEGER)
         FROM json_each(NEW.value, '$.candidate_ids') candidate
         JOIN files f ON f.id = CAST(candidate.value AS INTEGER)
         JOIN items i ON i.id = f.item_id
         JOIN settings mode_setting ON mode_setting.key = 'library.dv_disk_convert'
    LEFT JOIN dv_conversions d ON d.file_id = f.id
        WHERE json_extract(NEW.value, '$.request_kind') = 'library_batch'
          AND json_extract(NEW.value, '$.outcome') = 'queued'
          AND i.library_id =
                CAST(json_extract(NEW.value, '$.requested_library_id') AS INTEGER)
          AND LOWER(f.container) = 'mkv' AND f.dv_profile = 7
          AND f.dv_bl_compat_id IN (1, 6)
          AND f.dv_el_present = 1 AND f.dv_rpu_present = 1
          AND json_valid(mode_setting.value)
          AND json_type(mode_setting.value) = 'object'
          AND json_extract(
                mode_setting.value, '$."' || i.library_id || '"')
              IN ('manual', 'auto')
          AND (d.file_id IS NULL OR
               (json_extract(NEW.value, '$.retry_failed') = 1
                AND d.state = 'failed'))
       ON CONFLICT(file_id) DO UPDATE SET
         state = 'queued', el_type = NULL, original_path = NULL,
         bytes_before = NULL, bytes_after = NULL, error = NULL,
         queued_at_ms = excluded.queued_at_ms, finished_at_ms = NULL,
         recovery_guard_id = NULL
       WHERE json_extract(NEW.value, '$.retry_failed') = 1
         AND dv_conversions.state = 'failed';
       DELETE FROM settings WHERE key = NEW.key;
     END;
CREATE TRIGGER pretranscode_jobs_cancel_source BEFORE DELETE ON files
     BEGIN
       DELETE FROM pretranscode_jobs
        WHERE file_id = OLD.id AND state IN ('ready', 'failed', 'cancelled');
       UPDATE pretranscode_jobs
          SET state = 'cancelled', owner_node_id = NULL, staging_node_id = NULL,
              lease_expires_ms = NULL, policy_generation = '', requirements_json = '{}'
        WHERE file_id = OLD.id AND state IN ('queued', 'running');
     END;
CREATE TRIGGER media_session_publication_claim_au
    AFTER UPDATE OF publication_ready_at_ms ON media_sessions
    WHEN NEW.state = 'active' AND (
      (OLD.publication_ready_at_ms = 9223372036854775807
        AND NEW.publication_ready_at_ms != 9223372036854775807)
      OR (OLD.publication_ready_at_ms > 0
        AND OLD.publication_ready_at_ms < 9223372036854775807
        AND NEW.publication_ready_at_ms = 0)
    ) BEGIN
      UPDATE media_session_requests
         SET claim_expires_at_ms = CASE
               WHEN OLD.publication_ready_at_ms = 9223372036854775807
                    AND NEW.publication_ready_at_ms = 0
                 THEN NEW.lease_expires_at_ms
               WHEN OLD.publication_ready_at_ms = 9223372036854775807
                 THEN MIN(9223372036854775806,
                   NEW.publication_ready_at_ms
                     + (NEW.lease_expires_at_ms - OLD.updated_at_ms) + 1)
               ELSE MIN(9223372036854775806, NEW.lease_expires_at_ms + 1)
             END,
             updated_at_ms = NEW.updated_at_ms
       WHERE incarnation_id = NEW.incarnation_id
         AND request_fingerprint = NEW.request_fingerprint
         AND playback_id = NEW.playback_id
         AND owner_node_id = NEW.owner_node_id
         AND state = 'starting';
    END;
CREATE TRIGGER media_sessions_drain_ownership_fence_au
    BEFORE UPDATE OF owner_node_id, owner_epoch ON media_sessions
    WHEN OLD.drain_deadline_ms IS NOT NULL
     AND (NEW.owner_node_id IS NOT OLD.owner_node_id
          OR NEW.owner_epoch IS NOT OLD.owner_epoch)
    BEGIN
      SELECT RAISE(IGNORE);
    END;
CREATE TRIGGER media_playback_pointers_desired_fence_ai
    BEFORE INSERT ON media_playback_pointers
    WHEN EXISTS (SELECT 1 FROM media_playback_desired
                  WHERE user_id = NEW.user_id AND playback_id = NEW.playback_id
                    AND revision IS NOT NEW.desired_revision)
    BEGIN
      SELECT RAISE(ABORT, 'playback pointer written against an ask that is not current');
    END;
CREATE TRIGGER media_playback_pointers_desired_fence_au
    BEFORE UPDATE ON media_playback_pointers
    WHEN EXISTS (SELECT 1 FROM media_playback_desired
                  WHERE user_id = NEW.user_id AND playback_id = NEW.playback_id
                    AND revision IS NOT NEW.desired_revision)
    BEGIN
      SELECT RAISE(ABORT, 'playback pointer written against an ask that is not current');
    END;
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
CREATE TRIGGER cluster_fragment_indexes_cancel_source BEFORE DELETE ON files
    BEGIN
        DELETE FROM cluster_fragment_index_sources WHERE file_id = OLD.id;
        UPDATE cluster_fragment_index_jobs
           SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
               fence = fence + 1, last_error_code = 'source_deleted',
               updated_at_ms = MAX(updated_at_ms, OLD.scanned_at * 1000)
         WHERE file_id = OLD.id AND state IN ('queued', 'running');
    END;
CREATE TRIGGER analysis_requests_cancel_source BEFORE DELETE ON files
    BEGIN
        DELETE FROM cluster_fragment_index_heads
         WHERE generation_cache_key IN (
           SELECT cache_key FROM cluster_fragment_index_jobs WHERE file_id = OLD.id);
        UPDATE analysis_attempts
           SET phase = 'canceled', phase_updated_at_ms = MAX(phase_updated_at_ms, OLD.scanned_at * 1000),
               terminal_code = 'source_deleted'
         WHERE (request_id, claim_epoch) IN (
           SELECT request_id, fence FROM analysis_requests
            WHERE file_id = OLD.id AND state IN ('running', 'submitted'));
        UPDATE analysis_requests
           SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
               fence = fence + 1, cancel_requested = 1,
               last_error_code = 'source_deleted', updated_at_ms = MAX(updated_at_ms, OLD.scanned_at * 1000)
         WHERE file_id = OLD.id AND state IN ('queued', 'running', 'submitted');
    END;
CREATE TRIGGER analysis_requests_supersede_source
    AFTER UPDATE OF size, mtime ON files
    WHEN OLD.size <> NEW.size OR OLD.mtime <> NEW.mtime
    BEGIN
        DELETE FROM cluster_fragment_index_heads
         WHERE generation_cache_key IN (
           SELECT cache_key FROM cluster_fragment_index_jobs
            WHERE file_id = NEW.id
              AND (source_size <> NEW.size OR source_mtime <> NEW.mtime));
        UPDATE cluster_fragment_index_jobs
           SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
               fence = fence + 1, last_error_code = 'source_superseded',
               updated_at_ms = MAX(updated_at_ms, NEW.scanned_at * 1000)
         WHERE file_id = NEW.id AND state IN ('queued', 'running')
           AND (source_size <> NEW.size OR source_mtime <> NEW.mtime);
        UPDATE analysis_attempts
           SET phase = 'stale', phase_updated_at_ms = MAX(phase_updated_at_ms, NEW.scanned_at * 1000),
               terminal_code = 'source_superseded'
         WHERE (request_id, claim_epoch) IN (
           SELECT request_id, fence FROM analysis_requests
            WHERE file_id = NEW.id AND state IN ('running', 'submitted')
              AND (source_size <> NEW.size OR source_mtime <> NEW.mtime));
        UPDATE analysis_requests
           SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
               fence = fence + 1, cancel_requested = 1,
               last_error_code = 'source_superseded', updated_at_ms = MAX(updated_at_ms, NEW.scanned_at * 1000)
         WHERE file_id = NEW.id AND state IN ('queued', 'running', 'submitted')
           AND (source_size <> NEW.size OR source_mtime <> NEW.mtime);
    END;
CREATE TRIGGER analysis_requests_bound_terminal_history
    AFTER UPDATE OF state ON analysis_requests
    WHEN NEW.state IN ('ready', 'failed', 'cancelled')
    BEGIN
        DELETE FROM analysis_requests
         WHERE request_id IN (
           SELECT candidate.request_id FROM analysis_requests candidate
            WHERE candidate.state IN ('ready', 'failed', 'cancelled')
              AND candidate.request_id <> NEW.request_id
              AND (candidate.force_rebuild = 1
                OR NOT EXISTS (SELECT 1 FROM files
                     WHERE files.id = candidate.file_id
                       AND files.size = candidate.source_size
                       AND files.mtime = candidate.source_mtime)
                OR EXISTS (SELECT 1 FROM analysis_requests newer
                     WHERE newer.state IN ('ready', 'failed', 'cancelled')
                       AND newer.force_rebuild = 0
                       AND newer.file_id = candidate.file_id
                       AND newer.source_size = candidate.source_size
                       AND newer.source_mtime = candidate.source_mtime
                       AND newer.component = candidate.component
                       AND newer.pipeline_version = candidate.pipeline_version
                       AND newer.requested_generation = candidate.requested_generation
                       AND newer.target_node_id = candidate.target_node_id
                       AND (newer.updated_at_ms > candidate.updated_at_ms
                         OR (newer.updated_at_ms = candidate.updated_at_ms
                           AND newer.request_id > candidate.request_id))))
            ORDER BY candidate.updated_at_ms, candidate.request_id
            LIMIT MAX((SELECT COUNT(*) FROM analysis_requests
                        WHERE state IN ('ready', 'failed', 'cancelled')) - 8192, 0));
    END;
CREATE TRIGGER analysis_requests_lifecycle_counters
    AFTER UPDATE OF state ON analysis_requests
    WHEN OLD.state <> NEW.state
    BEGIN
        INSERT INTO analysis_lifecycle_counters(event, reason, count)
          SELECT 'claim', 'all', 1 WHERE NEW.state = 'running'
          ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'lease_loss', 'lease_expired', 1
       WHERE OLD.state = 'running'
         AND ((NEW.state = 'queued' AND NEW.last_error_code = 'lease_expired')
           OR (NEW.state = 'failed'
             AND OLD.lease_expires_ms IS NOT NULL
             AND OLD.lease_expires_ms <= NEW.updated_at_ms))
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
        INSERT INTO analysis_lifecycle_counters(event, reason, count)
          SELECT 'retry', CASE NEW.last_error_code
            WHEN 'lease_expired' THEN 'lease_expired'
            WHEN 'source_catalog_read_failed' THEN 'source_catalog_read_failed'
            WHEN 'source_probe_timeout' THEN 'source_probe_timeout'
            WHEN 'source_unavailable' THEN 'source_unavailable'
            WHEN 'source_attestation_failed' THEN 'source_attestation_failed'
            WHEN 'foreground_preempted' THEN 'foreground_preempted'
            WHEN 'source_attestation_timeout' THEN 'source_attestation_timeout'
            WHEN 'source_record_failed' THEN 'source_record_failed'
            WHEN 'queue_write_failed' THEN 'queue_write_failed'
            WHEN 'queue_full_or_busy' THEN 'queue_full_or_busy'
            WHEN 'pipeline_version_unavailable' THEN 'pipeline_version_unavailable'
            ELSE 'other' END, 1
           WHERE OLD.state = 'running' AND NEW.state = 'queued'
          ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
        INSERT INTO analysis_lifecycle_counters(event, reason, count)
          SELECT 'cancel', CASE NEW.last_error_code
            WHEN 'admin_cancelled' THEN 'admin_cancelled'
            WHEN 'source_deleted' THEN 'source_deleted'
            ELSE 'other' END, 1
           WHERE NEW.state = 'cancelled'
             AND COALESCE(NEW.last_error_code, '') NOT IN ('source_changed','source_superseded')
          ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
        INSERT INTO analysis_lifecycle_counters(event, reason, count)
          SELECT 'stale', 'source_identity_changed', 1
           WHERE NEW.state IN ('failed','cancelled')
             AND NEW.last_error_code IN ('source_changed','source_superseded')
          ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
        INSERT INTO analysis_lifecycle_counters(event, reason, count)
          SELECT 'failure', CASE NEW.last_error_code
            WHEN 'attempt_limit' THEN 'attempt_limit'
            WHEN 'pipeline_version_unavailable' THEN 'pipeline_version_unavailable'
            WHEN 'stored_probe_invalid' THEN 'stored_probe_invalid'
            WHEN 'source_duration_missing' THEN 'source_duration_missing'
            WHEN 'invalid_cache_identity' THEN 'invalid_cache_identity'
            WHEN 'unsupported' THEN 'unsupported'
            WHEN 'source_unavailable' THEN 'source_unavailable'
            ELSE 'other' END, 1
           WHERE NEW.state = 'failed'
             AND COALESCE(NEW.last_error_code, '') NOT IN ('source_changed','source_superseded')
          ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
        INSERT INTO analysis_lifecycle_counters(event, reason, count)
          SELECT 'publication', 'validated', 1
           WHERE NEW.state = 'ready' AND NEW.component = 'skip_markers'
          ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    END;
CREATE TRIGGER cluster_fragment_index_lifecycle_counters
    AFTER UPDATE OF state ON cluster_fragment_index_jobs
    WHEN OLD.state <> NEW.state
    BEGIN
        INSERT INTO analysis_lifecycle_counters(event, reason, count)
          SELECT 'claim', 'all', 1 WHERE NEW.state = 'running'
          ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
        INSERT INTO analysis_lifecycle_counters(event, reason, count)
          SELECT 'lease_loss', 'lease_expired', 1
           WHERE OLD.state = 'running'
             AND ((NEW.state = 'queued' AND NEW.last_error_code = 'lease_expired')
               OR (NEW.state = 'failed'
                 AND OLD.lease_expires_ms IS NOT NULL
                 AND OLD.lease_expires_ms <= NEW.updated_at_ms))
          ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
        INSERT INTO analysis_lifecycle_counters(event, reason, count)
          SELECT 'retry', CASE NEW.last_error_code
            WHEN 'lease_expired' THEN 'lease_expired'
            WHEN 'source_catalog_read_failed' THEN 'source_catalog_read_failed'
            WHEN 'source_unavailable' THEN 'source_unavailable'
            WHEN 'source_attestation_failed' THEN 'source_attestation_failed'
            WHEN 'foreground_preempted' THEN 'foreground_preempted'
            WHEN 'pipeline_version_unavailable' THEN 'pipeline_version_unavailable'
            ELSE 'other' END, 1
           WHERE OLD.state = 'running' AND NEW.state = 'queued'
          ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
        INSERT INTO analysis_lifecycle_counters(event, reason, count)
          SELECT 'cancel', CASE NEW.last_error_code
            WHEN 'source_deleted' THEN 'source_deleted' ELSE 'other' END, 1
           WHERE NEW.state = 'cancelled'
             AND COALESCE(NEW.last_error_code, '') NOT IN ('source_changed','source_superseded')
          ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
        INSERT INTO analysis_lifecycle_counters(event, reason, count)
          SELECT 'stale', 'source_identity_changed', 1
           WHERE NEW.state IN ('failed','cancelled')
             AND NEW.last_error_code IN ('source_changed','source_superseded')
          ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
        INSERT INTO analysis_lifecycle_counters(event, reason, count)
          SELECT 'failure', CASE NEW.last_error_code
            WHEN 'attempt_limit' THEN 'attempt_limit'
            WHEN 'pipeline_version_unavailable' THEN 'pipeline_version_unavailable'
            WHEN 'unsupported' THEN 'unsupported'
            WHEN 'source_unavailable' THEN 'source_unavailable'
            ELSE 'other' END, 1
           WHERE NEW.state = 'failed'
             AND COALESCE(NEW.last_error_code, '') NOT IN ('source_changed','source_superseded')
          ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
        INSERT INTO analysis_lifecycle_counters(event, reason, count)
          SELECT 'publication', 'validated', 1 WHERE NEW.state = 'ready'
          ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    END;
CREATE TRIGGER analysis_index_repairs_delete_source AFTER DELETE ON files
       BEGIN
         DELETE FROM analysis_index_repairs WHERE file_id = OLD.id;
       END;
CREATE TRIGGER library_channel_session_recipes_request_delete
    AFTER DELETE ON media_session_requests
BEGIN
    DELETE FROM library_channel_session_recipes
     WHERE user_id = OLD.user_id AND request_id = OLD.request_id;
END;
CREATE TRIGGER classification_ai AFTER INSERT ON media_classifications BEGIN
 INSERT INTO classification_fts(rowid,terms) VALUES(new.item_id,new.terms || ' ' || json_extract(new.source_json,'$.title') || ' ' || json_extract(new.source_json,'$.overview') || ' ' || json_extract(new.source_json,'$.genres') || ' ' || json_extract(new.source_json,'$.tags'));
END;
CREATE TRIGGER classification_ad AFTER DELETE ON media_classifications BEGIN
 DELETE FROM classification_fts WHERE rowid=old.item_id;
END;
CREATE TRIGGER classification_au AFTER UPDATE OF terms ON media_classifications BEGIN
 DELETE FROM classification_fts WHERE rowid=old.item_id;
 INSERT INTO classification_fts(rowid,terms) VALUES(new.item_id,new.terms || ' ' || json_extract(new.source_json,'$.title') || ' ' || json_extract(new.source_json,'$.overview') || ' ' || json_extract(new.source_json,'$.genres') || ' ' || json_extract(new.source_json,'$.tags'));
END;
CREATE TRIGGER classification_source_changed AFTER UPDATE OF title,overview,genres,tags,year,tmdb_id,kind ON items
WHEN old.kind IS NOT new.kind OR old.title IS NOT new.title OR old.overview IS NOT new.overview OR old.genres IS NOT new.genres OR old.tags IS NOT new.tags OR old.year IS NOT new.year OR old.tmdb_id IS NOT new.tmdb_id BEGIN
 DELETE FROM classification_fts WHERE rowid=new.id;
END;
INSERT INTO "cluster_meta" ("singleton", "schema_version", "protocol_min", "protocol_max", "migrated_at") VALUES (1, 42, 4, 4, 1790421733);
INSERT INTO "settings" ("key", "value", "updated_at") VALUES ('instance.id', '00000000-0000-4000-8000-000000000090', 1790421733);
