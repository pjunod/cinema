//! Exact collision bridge for the unpublished effort and deployed main ordinals.
//! Recognition is read-only. The bridge executes as one transaction, so there
//! is no legitimately committed partial bridge or intermediate marker.
//! Table witnesses below are the retained published69/91 capture schemas
//! (manifest8c58), not newly executed/private historical captures.
//!
//! Retirement: this bridge exists only for the 2026-10-04 ordinal collision
//! (SQLite `user_version` 88..=97 bridged to 97; replicated
//! `cluster_meta.schema_version` 66..=73 bridged to 73; see `bridge_plan`).
//! It may be deleted once no restorable artifact can sit inside either
//! range: every node's live store (SQLite and replicated) is at or above
//! the canonical end, AND no artifact older than that remains restorable —
//! no `plurxd` backup still retained under the `backup.keep` setting
//! (`keys::BACKUP_KEEP`, default 14, pruned in `backup.destination`) and no
//! activation import source backup still retained under `migration/`
//! (`MIGRATION_BACKUP_RETENTION` in `cluster/migration.rs`) predates the
//! bridge. Deleting the bridge must NOT delete the refusal: the migration
//! dispatchers — `SqliteStore::migrate` (`store/sqlite/mod.rs`, where
//! `bridge_sqlite` is called today) and `HiqliteAuthStore::migrate_schema`
//! (`store/hiqlite.rs`, where `bridge_private_lineage` is called today) —
//! must then refuse any marker in 88..=96 / 66..=72 before the ordinary
//! chain runs (today they bridge the recognised shapes instead). Private effort markers never exceed 93 / 69 and only the
//! published chain or this bridge stamps the canonical end, so without that
//! refusal a private-lineage backup restored later would be misread as a
//! canonical database at the same ordinal and migrated as one.
use std::collections::{BTreeMap, BTreeSet};

use crate::error::StoreError;
use rusqlite::Connection;

const HIQLITE_OFFLINE_PACKAGES: &str = r#"
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
) STRICT
"#;

const HIQLITE_DV_CONVERSIONS: &str = r#"
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
    requested_manually INTEGER NOT NULL DEFAULT 0 CHECK (requested_manually IN (0,1)),
    recovery_guard_id TEXT CHECK
                        (state != 'committed' OR original_path IS NOT NULL
                         OR recovery_guard_id IS NOT NULL)
) STRICT
"#;

const SQLITE_OFFLINE_PACKAGES: &str = r#"
CREATE TABLE offline_packages (
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
        expires_at         INTEGER NOT NULL, effective_rate_control TEXT NOT NULL
        DEFAULT 'vbr'
        CHECK (effective_rate_control = 'vbr'
               OR (effective_rate_control GLOB 'qvbr:[0-9]*'
                   AND substr(effective_rate_control, 6) NOT GLOB '*[^0-9]*'
                   AND length(substr(effective_rate_control, 6)) BETWEEN 1 AND 3
                   AND CAST(substr(effective_rate_control, 6) AS INTEGER) BETWEEN 0 AND 255
                   AND printf('%d', CAST(substr(effective_rate_control, 6) AS INTEGER)) =
                       substr(effective_rate_control, 6))), claim_generation INTEGER NOT NULL DEFAULT 0
        CHECK (claim_generation >= 0), decoder_recovery_state TEXT NOT NULL DEFAULT 'primary'
        CHECK (decoder_recovery_state IN
            ('primary', 'recovery_pending', 'rehome_pending', 'alternate')), alternate_recipe_hash TEXT,
        UNIQUE (user_id, request_id)
    ) STRICT
"#;

const SQLITE_DV_CONVERSIONS: &str = r#"
CREATE TABLE dv_conversions (
        file_id        INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
        state          TEXT NOT NULL,
        el_type        TEXT,
        original_path  TEXT,
        bytes_before   INTEGER,
        bytes_after    INTEGER,
        error          TEXT,
        queued_at_ms   INTEGER NOT NULL,
        finished_at_ms INTEGER
    , recovery_guard_id TEXT CHECK
         (state != 'committed' OR original_path IS NOT NULL
          OR recovery_guard_id IS NOT NULL), requested_manually INTEGER NOT NULL DEFAULT 0 CHECK (requested_manually IN (0,1))) STRICT
"#;

const SQLITE_NETWORK_PRIORS: &str = r#"
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
    ) STRICT
"#;

#[derive(Clone, Debug)]
pub(super) struct SchemaObject {
    pub kind: String,
    pub name: String,
    pub sql: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Backend {
    Hiqlite,
    Sqlite,
}

pub(super) const OBJECT_QUERY: &str = "SELECT type AS kind,name,sql FROM sqlite_schema WHERE name IN ('analysis_requests','background_jobs','background_job_commands','background_job_waiters','files','settings','offline_packages','dv_conversions','network_priors','candidate_link_priors','candidate_recovery','analysis_requests_result_target_force','background_jobs_file_source','dv_queue_admission_settings_ai','playback_input_generation','playback_settings_insert','playback_settings_update','playback_settings_delete','background_job_copy_output_target','background_job_publish_copy_output_command','background_job_encoded_output_target','background_job_source_changed','background_job_source_deleted') AND sql IS NOT NULL ORDER BY name";

// Raw definitions (including the full tables carrying the relevant columns)
// are rebound inside the replicated transaction, not merely read beforehand.
#[cfg(feature = "hiqlite-store")]
pub(super) const FINGERPRINT_QUERY: &str = "SELECT json_group_array(json_array(kind,name,sql)) AS fingerprint FROM (SELECT type AS kind,name,sql FROM sqlite_schema WHERE name IN ('analysis_requests','background_jobs','background_job_commands','background_job_waiters','files','settings','offline_packages','dv_conversions','network_priors','candidate_link_priors','candidate_recovery','analysis_requests_result_target_force','background_jobs_file_source','dv_queue_admission_settings_ai','playback_input_generation','playback_settings_insert','playback_settings_update','playback_settings_delete','background_job_copy_output_target','background_job_publish_copy_output_command','background_job_encoded_output_target','background_job_source_changed','background_job_source_deleted') AND sql IS NOT NULL ORDER BY name)";

/// Only static migration SQL is normalized. Quoted literals remain exact;
/// whitespace/case outside quotes and CREATE's IF NOT EXISTS do not change
/// SQLite's stored definition. No substring/existence-only match is accepted.
fn normalized(sql: &str) -> String {
    let mut output = String::new();
    let mut unquoted_head = String::new();
    let mut collect_head = true;
    let mut chars = sql.chars().peekable();
    let mut quote = None;
    while let Some(ch) = chars.next() {
        if let Some(delimiter) = quote {
            output.push(ch);
            if ch == delimiter {
                if chars.peek() == Some(&delimiter) {
                    output.push(chars.next().expect("peeked quote"));
                } else {
                    quote = None;
                }
            }
        } else if ch == '-' && chars.peek() == Some(&'-') {
            if collect_head {
                unquoted_head.push(' ');
            }
            for next in chars.by_ref() {
                if next == '\n' {
                    break;
                }
            }
        } else if ch == '\'' || ch == '"' || ch == '`' {
            collect_head = false;
            quote = Some(ch);
            output.push(ch);
        } else {
            if collect_head {
                unquoted_head.push(ch.to_ascii_lowercase());
            }
            if !ch.is_whitespace() {
                output.push(ch.to_ascii_lowercase());
            }
        }
    }
    let tokens: Vec<_> = unquoted_head.split_whitespace().take(6).collect();
    for (words, prefix) in [
        (
            &["create", "table", "if", "not", "exists"][..],
            "createtable",
        ),
        (
            &["create", "trigger", "if", "not", "exists"][..],
            "createtrigger",
        ),
        (
            &["create", "index", "if", "not", "exists"][..],
            "createindex",
        ),
        (
            &["create", "unique", "index", "if", "not", "exists"][..],
            "createuniqueindex",
        ),
    ] {
        if tokens.starts_with(words) {
            output.replace_range(prefix.len()..prefix.len() + "ifnotexists".len(), "");
            break;
        }
    }
    output.trim_end_matches(';').to_owned()
}

fn table_parts(sql: &str) -> Option<Vec<&str>> {
    let start = sql.find('(')?;
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut quote = None;
    let mut segment = start + 1;
    let mut chars = sql
        .char_indices()
        .skip_while(|(index, _)| *index <= start)
        .peekable();
    while let Some((index, ch)) = chars.next() {
        if let Some(delimiter) = quote {
            if ch == delimiter {
                if chars.peek().is_some_and(|(_, next)| *next == delimiter) {
                    chars.next();
                } else {
                    quote = None;
                }
            }
            continue;
        }
        if ch == '\'' || ch == '"' || ch == '`' {
            quote = Some(ch);
        } else if ch == '(' {
            depth += 1;
        } else if (ch == ',' || ch == ')') && depth == 0 {
            let definition = sql[segment..index].trim();
            parts.push(definition);
            if ch == ')' {
                return Some(parts);
            }
            segment = index + 1;
        } else if ch == ')' {
            depth -= 1;
        }
    }
    None
}

fn column_definition(sql: &str, name: &str) -> Option<String> {
    table_parts(sql)?
        .into_iter()
        .find(|part| part.split_whitespace().next() == Some(name))
        .map(normalized)
}

fn known_table(
    objects: &BTreeMap<&str, &SchemaObject>,
    name: &str,
    expected: &str,
    additive_columns: &[&str],
) -> bool {
    let Some(object) = objects.get(name) else {
        return false;
    };
    if object.kind != "table" {
        return false;
    }
    let parts = |sql: &str| -> Option<BTreeSet<String>> {
        Some(
            table_parts(sql)?
                .into_iter()
                .filter(|part| {
                    !additive_columns.contains(&part.split_whitespace().next().unwrap_or_default())
                })
                .map(normalized)
                .collect(),
        )
    };
    let suffix = |sql: &str| sql.rfind(')').map(|end| normalized(&sql[end + 1..]));
    parts(&object.sql).is_some_and(|actual| Some(actual) == parts(expected))
        && suffix(&object.sql) == suffix(expected)
}

pub(super) fn playback_statements() -> Vec<&'static str> {
    // Each trigger's inner semicolon is followed by END, not a newline.
    // This is deliberately bound to this one five-statement static schema.
    super::PLAYBACK_INPUT_SCHEMA
        .split(";\n")
        .map(str::trim)
        .filter(|sql| !sql.is_empty())
        .collect()
}

fn object_statement<'a>(source: &'a str, kind: &str, name: &str) -> Option<&'a str> {
    let prefix = format!("create{kind}{name}");
    source
        .split("-- next statement\n")
        .find(|sql| normalized(sql).starts_with(&prefix))
}

fn has_exact(objects: &BTreeMap<&str, &SchemaObject>, kind: &str, name: &str, sql: &str) -> bool {
    objects
        .get(name)
        .is_some_and(|object| object.kind == kind && normalized(&object.sql) == normalized(sql))
}

fn expected_object(
    objects: &BTreeMap<&str, &SchemaObject>,
    kind: &str,
    name: &str,
    source: &str,
    present: bool,
) -> bool {
    if present {
        object_statement(source, kind, name).is_some_and(|sql| has_exact(objects, kind, name, sql))
    } else {
        !objects.contains_key(name)
    }
}

fn exact_column(
    objects: &BTreeMap<&str, &SchemaObject>,
    table: &str,
    name: &str,
    expected: Option<&str>,
) -> bool {
    let Some(object) = objects.get(table) else {
        return false;
    };
    if object.kind != "table" {
        return false;
    }
    let actual = column_definition(&object.sql, name);
    actual == expected.map(normalized)
}

fn matches(
    objects: &BTreeMap<&str, &SchemaObject>,
    backend: Backend,
    main: usize,
    effort: usize,
    fresh_encoded_omission: bool,
) -> bool {
    matches_with_playback(
        objects,
        backend,
        main,
        effort,
        fresh_encoded_omission,
        if main >= 4 { 4 } else { 0 },
    )
}

fn matches_with_playback(
    objects: &BTreeMap<&str, &SchemaObject>,
    backend: Backend,
    main: usize,
    effort: usize,
    fresh_encoded_omission: bool,
    playback_count: usize,
) -> bool {
    let sqlite = backend == Backend::Sqlite;
    let copy_at = if sqlite { 4 } else { 2 };
    let candidate_at = if sqlite { 5 } else { 3 };
    let encoded_at = if sqlite { 6 } else { 4 };
    let encoded = effort >= encoded_at && !fresh_encoded_omission;
    let guards = if encoded {
        super::background_jobs::ENCODED_OUTPUT_SCHEMA
    } else if effort >= copy_at {
        super::background_jobs::COPY_OUTPUT_SCHEMA
    } else {
        super::background_jobs::SCHEMA
    };
    let playback = playback_statements();
    if playback.len() != 5 {
        return false;
    }
    let playback_objects = [
        "playback_input_generation",
        "playback_settings_insert",
        "playback_settings_update",
        "playback_settings_delete",
    ];
    let playback_sql = [playback[0], playback[2], playback[3], playback[4]];
    let audio = (effort >= 1).then_some("audio_recipe TEXT");
    let provenance = (main >= 3).then_some(
        "requested_manually INTEGER NOT NULL DEFAULT 0 CHECK (requested_manually IN (0,1))",
    );
    known_table(
        objects,
        "offline_packages",
        if sqlite {
            SQLITE_OFFLINE_PACKAGES
        } else {
            HIQLITE_OFFLINE_PACKAGES
        },
        &["audio_recipe"],
    ) && [
        "background_jobs",
        "background_job_commands",
        "background_job_waiters",
    ]
    .iter()
    .all(|name| {
        object_statement(super::background_jobs::SCHEMA, "table", name)
            .is_some_and(|sql| known_table(objects, name, sql, &[]))
    }) && known_table(
        objects,
        "dv_conversions",
        if sqlite {
            SQLITE_DV_CONVERSIONS
        } else {
            HIQLITE_DV_CONVERSIONS
        },
        &["requested_manually"],
    ) && (!sqlite
        || known_table(
            objects,
            "network_priors",
            SQLITE_NETWORK_PRIORS,
            &["link_worst_rung_height", "link_starved_at_ms"],
        ))
        && exact_column(objects, "offline_packages", "audio_recipe", audio)
        && exact_column(objects, "dv_conversions", "requested_manually", provenance)
        && (!sqlite
            || (exact_column(
                objects,
                "network_priors",
                "link_worst_rung_height",
                (effort >= 2).then_some("link_worst_rung_height INTEGER"),
            ) && exact_column(
                objects,
                "network_priors",
                "link_starved_at_ms",
                (effort >= 2).then_some("link_starved_at_ms INTEGER"),
            )))
        && expected_object(
            objects,
            "index",
            "analysis_requests_result_target_force",
            super::fragment_index_cluster::ANALYSIS_RESULT_TARGET_FORCE_SCHEMA,
            main >= 1,
        )
        && expected_object(
            objects,
            "index",
            "background_jobs_file_source",
            super::background_jobs::PREPARATION_INDEX_SCHEMA,
            main >= 2,
        )
        && (sqlite
            || has_exact(
                objects,
                "trigger",
                "dv_queue_admission_settings_ai",
                if main >= 3 {
                    super::dv_conversion::DV_REQUEST_PROVENANCE_TRIGGER
                } else {
                    super::dv_conversion::DV_QUEUE_ADMISSION_MIGRATION_TRIGGER
                },
            ))
        && playback_objects
            .iter()
            .zip(playback_sql)
            .enumerate()
            .all(|(index, (name, sql))| {
                expected_object(
                    objects,
                    if *name == "playback_input_generation" {
                        "table"
                    } else {
                        "trigger"
                    },
                    name,
                    sql,
                    index < playback_count,
                )
            })
        && expected_object(
            objects,
            "table",
            "candidate_recovery",
            super::candidate_recovery::SCHEMA,
            effort >= candidate_at,
        )
        && (!sqlite
            || expected_object(
                objects,
                "table",
                "candidate_link_priors",
                super::candidate_link::SCHEMA,
                effort >= 3,
            ))
        && expected_object(
            objects,
            "trigger",
            "background_job_copy_output_target",
            super::background_jobs::COPY_OUTPUT_SCHEMA,
            effort >= copy_at,
        )
        && expected_object(
            objects,
            "trigger",
            "background_job_publish_copy_output_command",
            super::background_jobs::COPY_OUTPUT_SCHEMA,
            effort >= copy_at,
        )
        && expected_object(
            objects,
            "trigger",
            "background_job_encoded_output_target",
            super::background_jobs::ENCODED_OUTPUT_SCHEMA,
            encoded,
        )
        && expected_object(
            objects,
            "trigger",
            "background_job_source_changed",
            guards,
            true,
        )
        && expected_object(
            objects,
            "trigger",
            "background_job_source_deleted",
            guards,
            true,
        )
}

/// `None` is a recognized canonical prefix: the existing published dispatcher
/// applies its unchanged chain. `Some` is exactly one old effort fingerprint.
pub(super) fn bridge_plan(
    backend: Backend,
    marker: i64,
    objects: &[SchemaObject],
) -> Result<Option<Vec<String>>, StoreError> {
    let threshold = if backend == Backend::Sqlite { 88 } else { 66 };
    if marker < threshold {
        return Ok(None);
    }
    let objects: BTreeMap<_, _> = objects
        .iter()
        .map(|object| (object.name.as_str(), object))
        .collect();
    let main_end = threshold + 3;
    let effort_end = if backend == Backend::Sqlite { 6 } else { 4 };
    let canonical_end = main_end + effort_end;
    // The collision only spans markers both lineages wrote: private effort
    // markers stop at `threshold + effort_end - 1` and published markers at
    // `canonical_end`. A marker past `canonical_end` was stamped by a binary
    // that already reached the canonical union (the bridge itself stamps
    // `canonical_end`) and then applied later ordinary steps, which may
    // legitimately reshape these objects. It is not an ambiguous ordinal, so
    // the ordinary dispatcher owns it; the caller's newer-than-binary refusal
    // still applies.
    if marker > canonical_end {
        return Ok(None);
    }
    let main = (marker - threshold + 1).min(4) as usize;
    let effort = (marker - main_end).max(0) as usize;
    if matches(&objects, backend, main, effort, false) {
        return Ok(None);
    }
    // Preserve the explicitly known published replay boundaries. The
    // published SQLite runner committed each main step before its
    // separate user_version update, so a database it tore can sit one
    // marker behind its shape; the existing v90 guard handles an
    // already-present exact column. The current runner stamps the marker
    // inside each step's transaction, so no effort step (v92+) can tear.
    // Hiqlite's published playback batch can leave its exact ordered
    // object prefix before the v69 marker transaction. No effort object
    // or arbitrary partial/mixed schema is admitted by these cases.
    if backend == Backend::Sqlite
        && marker < main_end
        && matches(&objects, backend, main + 1, 0, false)
    {
        return Ok(None);
    }
    if backend == Backend::Hiqlite
        && marker == 68
        && (1..=4).any(|count| matches_with_playback(&objects, backend, 3, 0, false, count))
    {
        return Ok(None);
    }
    let effort = (marker - threshold + 1) as usize;
    let known_private =
        effort <= effort_end as usize && matches(&objects, backend, 0, effort, false);
    let fresh_omission =
        backend == Backend::Hiqlite && effort == 4 && matches(&objects, backend, 0, effort, true);
    if !known_private && !fresh_omission {
        return Err(StoreError::Migration(format!("schema lineage at marker {marker} is unknown, partial or mixed; no lineage SQL was submitted; retain the database and inspect its named migration objects before using a supported bridge")));
    }
    let mut sql = vec![
        super::fragment_index_cluster::ANALYSIS_RESULT_TARGET_FORCE_SCHEMA.to_owned(),
        super::background_jobs::PREPARATION_INDEX_SCHEMA.to_owned(),
        super::dv_conversion::DV_REQUEST_PROVENANCE_COLUMN.to_owned(),
    ];
    if backend == Backend::Hiqlite {
        sql.push("DROP TRIGGER dv_queue_admission_settings_ai".to_owned());
        sql.push(super::dv_conversion::DV_REQUEST_PROVENANCE_TRIGGER.to_owned());
    }
    sql.extend(playback_statements().into_iter().map(str::to_owned));
    if backend == Backend::Sqlite {
        if effort < 2 {
            sql.extend(
                super::telemetry::NETWORK_PRIOR_LINK_COLUMNS
                    .split(';')
                    .map(str::trim)
                    .filter(|sql| !sql.is_empty())
                    .map(str::to_owned),
            );
        }
        if effort < 3 {
            sql.push(super::candidate_link::SCHEMA.to_owned());
        }
    }
    let copy_at = if backend == Backend::Sqlite { 4 } else { 2 };
    let candidate_at = if backend == Backend::Sqlite { 5 } else { 3 };
    if effort < copy_at {
        sql.extend(
            super::background_jobs::COPY_OUTPUT_SCHEMA
                .split("-- next statement\n")
                .map(str::to_owned),
        );
    }
    if effort < candidate_at {
        sql.push(super::candidate_recovery::SCHEMA.to_owned());
    }
    if effort < effort_end as usize || fresh_omission {
        sql.extend(
            super::background_jobs::ENCODED_OUTPUT_SCHEMA
                .split("-- next statement\n")
                .map(str::to_owned),
        );
    }
    Ok(Some(sql))
}

pub(super) fn verify_union(backend: Backend, objects: &[SchemaObject]) -> Result<(), StoreError> {
    let objects = objects
        .iter()
        .map(|object| (object.name.as_str(), object))
        .collect();
    if matches(
        &objects,
        backend,
        4,
        if backend == Backend::Sqlite { 6 } else { 4 },
        false,
    ) {
        Ok(())
    } else {
        Err(StoreError::Migration(
            "lineage bridge did not produce the exact canonical schema; refusing the marker update"
                .to_owned(),
        ))
    }
}

pub(super) fn collect_sqlite(conn: &Connection) -> Result<Vec<SchemaObject>, StoreError> {
    let mut query = conn.prepare(OBJECT_QUERY)?;
    let objects = query.query_map([], |row| {
        Ok(SchemaObject {
            kind: row.get(0)?,
            name: row.get(1)?,
            sql: row.get(2)?,
        })
    })?;
    objects
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

pub(super) fn fingerprint(objects: &[SchemaObject]) -> Result<String, StoreError> {
    serde_json::to_string(
        &objects
            .iter()
            .map(|object| [&object.kind, &object.name, &object.sql])
            .collect::<Vec<_>>(),
    )
    .map_err(|error| StoreError::Migration(format!("schema fingerprint: {error}")))
}

/// Derive the exact expected SQLite spelling with the same bundled engine,
/// using definitions only, never copying rows, auth material or a database.
/// This avoids inventing ALTER TABLE's sqlite_schema text representation.
pub(super) fn expected_fingerprint(
    backend: Backend,
    objects: &[SchemaObject],
    statements: &[String],
) -> Result<String, StoreError> {
    if objects.len() > 40
        || objects.iter().map(|object| object.sql.len()).sum::<usize>() > 256 * 1024
    {
        return Err(StoreError::Migration(
            "lineage schema definition bound exceeded".to_owned(),
        ));
    }
    let scratch = Connection::open_in_memory()?;
    for kind in ["table", "index", "trigger"] {
        for object in objects.iter().filter(|object| object.kind == kind) {
            scratch.execute_batch(&object.sql)?;
        }
    }
    for statement in statements {
        scratch.execute_batch(statement)?;
    }
    let expected = collect_sqlite(&scratch)?;
    verify_union(backend, &expected)?;
    fingerprint(&expected)
}

/// Called before the ordinary SQLite dispatcher can reinterpret old ordinals.
pub(super) fn bridge_sqlite(conn: &Connection, marker: i64) -> Result<bool, StoreError> {
    bridge_sqlite_inner(conn, marker, false)
}

fn bridge_sqlite_inner(
    conn: &Connection,
    marker: i64,
    fail_before_stamp: bool,
) -> Result<bool, StoreError> {
    if marker < 88 {
        return Ok(false);
    }
    let objects = collect_sqlite(conn)?;
    let Some(statements) = bridge_plan(Backend::Sqlite, marker, &objects)? else {
        return Ok(false);
    };
    let expected = expected_fingerprint(Backend::Sqlite, &objects, &statements)?;
    let before = fingerprint(&objects)?;
    // IMMEDIATE excludes another writer between the fingerprint/marker CAS
    // and DDL. Foreign keys remain ON; this bridge drops no table or row.
    let foreign_keys: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    if foreign_keys != 1 {
        return Err(StoreError::Migration(
            "lineage bridge requires foreign_keys=ON".to_owned(),
        ));
    }
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result: Result<bool, StoreError> = (|| {
        let current: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if current != marker || fingerprint(&collect_sqlite(conn)?)? != before {
            return Err(StoreError::Migration(
                "lineage marker/schema changed before transaction; no bridge SQL applied"
                    .to_owned(),
            ));
        }
        for statement in &statements {
            conn.execute_batch(statement)?;
        }
        if fingerprint(&collect_sqlite(conn)?)? != expected {
            return Err(StoreError::Migration(
                "lineage post-schema differs; rolling back without a marker update".to_owned(),
            ));
        }
        let dangling: i64 =
            conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })?;
        if dangling != 0 {
            return Err(StoreError::Migration(format!(
                "lineage bridge left {dangling} foreign-key violations"
            )));
        }
        if fail_before_stamp {
            conn.execute_batch("SELECT json('validation-only lineage rollback')")?;
        }
        conn.pragma_update(None, "user_version", 97)?;
        conn.execute_batch("COMMIT")?;
        Ok(true)
    })();
    if result.is_err() {
        // Preserve the original bridge error; report a rollback failure too.
        if let Err(rollback) = conn.execute_batch("ROLLBACK") {
            return Err(StoreError::Migration(format!(
                "{}; lineage rollback failed: {rollback}",
                result.expect_err("checked error")
            )));
        }
    }
    result
}

#[cfg(feature = "hiqlite-contract-tests")]
pub fn validation_sqlite_bridge_rollback(conn: &Connection) -> Result<bool, StoreError> {
    let marker = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    bridge_sqlite_inner(conn, marker, true)
}

#[cfg(feature = "hiqlite-contract-tests")]
pub fn validation_sqlite_union_fingerprint(conn: &Connection) -> Result<String, StoreError> {
    let objects = collect_sqlite(conn)?;
    verify_union(Backend::Sqlite, &objects)?;
    fingerprint(&objects)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_past_canonical_end_belong_to_the_ordinary_dispatcher() {
        // The next ordinary step (SQLite v98, hiqlite v74) reshapes objects
        // freely; it is not a collision ordinal and needs no bridge.
        assert!(matches!(bridge_plan(Backend::Sqlite, 98, &[]), Ok(None)));
        assert!(matches!(bridge_plan(Backend::Hiqlite, 74, &[]), Ok(None)));
        // Inside the collision range an unrecognized schema still refuses.
        assert!(bridge_plan(Backend::Sqlite, 97, &[]).is_err());
        assert!(bridge_plan(Backend::Hiqlite, 73, &[]).is_err());
    }

    #[test]
    fn quoted_literals_are_exact_and_foreign_defaults_refuse() {
        assert_eq!(
            normalized(
                "CREATE TABLE IF NOT EXISTS example (value TEXT DEFAULT 'IF NOT EXISTS Primary');"
            ),
            normalized("CREATE TABLE example (value TEXT DEFAULT 'IF NOT EXISTS Primary');")
        );
        assert_ne!(
            normalized("DEFAULT 'IF NOT EXISTS Primary'"),
            normalized("DEFAULT 'Primary'")
        );
        assert_ne!(
            normalized("DEFAULT 'Primary'"),
            normalized("DEFAULT 'primary'")
        );
        assert_eq!(
            normalized("DEFAULT 'a''IF NOT EXISTS B'"),
            "default'a''IF NOT EXISTS B'"
        );
        let mut objects = Vec::new();
        for name in [
            "background_jobs",
            "background_job_commands",
            "background_job_waiters",
        ] {
            objects.push(SchemaObject {
                kind: "table".to_owned(),
                name: name.to_owned(),
                sql: object_statement(super::super::background_jobs::SCHEMA, "table", name)
                    .expect("known table source")
                    .to_owned(),
            });
        }
        for name in [
            "background_job_source_changed",
            "background_job_source_deleted",
        ] {
            objects.push(SchemaObject {
                kind: "trigger".to_owned(),
                name: name.to_owned(),
                sql: object_statement(super::super::background_jobs::SCHEMA, "trigger", name)
                    .expect("known guard source")
                    .to_owned(),
            });
        }
        objects.push(SchemaObject {
            kind: "table".to_owned(),
            name: "offline_packages".to_owned(),
            sql: HIQLITE_OFFLINE_PACKAGES.replace(
                "UNIQUE (user_id, request_id)",
                "audio_recipe TEXT, UNIQUE (user_id, request_id)",
            ),
        });
        objects.push(SchemaObject { kind: "table".to_owned(), name: "dv_conversions".to_owned(), sql: HIQLITE_DV_CONVERSIONS.replace("    requested_manually INTEGER NOT NULL DEFAULT 0 CHECK (requested_manually IN (0,1)),\n", "") });
        objects.push(SchemaObject {
            kind: "trigger".to_owned(),
            name: "dv_queue_admission_settings_ai".to_owned(),
            sql: super::super::dv_conversion::DV_QUEUE_ADMISSION_MIGRATION_TRIGGER.to_owned(),
        });
        assert!(bridge_plan(Backend::Hiqlite, 66, &objects)
            .expect("exact private source")
            .is_some());
        let offline = objects
            .iter_mut()
            .find(|object| object.name == "offline_packages")
            .expect("offline witness");
        offline.sql = offline
            .sql
            .replace("DEFAULT 'primary'", "DEFAULT 'IF NOT EXISTS primary'");
        assert!(
            bridge_plan(Backend::Hiqlite, 66, &objects).is_err(),
            "foreign default cannot normalize into a known lineage"
        );
    }
}
