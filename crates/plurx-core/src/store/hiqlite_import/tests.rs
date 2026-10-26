use super::*;

#[test]
fn source_guard_validation_rejects_every_malformed_link_and_allows_repairs() {
    let source = Connection::open_in_memory().expect("source");
    source
        .execute_batch(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, library_id INTEGER NOT NULL);
                 CREATE TABLE files (
                   id INTEGER PRIMARY KEY, item_id INTEGER NOT NULL, path TEXT NOT NULL);
                 CREATE TABLE dv_conversions (
                   file_id INTEGER PRIMARY KEY, state TEXT NOT NULL,
                   original_path TEXT, recovery_guard_id TEXT);
                 CREATE TABLE dv_recovery_guards (
                   guard_id TEXT PRIMARY KEY, file_id INTEGER NOT NULL,
                   library_id INTEGER NOT NULL, source_path TEXT NOT NULL,
                   state TEXT NOT NULL);
                 INSERT INTO items VALUES (1, 7);
                 INSERT INTO files VALUES (11, 1, '/library/movie.mkv');
                 INSERT INTO dv_conversions
                   VALUES (11, 'verified', NULL, 'guard-11');
                 INSERT INTO dv_recovery_guards
                   VALUES ('guard-11', 11, 7, '/wrong/movie.mkv', 'intent');",
        )
        .expect("fixture");

    let malformed_verified =
        verify_source_dv_recovery_guards(&source, 44).expect_err("reject verified link");
    assert!(
        malformed_verified
            .to_string()
            .contains("exact recovery guard linkage"),
        "{malformed_verified}"
    );
    source
        .execute(
            "UPDATE dv_recovery_guards SET source_path = '/library/movie.mkv'",
            [],
        )
        .expect("repair verified link");
    verify_source_dv_recovery_guards(&source, 44).expect("accept repaired verified link");

    source
        .execute_batch(
            "UPDATE dv_conversions
                    SET state = 'committed', original_path = '/library/movie.mkv.p7.orig';
                 UPDATE dv_recovery_guards SET library_id = 8, state = 'active';",
        )
        .expect("break retained-original link");
    let malformed_retained = verify_source_dv_recovery_guards(&source, 44)
        .expect_err("retained original does not excuse malformed recovery link");
    assert!(
        malformed_retained
            .to_string()
            .contains("exact recovery guard linkage"),
        "{malformed_retained}"
    );
    source
        .execute("UPDATE dv_recovery_guards SET library_id = 7", [])
        .expect("repair retained-original link");
    verify_source_dv_recovery_guards(&source, 44).expect("accept repaired retained-original link");
}

#[test]
fn incremental_digest_matches_the_original_ordered_json_contract() {
    let rows = vec![
        r#"[1,"alpha",null]"#.to_owned(),
        r#"[2,"quote: \"",17]"#.to_owned(),
        r#"[10,"unicode: ☃",0]"#.to_owned(),
    ];
    let expected = hex::encode(Sha256::digest(
        serde_json::to_vec(&rows).expect("serialize reference rows"),
    ));
    let mut digest = OrderedRowsHasher::new();
    for row in &rows {
        digest.push(row).expect("hash row");
    }
    assert_eq!(
        digest.finish(),
        OrderedRowsDigest {
            row_count: 3,
            sha256: expected,
        }
    );
}

#[test]
fn target_parity_uses_bounded_multi_column_keyset_pages() {
    let table = TABLES
        .iter()
        .find(|table| table.name == "transcode_cache_locations")
        .copied()
        .expect("location table plan");
    let (first_sql, first_params) = target_parity_page(table, &[]).expect("first page");
    assert!(!first_sql.contains(" WHERE "));
    assert!(first_sql.ends_with("LIMIT $1"));
    assert_eq!(first_params, vec![Param::Integer(PARITY_PAGE_ROWS)]);

    let cursor = parity_cursor_params(table, r#"["recipe","node","ssd"]"#).expect("decode cursor");
    let (next_sql, next_params) = target_parity_page(table, &cursor).expect("next page");
    assert!(next_sql.contains("WHERE (recipe_hash, node_id, storage_class) > ($1, $2, $3)"));
    assert!(next_sql.ends_with("LIMIT $4"));
    assert_eq!(next_params.len(), 4);
    assert_eq!(next_params[3], Param::Integer(PARITY_PAGE_ROWS));
}

#[tokio::test(flavor = "current_thread")]
async fn source_reader_blocking_work_keeps_the_async_executor_responsive() {
    let data = tempfile::tempdir().expect("data dir");
    let path = data.path().join(crate::cluster::migration::SQLITE_FILENAME);
    crate::store::SqliteStore::open(&path).expect("source store");
    let prepared =
        crate::cluster::migration::prepare_sqlite_import(data.path()).expect("prepared backup");
    let (reader, _) = SourceReader::open(
        &prepared.backup_path,
        &prepared.backup_sha256,
        prepared.schema_version,
    )
    .await
    .expect("source reader");

    let pause = reader.pause(std::time::Duration::from_millis(500));
    tokio::pin!(pause);
    tokio::select! {
        result = &mut pause => panic!("blocking source work returned early: {result:?}"),
        () = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
    }
    pause.await.expect("source pause completes");
}

#[test]
fn v14_outbox_projection_supplies_unleased_claim_deadline() {
    let table = TABLES
        .iter()
        .find(|table| table.name == "watched_outbox")
        .copied()
        .expect("watched_outbox plan");
    let v14 = value_projection(table, 14, false);
    let current = value_projection(table, SQLITE_SCHEMA_VERSION, false);
    assert!(v14.ends_with("updated_at, 0"));
    assert!(current.ends_with("updated_at, claim_until"));
}

#[test]
fn pre_v18_offline_projection_supplies_legacy_vbr_snapshot() {
    let table = TABLES
        .iter()
        .find(|table| table.name == "offline_packages")
        .copied()
        .expect("offline package plan");
    let v17 = value_projection(table, 17, false);
    let current = value_projection(table, SQLITE_SCHEMA_VERSION, false);
    assert!(v17.contains("recipe_hash, 0, 'primary', NULL, 'vbr', target_height"));
    assert!(current.contains(
            "recipe_hash, claim_generation, decoder_recovery_state, alternate_recipe_hash, effective_rate_control, target_height"
        ));
}

#[test]
fn pre_audio_recipe_offline_projection_preserves_legacy_audio_and_current_snapshot() {
    use crate::store::sqlite::{MIGRATIONS, OFFLINE_AUDIO_RECIPE_SCHEMA};
    let table = TABLES
        .iter()
        .find(|table| table.name == "offline_packages")
        .copied()
        .expect("offline table import contract");
    // The column lands at v92 on the composed chain; the effort's private
    // numbering (v88) is a different migration on main.
    assert_eq!(OFFLINE_AUDIO_RECIPE_SCHEMA, 92);
    assert_eq!(
        MIGRATIONS[OFFLINE_AUDIO_RECIPE_SCHEMA as usize - 1],
        "ALTER TABLE offline_packages ADD COLUMN audio_recipe TEXT;"
    );
    assert!(value_projection(table, 88, false).ends_with("expires_at, NULL"));
    assert!(
        value_projection(table, OFFLINE_AUDIO_RECIPE_SCHEMA - 1, false)
            .ends_with("expires_at, NULL")
    );
    assert!(value_projection(table, OFFLINE_AUDIO_RECIPE_SCHEMA, false)
        .ends_with("expires_at, audio_recipe"));
    assert!(
        value_projection(table, SQLITE_SCHEMA_VERSION, false).ends_with("expires_at, audio_recipe")
    );
}

#[test]
fn pre_v54_offline_projection_supplies_unspent_recovery_claim() {
    let table = TABLES
        .iter()
        .find(|table| table.name == "offline_packages")
        .copied()
        .expect("offline package plan");
    let v53 = value_projection(table, 53, false);
    let current = value_projection(table, SQLITE_SCHEMA_VERSION, false);
    assert!(v53.contains("recipe_hash, 0, 'primary', NULL, effective_rate_control, target_height"));
    assert!(current.contains(
            "recipe_hash, claim_generation, decoder_recovery_state, alternate_recipe_hash, effective_rate_control, target_height"
        ));
}

#[test]
fn pre_v21_item_projection_supplies_null_book_facts() {
    let table = TABLES
        .iter()
        .find(|table| table.name == "items")
        .copied()
        .expect("items table plan");
    let v20 = value_projection(table, 20, false);
    let current = value_projection(table, SQLITE_SCHEMA_VERSION, false);
    assert!(v20.ends_with("artwork_error, genres, NULL, NULL, NULL, NULL"));
    assert!(current.ends_with(
        "artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source"
    ));
}

/// A backup taken before the Dolby Vision columns existed still imports.
///
/// The import opens the source read-only and never migrates it, and it
/// accepts every schema back to `MINIMUM_IMPORT_SCHEMA_VERSION` — so a
/// column added to the plan without a projection arm turns the whole
/// clustering import into `no such column`, on the one path an operator
/// takes exactly once and cannot retry differently.
#[test]
fn pre_v38_file_projection_supplies_null_dolby_vision_facts() {
    let table = TABLES
        .iter()
        .find(|table| table.name == "files")
        .copied()
        .expect("files table plan");
    let v37 = value_projection(table, 37, false);
    let current = value_projection(table, SQLITE_SCHEMA_VERSION, false);
    assert!(
        v37.ends_with(
            "hdr_format, audio_offset_ms, NULL, NULL, NULL, NULL, NULL, NULL, NULL, \
                 NULL, NULL, NULL, NULL, '[]'"
        ),
        "{v37}"
    );
    assert!(
            current.ends_with(
                "hdr_format, audio_offset_ms, dv_profile, dv_level, dv_bl_compat_id, \
                 dv_el_present, dv_rpu_present, video_codec_tag, field_order, \
                 max_cll, max_fall, mastering_max_luminance, luminance_source, downloaded_subtitles"
            ),
            "{current}"
        );
}

#[test]
fn pre_v60_file_projection_supplies_null_video_codec_tag() {
    let table = TABLES
        .iter()
        .find(|table| table.name == "files")
        .copied()
        .expect("files table plan");
    let v59 = value_projection(table, 59, false);
    let current = value_projection(table, SQLITE_SCHEMA_VERSION, false);
    assert!(
        v59.ends_with("dv_el_present, dv_rpu_present, NULL, NULL, NULL, NULL, NULL, NULL, '[]'"),
        "{v59}"
    );
    assert!(current.ends_with(
        "dv_el_present, dv_rpu_present, video_codec_tag, field_order, max_cll, max_fall, \
             mastering_max_luminance, luminance_source, downloaded_subtitles"
    ));
}

#[test]
fn pre_v64_file_projection_supplies_null_field_order() {
    let table = TABLES
        .iter()
        .find(|table| table.name == "files")
        .copied()
        .expect("files table plan");
    let v63 = value_projection(table, 63, false);
    let current = value_projection(table, SQLITE_SCHEMA_VERSION, false);
    assert!(
        v63.ends_with("dv_rpu_present, video_codec_tag, NULL, NULL, NULL, NULL, NULL, '[]'"),
        "{v63}"
    );
    assert!(current.ends_with(
        "dv_rpu_present, video_codec_tag, field_order, max_cll, max_fall, \
             mastering_max_luminance, luminance_source, downloaded_subtitles"
    ));
}

/// The luminance columns append one step after the field-order column, so
/// a v64 source predates them and has to project explicit nulls.
#[test]
fn pre_v65_file_projection_supplies_null_luminance() {
    let table = TABLES
        .iter()
        .find(|table| table.name == "files")
        .copied()
        .expect("files table plan");
    let v64 = value_projection(table, 64, false);
    let current = value_projection(table, SQLITE_SCHEMA_VERSION, false);
    assert!(
        v64.ends_with("video_codec_tag, field_order, NULL, NULL, NULL, NULL, '[]'"),
        "{v64}"
    );
    assert!(current.ends_with(
        "video_codec_tag, field_order, max_cll, max_fall, mastering_max_luminance, \
             luminance_source, downloaded_subtitles"
    ));
}

#[test]
fn pre_v67_file_projection_supplies_empty_downloaded_subtitles() {
    let table = files_plan();
    assert!(value_projection(table, 66, false).ends_with("luminance_source, '[]'"));
    assert!(value_projection(table, SQLITE_SCHEMA_VERSION, false)
        .ends_with("luminance_source, downloaded_subtitles"));
}

/// Every import plan must name the migration that actually creates its
/// table.
///
/// `minimum_schema` is compared against the *source database's* `PRAGMA
/// user_version`, so it lives on the SQLite migration number line and
/// nothing else. `media_playback_desired` had the replicated AUTH version
/// there instead — a different number line that happens to contain small
/// integers too — which would have sent a source between those two
/// numbers to count and read a table it does not have. Nothing catches
/// that until a restore, which is the worst place to find it.
///
/// Checked against `MIGRATIONS` rather than against a hand-kept list, so
/// a table added in a later migration cannot be given an earlier number
/// and pass. Tables created before the migration list began, or by an
/// `ALTER` rather than a `CREATE`, have no creating migration to find and
/// are exempted by name — deliberately a short list that has to be edited
/// deliberately.
#[test]
fn every_import_plan_names_the_migration_that_creates_its_table() {
    // Tables whose creation predates the migration list, so there is no
    // entry to point at. Each is present from v1.
    const PREDATES_THE_LIST: &[&str] = &[];
    let migrations = crate::store::sqlite::MIGRATIONS;
    for table in TABLES {
        if PREDATES_THE_LIST.contains(&table.name) {
            continue;
        }
        let creates = |sql: &str| {
            sql.contains(&format!("CREATE TABLE IF NOT EXISTS {}", table.name))
                || sql.contains(&format!("CREATE TABLE {}", table.name))
        };
        let Some(index) = migrations.iter().position(|sql| creates(sql)) else {
            continue;
        };
        let creating_version = index as i64 + 1;
        assert!(
            table.minimum_schema >= creating_version,
            "{}: minimum_schema {} is below v{creating_version}, the migration that \
                 creates the table — a source in between would be asked to read a table \
                 it does not have",
            table.name,
            table.minimum_schema,
        );
    }
}

#[test]
fn replicated_tables_exclude_node_local_and_derived_state() {
    let names = TABLES.iter().map(|table| table.name).collect::<Vec<_>>();
    assert!(!names.contains(&"playback_events"));
    assert!(
        !names.contains(&"fragment_indexes"),
        "a fragment index describes one machine's ffmpeg output; replicating \
             it would let one node's byte counts place another node's landings"
    );
    assert!(!names.contains(&"items_fts"));
    assert!(!names.contains(&"offline_lease_guards"));
    assert!(names.contains(&"media_session_requests"));
    assert!(names.contains(&"media_playback_pointers"));
    assert!(names.contains(&"media_sessions"));
    assert!(names.contains(&"media_session_terminal_acks"));
    assert!(names.contains(&"quality_cancellation_receipts"));
    assert!(names.contains(&"quality_preparation_owners"));
    assert!(names.contains(&"continuous_quality_ledgers"));
    assert!(names.contains(&"cache_storage_members"));
    assert!(names.contains(&"cache_consumer_pins"));
    assert!(names.contains(&"cluster_fragment_index_sources"));
    assert!(names.contains(&"cluster_fragment_index_jobs"));
    assert!(names.contains(&"cluster_fragment_index_artifacts"));
    assert!(names.contains(&"cluster_fragment_index_locations"));
    assert!(names.contains(&"dv_conversions"));
    assert!(names.contains(&"dv_recovery_guards"));
    assert!(names.contains(&"analysis_requests"));
    assert!(names.contains(&"analysis_attempts"));
    assert!(names.contains(&"cluster_fragment_index_heads"));
    assert!(names.contains(&"analysis_lifecycle_counters"));
    assert!(names.contains(&"timeline_annotation_sets"));
    assert!(names.contains(&"timeline_manual_overrides"));
    assert!(names.contains(&"media_playback_desired"));
    // The DVR's three durable tables. Each is replicated rather than
    // node-local on purpose: a recording is scheduled on any node, written
    // by the owner, and served by all of them, so the row has to outlive
    // the node that made it. None of the three carries derived state — the
    // capture's bytes live on the DVR root and the sidecar, not here.
    assert!(names.contains(&"dvr_recordings"));
    assert!(names.contains(&"dvr_rules"));
    assert!(names.contains(&"dvr_reminders"));
    assert!(names.contains(&"media_classifications"));
    assert!(names.contains(&"file_grants"));
    for name in [
        "background_library_requests",
        "background_job_domain_leases",
        "background_provider_budgets",
        "background_storage_domains",
    ] {
        assert!(names.contains(&name));
    }
    assert!(!names.contains(&"classification_fts"));
    assert!(names.contains(&"live_tv_resource_records"));
    // The revision/nonce is reconstructed above the greatest restored epoch.
    assert!(!names.contains(&"live_tv_resource_revision"));
    for name in [
        "sharing_identity",
        "sharing_invitations",
        "sharing_exports",
        "sharing_export_libraries",
        "sharing_imports",
        "sharing_viewers",
        "sharing_assignments",
        "sharing_watch",
        "sharing_rotations",
        "sharing_import_rotations",
        "sharing_catalogue_revisions",
        "sharing_endpoint_manifest",
        "sharing_relay_upstream",
        "sharing_delivery_grants",
        "sharing_ingress_custody",
    ] {
        assert!(names.contains(&name), "durable sharing table: {name}");
    }
    // Authenticated candidate failures are replicated: any node may serve
    // the next attempt, and each must refuse the recipe that failed. The
    // Link samples beside them are one node's measurements and never
    // reach Raft.
    assert!(names.contains(&"candidate_recovery"));
    assert!(!names.contains(&"candidate_link_priors"));
    assert!(names.contains(&"jellyfin_login_tokens"));
    assert!(names.contains(&"jellyfin_entity_ids"));
    assert!(names.contains(&"jellyfin_plays"));
    // v113 adds three durable annotation tables; the schema authority marker
    // is installed locally and is not an imported catalogue row population.
    for table in [
        "library_annotations_lists",
        "library_annotations_ratings",
        "library_annotations_rating_fetches",
    ] {
        assert!(names.contains(&table), "durable annotation table: {table}");
    }
    assert!(!names.contains(&"library_annotations_schema"));
    // Playback handoff codes and tokens are durable account state.
    assert!(names.contains(&"playback_handoffs"));
    assert!(names.contains(&"playback_handoffs_tokens"));
    // 95 with the three item-annotation tables (SQLite v113), 97 with the
    // two playback handoff tables (SQLite v115).
    assert_eq!(names.len(), 97, "review every imported durable table");
}

/// A source from before the pointer fence has no revision to attribute its
/// pointers to, and the import must say so rather than invent one.
///
/// `NULL` is the whole point. The fence reads a null on a playback that
/// *has* an ask as a writer from before the column, so a restore that
/// filled this in with the ask that happens to be current would hand every
/// imported pointer a token it never earned — which is the "never replace
/// a missing expected token with the current revision" rule, arriving
/// through a restore instead of through a write. It is safe precisely
/// because such a source has no asks either: the ask table arrived one
/// version earlier and the fence only fires where an ask exists.
#[test]
fn a_pre_fence_source_projects_a_null_pointer_revision() {
    let table = TABLES
        .iter()
        .find(|table| table.name == "media_playback_pointers")
        .copied()
        .expect("pointer table plan");
    assert!(
        value_projection(table, 48, false).ends_with("updated_at_ms, NULL"),
        "a source from before the column carries no revision for its pointers"
    );
    assert!(
        value_projection(table, SQLITE_SCHEMA_VERSION, false)
            .ends_with("updated_at_ms, desired_revision"),
        "and a current source carries the one its pointers were written against"
    );
}

#[test]
fn pre_v44_conversion_projection_supplies_a_null_recovery_guard_link() {
    let table = TABLES
        .iter()
        .find(|table| table.name == "dv_conversions")
        .copied()
        .expect("conversion table plan");
    assert!(value_projection(table, 43, false).ends_with("finished_at_ms, NULL, 0"));
    assert!(value_projection(table, SQLITE_SCHEMA_VERSION, false)
        .ends_with("finished_at_ms, recovery_guard_id, requested_manually"));
}

/// A source below v46 has no attempt history, and the import must carry
/// the column anyway.
///
/// A plan that omits a column silently drops it: `import_table` projects
/// and inserts only `table.columns`, and the parity digest is computed
/// from that same projection on both sides, so source and target agree
/// about a value neither of them carried. An activation import of a
/// library full of dead jobs would land with every history blank — the
/// exact defect this column exists to fix, at the moment an operator is
/// most likely to be looking for it.
#[test]
fn pre_v46_fragment_index_jobs_project_an_empty_attempt_history() {
    let table = TABLES
        .iter()
        .find(|table| table.name == "cluster_fragment_index_jobs")
        .copied()
        .expect("fragment-index job table plan");
    let v45 = value_projection(table, 45, false);
    assert!(v45.contains("last_error_code, '', created_at_ms"), "{v45}");
    let current = value_projection(table, SQLITE_SCHEMA_VERSION, false);
    assert!(
        current.contains("last_error_code, attempt_errors, created_at_ms"),
        "{current}"
    );
}

#[test]
fn pre_v26_cache_locations_project_stable_local_storage_identity() {
    let table = TABLES
        .iter()
        .find(|table| table.name == "transcode_cache_locations")
        .copied()
        .expect("cache location table plan");
    let v25 = value_projection(table, 25, false);
    let v53 = value_projection(table, 53, false);
    let current = value_projection(table, SQLITE_SCHEMA_VERSION, false);
    assert!(
        v25.ends_with("'node:' || node_id || ':cache', relative_dir, 0"),
        "{v25}"
    );
    assert!(v53.ends_with("storage_id, generation_id, 0"), "{v53}");
    assert!(
        current.ends_with("storage_id, generation_id, publication_generation"),
        "{current}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a05_recovery_import_preserves_spent_scope_and_legacy_epoch_projection() {
    let sessions = TABLES
        .iter()
        .find(|t| t.name == "media_sessions")
        .copied()
        .expect("recovery import fixture");
    let memory = TABLES
        .iter()
        .find(|t| t.name == "candidate_recovery")
        .copied()
        .expect("recovery import fixture");
    let introduced = crate::store::candidate_recovery::SQLITE_INTRODUCED_SCHEMA;
    assert_eq!(
        introduced, 96,
        "candidate_recovery lands at v96 on the composed chain"
    );
    assert_eq!(memory.minimum_schema, introduced);
    for version in [51_i64, 53, introduced - 1, introduced] {
        let dir = tempfile::tempdir().expect("recovery import fixture");
        let path = dir.path().join("source.db");
        let source = Connection::open(&path).expect("recovery import fixture");
        crate::store::sqlite::SqliteStore::apply_migrations_for_test(&source, version)
            .expect("recovery import fixture");
        source
            .execute(
                "INSERT INTO settings(key,value,updated_at) VALUES(?1,?2,1)",
                rusqlite::params![keys::INSTANCE_ID, "a05-import"],
            )
            .expect("recovery import fixture");
        source.execute("INSERT INTO media_sessions(incarnation_id,session_id,user_id,playback_id,request_fingerprint,owner_node_id,owner_epoch,lease_expires_at_ms,state,recipe_json,response_json,updated_at_ms) VALUES('incarnation','session',42,'player','request','owner',7,9000,'active','{}','{}',1)", []).expect("recovery import fixture");
        if version >= 53 {
            source.execute("UPDATE media_sessions SET recovery_epoch='d23b2032-2910-469a-bf76-3c0b12735028'", []).expect("recovery import fixture");
        }
        if version >= memory.minimum_schema {
            for (recipe, step) in [("11".repeat(32), 1_i64), ("22".repeat(32), 0)] {
                source.execute("INSERT INTO candidate_recovery(scope,recipe,cause,event_id,user_id,playback_id,recovery_epoch,created_ms,quality_step) VALUES('exact-authenticated-scope',?1,'decode','original-event',42,'player','d23b2032-2910-469a-bf76-3c0b12735028',1234,?2)", rusqlite::params![recipe,step]).expect("recovery import fixture");
            }
        }
        drop(source);
        let hash = sha256_file(&path).expect("recovery import fixture");
        let (reader, _) = SourceReader::open(&path, &hash, version)
            .await
            .expect("recovery import fixture");
        let rows = reader
            .import_chunk(sessions, version, SourceChunk::Offset(0))
            .await
            .expect("recovery import fixture");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].last(),
            Some(&Param::Text(if version < 53 {
                String::new()
            } else {
                "d23b2032-2910-469a-bf76-3c0b12735028".to_owned()
            }))
        );
        if version < memory.minimum_schema {
            // Production import_table skips before asking an older source
            // for a table it cannot contain; parity uses an empty digest.
            let source = Connection::open(&path).expect("recovery import fixture");
            assert_eq!(
                source_digest(&source, version, memory).expect("recovery import fixture"),
                OrderedRowsHasher::new().finish()
            );
            continue;
        }
        assert_eq!(
            reader
                .count(memory, true)
                .await
                .expect("recovery import fixture"),
            2
        );
        let recovered = reader
            .import_chunk(memory, version, SourceChunk::Offset(0))
            .await
            .expect("recovery import fixture");
        assert_eq!(recovered.len(), 2);
        let target = Connection::open_in_memory().expect("recovery import fixture");
        target
            .execute_batch(crate::store::candidate_recovery::SCHEMA)
            .expect("recovery import fixture");
        for row in recovered {
            let values: Vec<rusqlite::types::Value> = row
                .into_iter()
                .map(|value| match value {
                    Param::Text(text) => rusqlite::types::Value::Text(text),
                    Param::Integer(number) => rusqlite::types::Value::Integer(number),
                    _ => panic!("unexpected recovery import type"),
                })
                .collect();
            let sql = format!(
                "INSERT INTO candidate_recovery({}) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                memory.columns.join(",")
            );
            target
                .execute(&sql, rusqlite::params_from_iter(values))
                .expect("recovery import fixture");
        }
        let original = Connection::open(&path).expect("recovery import fixture");
        assert_eq!(
            source_digest(&original, version, memory).expect("recovery import fixture"),
            source_digest(&target, version, memory).expect("recovery import fixture")
        );
        let spent: i64 = target.query_row("SELECT SUM(quality_step) FROM candidate_recovery WHERE recovery_epoch='d23b2032-2910-469a-bf76-3c0b12735028' AND created_ms=1234 AND event_id='original-event'", [], |row| row.get(0)).expect("recovery import fixture");
        assert_eq!(
            spent, 1,
            "import preserves original spent admission without replay or timestamp refresh"
        );
        let mut statement = target
            .prepare("SELECT recipe FROM candidate_recovery ORDER BY recipe")
            .expect("recovery import fixture");
        let recipes = statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("recovery import fixture")
            .collect::<Result<Vec<_>, _>>()
            .expect("recovery import fixture");
        assert_eq!(
            recipes,
            vec!["11".repeat(32), "22".repeat(32)],
            "full recipe identities survive import"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn media_session_import_reads_real_legacy_schema_boundaries() {
    let table = TABLES
        .iter()
        .find(|table| table.name == "media_sessions")
        .copied()
        .expect("media session table plan");
    for schema_version in [33_i64, 34, 35, 50, 51] {
        let data = tempfile::tempdir().expect("source dir");
        let path = data.path().join("legacy.db");
        {
            let conn = Connection::open(&path).expect("raw source open");
            crate::store::sqlite::SqliteStore::apply_migrations_for_test(&conn, schema_version)
                .expect("apply legacy migrations");
            conn.execute(
                "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![keys::INSTANCE_ID, "legacy-instance", 1_i64],
            )
            .expect("seed source instance");

            // This is the actual v25 media_sessions shape; v34 and v35
            // append their columns through the real migration chain.
            let columns = match schema_version {
                33 => {
                    "incarnation_id, session_id, user_id, playback_id,
                           request_fingerprint, owner_node_id, owner_epoch,
                           lease_expires_at_ms, state, recipe_json, response_json,
                           produced_playable_through_ms, fetched_through_ms,
                           media_origin_ms, media_sequence, discontinuity_sequence,
                           updated_at_ms"
                }
                34 => {
                    "incarnation_id, session_id, user_id, playback_id,
                           request_fingerprint, owner_node_id, owner_epoch,
                           lease_expires_at_ms, state, terminal_reason, recipe_json,
                           response_json, produced_playable_through_ms,
                           fetched_through_ms, media_origin_ms, media_sequence,
                           discontinuity_sequence, updated_at_ms"
                }
                35 => {
                    "incarnation_id, session_id, user_id, playback_id,
                           request_fingerprint, owner_node_id, owner_epoch,
                           lease_expires_at_ms, state, terminal_reason,
                           publication_ready_at_ms, recipe_json, response_json,
                           produced_playable_through_ms, fetched_through_ms,
                           media_origin_ms, media_sequence, discontinuity_sequence,
                           updated_at_ms"
                }
                50 => {
                    "incarnation_id, session_id, user_id, playback_id,
                           request_fingerprint, owner_node_id, owner_epoch,
                           lease_expires_at_ms, state, terminal_reason,
                           publication_ready_at_ms, recipe_json, response_json,
                           produced_playable_through_ms, fetched_through_ms,
                           media_origin_ms, media_sequence, discontinuity_sequence,
                           updated_at_ms"
                }
                51 => {
                    "incarnation_id, session_id, user_id, playback_id,
                           request_fingerprint, owner_node_id, owner_epoch,
                           lease_expires_at_ms, state, terminal_reason,
                           publication_ready_at_ms, recipe_json, response_json,
                           produced_playable_through_ms, fetched_through_ms,
                           media_origin_ms, media_sequence, discontinuity_sequence,
                           updated_at_ms, drain_deadline_ms"
                }
                _ => unreachable!(),
            };
            let values = match schema_version {
                33 => {
                    "?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                           ?12, ?13, ?14, ?15, ?16, ?17"
                }
                34 => {
                    "?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                           ?12, ?13, ?14, ?15, ?16, ?17, ?18"
                }
                35 => {
                    "?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                           ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19"
                }
                50 => {
                    "?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                           ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19"
                }
                51 => {
                    "?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                           ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20"
                }
                _ => unreachable!(),
            };
            let insert_sql = format!("INSERT INTO media_sessions ({columns}) VALUES ({values})");
            match schema_version {
                33 => conn.execute(
                    &insert_sql,
                    rusqlite::params![
                        "incarnation",
                        "session",
                        42_i64,
                        "playback",
                        "fingerprint",
                        "owner",
                        7_i64,
                        8_000_i64,
                        "ended",
                        "{\"recipe\":true}",
                        "{\"response\":true}",
                        1_000_i64,
                        2_000_i64,
                        3_000_i64,
                        4_i64,
                        5_i64,
                        9_000_i64,
                    ],
                ),
                34 => conn.execute(
                    &insert_sql,
                    rusqlite::params![
                        "incarnation",
                        "session",
                        42_i64,
                        "playback",
                        "fingerprint",
                        "owner",
                        7_i64,
                        8_000_i64,
                        "ended",
                        "superseded",
                        "{\"recipe\":true}",
                        "{\"response\":true}",
                        1_000_i64,
                        2_000_i64,
                        3_000_i64,
                        4_i64,
                        5_i64,
                        9_000_i64,
                    ],
                ),
                35 | 50 => conn.execute(
                    &insert_sql,
                    rusqlite::params![
                        "incarnation",
                        "session",
                        42_i64,
                        "playback",
                        "fingerprint",
                        "owner",
                        7_i64,
                        8_000_i64,
                        "ended",
                        "superseded",
                        8_765_i64,
                        "{\"recipe\":true}",
                        "{\"response\":true}",
                        1_000_i64,
                        2_000_i64,
                        3_000_i64,
                        4_i64,
                        5_i64,
                        9_000_i64,
                    ],
                ),
                51 => conn.execute(
                    &insert_sql,
                    rusqlite::params![
                        "incarnation",
                        "session",
                        42_i64,
                        "playback",
                        "fingerprint",
                        "owner",
                        7_i64,
                        8_000_i64,
                        "ended",
                        "superseded",
                        8_765_i64,
                        "{\"recipe\":true}",
                        "{\"response\":true}",
                        1_000_i64,
                        2_000_i64,
                        3_000_i64,
                        4_i64,
                        5_i64,
                        9_000_i64,
                        9_876_i64,
                    ],
                ),
                _ => unreachable!(),
            }
            .expect("seed source media session");
        }

        let expected_sha256 = sha256_file(&path).expect("hash source");
        let (reader, metadata) = SourceReader::open(&path, &expected_sha256, schema_version)
            .await
            .expect("open source reader");
        assert_eq!(metadata.schema_version, schema_version);
        assert_eq!(reader.count(table, true).await.expect("count source"), 1);
        let rows = reader
            .import_chunk(table, schema_version, SourceChunk::Offset(0))
            .await
            .expect("read source import chunk");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 21, "media session import parameter count");
        let expected_terminal = if schema_version < 34 {
            Param::Null
        } else {
            Param::Text("superseded".to_owned())
        };
        let expected_publication = if schema_version < 35 {
            Param::Integer(0)
        } else {
            Param::Integer(8_765)
        };
        assert_eq!(
            rows[0],
            vec![
                Param::Text("incarnation".to_owned()),
                Param::Text("session".to_owned()),
                Param::Integer(42),
                Param::Text("playback".to_owned()),
                Param::Text("fingerprint".to_owned()),
                Param::Text("owner".to_owned()),
                Param::Integer(7),
                Param::Integer(8_000),
                Param::Text("ended".to_owned()),
                expected_terminal,
                expected_publication,
                Param::Text("{\"recipe\":true}".to_owned()),
                Param::Text("{\"response\":true}".to_owned()),
                Param::Integer(1_000),
                Param::Integer(2_000),
                Param::Integer(3_000),
                Param::Integer(4),
                Param::Integer(5),
                Param::Integer(9_000),
                if schema_version < 51 {
                    Param::Null
                } else {
                    Param::Integer(9_876)
                },
                Param::Text(String::new()),
            ],
            "schema v{schema_version} import row",
        );
    }
}

/// The v30 queue predates `analysis_requests`, while v31-v40 queues have
/// no target column of their own. Exercise the real read-only import SQL
/// (including its ordering and digest paths) at both boundaries so a new
/// target projection cannot make an otherwise supported backup unreadable.
#[tokio::test(flavor = "current_thread")]
async fn fragment_index_job_import_reads_real_v30_and_v40_schemas() {
    let table = TABLES
        .iter()
        .find(|table| table.name == "cluster_fragment_index_jobs")
        .copied()
        .expect("fragment-index job plan");

    for schema_version in [30_i64, 40] {
        let data = tempfile::tempdir().expect("source dir");
        let path = data.path().join("legacy.db");
        {
            let conn = Connection::open(&path).expect("raw source open");
            crate::store::sqlite::SqliteStore::apply_migrations_for_test(&conn, schema_version)
                .expect("apply legacy migrations");
            conn.execute(
                "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![keys::INSTANCE_ID, "legacy-instance", 1_i64],
            )
            .expect("seed source instance");
            conn.execute(
                "INSERT INTO cluster_fragment_index_jobs
                        (cache_key, file_id, source_size, source_mtime,
                         source_sha256, pipeline_sha256, state, owner_node_id,
                         fence, lease_expires_ms, attempts, not_before_ms,
                         last_error_code, created_at_ms, updated_at_ms)
                     VALUES ('legacy-key', 7, 100, 20, 'source-sha',
                             'pipeline-sha', 'queued', NULL, 0, NULL, 0,
                             30, NULL, 30, 30)",
                [],
            )
            .expect("seed legacy job");
            if schema_version >= 31 {
                conn.execute(
                    "INSERT INTO analysis_requests
                            (request_id, file_id, source_size, source_mtime,
                             component, force_rebuild, target_node_id, state,
                             not_before_ms, result_cache_key, created_at_ms,
                             updated_at_ms)
                         VALUES ('legacy-request', 7, 100, 20,
                                 'fragment_index', 0, 'node-legacy', 'queued',
                                 30, 'legacy-key', 30, 30)",
                    [],
                )
                .expect("seed legacy request target");
            }
        }

        let expected_sha256 = sha256_file(&path).expect("hash source");
        let (reader, metadata) = SourceReader::open(&path, &expected_sha256, schema_version)
            .await
            .expect("open source reader");
        assert_eq!(metadata.schema_version, schema_version);
        let rows = reader
            .import_chunk(table, schema_version, SourceChunk::Offset(0))
            .await
            .expect("read source import chunk");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), table.columns.len());
        assert_eq!(rows[0][6], Param::Text("normal".to_owned()));
        assert_eq!(rows[0][7], Param::Text("background".to_owned()));
        assert_eq!(
            rows[0][8],
            Param::Text(if schema_version == 30 {
                String::new()
            } else {
                "node-legacy".to_owned()
            })
        );
        assert_eq!(
            reader
                .digest(table, schema_version)
                .await
                .expect("digest legacy source")
                .row_count,
            1
        );
    }
}

#[tokio::test]
async fn background_jobs_import_distinguishes_v70_legacy_from_v71_queue() {
    for version in [70, 71] {
        let directory = tempfile::tempdir().expect("source directory");
        let path = directory.path().join("source.db");
        {
            let connection = Connection::open(&path).expect("source");
            crate::store::sqlite::SqliteStore::apply_migrations_for_test(&connection, version)
                .expect("literal schema");
            connection.execute("INSERT INTO settings (key, value, updated_at) VALUES (?1, 'queue-import-fixture', 1)", [keys::INSTANCE_ID]).expect("instance");
            if version == 71 {
                connection.execute_batch("INSERT INTO background_jobs
                        (id, kind, payload_version, payload_json, dedupe_key, priority, state, not_before_ms, created_at_ms, updated_at_ms)
                        VALUES ('fixture', 'artifact_hydrate', 1, '{}', 'fixture', 1, 'queued', 0, 0, 0);").expect("queued state");
            }
        }
        let digest = sha256_file(&path).expect("source hash");
        let (reader, _) = SourceReader::open(&path, &digest, version)
            .await
            .expect("reader");
        for table in TABLES
            .iter()
            .copied()
            .filter(|table| table.name.starts_with("background_"))
        {
            let introduced = match table.name {
                "background_job_domain_leases" => 72,
                "background_library_requests" => 73,
                "background_storage_domains" => 74,
                "background_provider_budgets" => 75,
                "background_artwork_locations" => 77,
                "background_transcode_artifacts" => 78,
                "background_predictions" => 79,
                "background_embeddings" => 80,
                "background_artifact_repairs" => 82,
                _ => 71,
            };
            assert_eq!(table.minimum_schema, introduced, "{}", table.name);
            let rows = if version >= table.minimum_schema {
                reader
                    .import_chunk(table, version, SourceChunk::Offset(0))
                    .await
                    .expect("queue table projection at source boundary")
            } else {
                Vec::new()
            };
            let expected = i64::from(
                version == 71
                    && matches!(table.name, "background_jobs" | "background_job_migration"),
            );
            assert_eq!(rows.len() as i64, expected, "v{version} {}", table.name);
            assert_eq!(
                reader
                    .digest(table, version)
                    .await
                    .expect("queue digest")
                    .row_count as i64,
                expected
            );
        }
    }
}

#[test]
fn insert_parameters_follow_hiqlite_numeric_order() {
    for table in TABLES {
        super::super::hiqlite::validate_sql(&insert_sql(*table))
            .unwrap_or_else(|error| panic!("{} insert is invalid: {error}", table.name));
    }
}

fn files_plan() -> TablePlan {
    *TABLES
        .iter()
        .find(|table| table.name == "files")
        .expect("files import plan")
}

/// A `files` row whose `probe_json` is padded to make the whole row
/// estimate `row_bytes`, so a test can sit a row deliberately either side
/// of a bound.
fn sized_files_row(id: i64, row_bytes: usize, statement_bytes: usize) -> hiqlite::Params {
    let table = files_plan();
    let mut row = vec![Param::Integer(id)];
    row.extend(std::iter::repeat_n(Param::Null, table.columns.len() - 2));
    row.push(Param::Text(String::new()));
    let overhead = statement_bytes + estimated_row_bytes(&row);
    let padding = row_bytes
        .checked_sub(overhead)
        .expect("requested row is smaller than its own framing");
    let last = row.len() - 1;
    row[last] = Param::Text("x".repeat(padding));
    assert_eq!(statement_bytes + estimated_row_bytes(&row), row_bytes);
    row
}

fn files_statement_bytes() -> usize {
    insert_sql(files_plan()).len() + PER_STATEMENT_FRAMING_BYTES
}

/// The bound that replaced the row count: a run of rows small enough to
/// clear the row ceiling still splits once their bytes reach the budget.
#[test]
fn a_row_crossing_the_byte_budget_starts_a_new_transaction() {
    let statement_bytes = files_statement_bytes();
    let half = IMPORT_TXN_BUDGET_BYTES / 2;
    let mut builder = ImportTransactionBuilder::new(files_plan(), &insert_sql(files_plan()));

    assert!(
        builder
            .push(sized_files_row(1, half, statement_bytes))
            .expect("first large row is accepted")
            .is_none(),
        "the first row cannot fill a transaction on its own"
    );
    let flushed = builder
        .push(sized_files_row(2, half, statement_bytes))
        .expect("second large row is accepted")
        .expect("two rows summing past the budget must emit the first transaction");
    assert_eq!(flushed.len(), 1, "the crossing row belongs to the next txn");
    let last = builder.finish().expect("the crossing row is still pending");
    assert_eq!(last.len(), 1);
    assert!(
        builder.finish().is_none(),
        "finish must not re-emit rows already submitted"
    );
}

/// Immediately below and immediately above the budget, measured on the
/// same rows the importer would submit.
#[test]
fn the_budget_boundary_splits_exactly_one_byte_late() {
    let statement_bytes = files_statement_bytes();
    let head = IMPORT_TXN_BUDGET_BYTES / 2;
    let exact = IMPORT_TXN_BUDGET_BYTES - IMPORT_TXN_ENVELOPE_BYTES - head;

    let mut fitting = ImportTransactionBuilder::new(files_plan(), &insert_sql(files_plan()));
    fitting
        .push(sized_files_row(1, head, statement_bytes))
        .expect("head row");
    assert!(
        fitting
            .push(sized_files_row(2, exact, statement_bytes))
            .expect("row landing exactly on the budget")
            .is_none(),
        "a row filling the budget exactly still belongs to the same transaction"
    );
    assert_eq!(
        fitting.finish().expect("pending transaction").len(),
        2,
        "both rows must be submitted together"
    );

    let mut crossing = ImportTransactionBuilder::new(files_plan(), &insert_sql(files_plan()));
    crossing
        .push(sized_files_row(1, head, statement_bytes))
        .expect("head row");
    assert!(
        crossing
            .push(sized_files_row(2, exact + 1, statement_bytes))
            .expect("row landing one byte past the budget")
            .is_some(),
        "one byte past the budget must start a new transaction"
    );
}

/// Small rows must not pay for the large ones: the byte budget only splits
/// where bytes demand it, and the row ceiling still bounds the rest.
#[test]
fn mixed_row_sizes_flush_on_bytes_and_on_the_row_ceiling() {
    let statement_bytes = files_statement_bytes();
    let table = files_plan();
    let large = IMPORT_TXN_BUDGET_BYTES * 3 / 5;
    let mut builder = ImportTransactionBuilder::new(table, &insert_sql(table));
    let mut submitted = Vec::new();

    for id in 0..40_i64 {
        let row = if id % 10 == 0 {
            sized_files_row(id, large, statement_bytes)
        } else {
            sized_files_row(id, statement_bytes + 512, statement_bytes)
        };
        if let Some(ready) = builder.push(row).expect("row within bounds") {
            submitted.push(ready);
        }
    }
    if let Some(ready) = builder.finish() {
        submitted.push(ready);
    }

    assert_eq!(
        submitted.iter().map(Vec::len).sum::<usize>(),
        40,
        "every row must be submitted exactly once"
    );
    assert!(
        submitted.len() > 40 / IMPORT_CHUNK_ROWS as usize,
        "large rows must force flushes the row ceiling alone would not: {} transactions",
        submitted.len()
    );
    for transaction in &submitted {
        assert!(
            transaction.len() <= IMPORT_CHUNK_ROWS as usize,
            "the row ceiling remains a secondary bound"
        );
        let bytes = IMPORT_TXN_ENVELOPE_BYTES
            + transaction
                .iter()
                .map(|row| statement_bytes + estimated_row_bytes(row))
                .sum::<usize>();
        assert!(
            bytes <= HIQLITE_WAL_USABLE_PAYLOAD_BYTES,
            "no emitted transaction may exceed the WAL payload capacity: {bytes} bytes"
        );
    }
}

/// A row too large for the budget but small enough for the WAL is carried
/// alone rather than refused; refusing it would fail an import the WAL can
/// actually accept.
#[test]
fn a_row_above_the_budget_but_inside_the_wal_is_submitted_alone() {
    let statement_bytes = files_statement_bytes();
    let table = files_plan();
    let mut builder = ImportTransactionBuilder::new(table, &insert_sql(table));
    builder
        .push(sized_files_row(1, statement_bytes + 512, statement_bytes))
        .expect("small row");
    let flushed = builder
        .push(sized_files_row(2, IMPORT_MAX_ROW_BYTES, statement_bytes))
        .expect("a row inside the single-row limit is accepted")
        .expect("the small row is submitted first");
    assert_eq!(flushed.len(), 1);
    assert_eq!(
        builder.finish().expect("oversized row still pending").len(),
        1,
        "a row larger than the budget travels as its own transaction"
    );
}

/// The WAL increase is real single-entry headroom, not only a larger
/// segment allocation. This historical ceiling is the measured usable
/// payload from the former 2 MiB tuning; reverting the production constant
/// makes this row fail the importer's single-row check again.
#[test]
fn raised_wal_accepts_a_row_past_the_former_payload_ceiling() {
    let statement_bytes = files_statement_bytes();
    let table = files_plan();
    let mut builder = ImportTransactionBuilder::new(table, &insert_sql(table));
    builder
        .push(sized_files_row(1, 2_100 * 1024, statement_bytes))
        .expect("the raised WAL accepts a row beyond the former payload ceiling");
    assert_eq!(
        builder.finish().expect("raised-headroom row pending").len(),
        1
    );
}

/// The failure #290 recorded in production, converted into a refusal: a row
/// no transaction can carry must never reach `txn`, where the WAL writer
/// panics and the node restarts into unreplicated SQLite recovery.
#[test]
fn a_row_larger_than_the_wal_is_refused_with_an_actionable_error() {
    let statement_bytes = files_statement_bytes();
    let table = files_plan();
    let mut builder = ImportTransactionBuilder::new(table, &insert_sql(table));
    let error = builder
        .push(sized_files_row(
            4321,
            IMPORT_MAX_ROW_BYTES + 1,
            statement_bytes,
        ))
        .expect_err("a row above the single-row limit must be refused");
    let message = error.to_string();
    assert!(message.contains("files"), "names the table: {message}");
    assert!(message.contains("row id=4321"), "names the row: {message}");
    assert!(
        message.contains(&(IMPORT_MAX_ROW_BYTES + 1).to_string()),
        "names the measured size: {message}"
    );
    assert!(
        message.contains(&IMPORT_MAX_ROW_BYTES.to_string()),
        "names the limit: {message}"
    );
    assert!(
        !message.contains("xxxx"),
        "an operator-facing error must not quote imported payload: {message}"
    );
    assert!(
        builder.finish().is_none(),
        "a refused row must not be left pending for submission"
    );
}

/// Tables keyed by text — `tokens.token_hash`, `settings.key` — must fall
/// back to a position rather than quote the key itself.
#[test]
fn a_refusal_on_a_text_keyed_table_names_a_position_not_the_key() {
    let table = *TABLES
        .iter()
        .find(|table| table.name == "tokens")
        .expect("tokens import plan");
    let insert = insert_sql(table);
    let mut builder = ImportTransactionBuilder::new(table, &insert);
    let mut row = vec![Param::Text("secret-token-hash".to_owned())];
    row.extend(std::iter::repeat_n(Param::Null, table.columns.len() - 2));
    row.push(Param::Text("y".repeat(IMPORT_MAX_ROW_BYTES)));
    let message = builder
        .push(row)
        .expect_err("an oversized token row must be refused")
        .to_string();
    assert!(
        !message.contains("secret-token-hash"),
        "a credential hash must never reach an error message: {message}"
    );
    assert!(
        message.contains("row 1 of the token_hash import order"),
        "falls back to the deterministic import position: {message}"
    );
}

/// The estimate exists to be an upper bound. If it ever under-counts the
/// encoder, every bound above it is decoration.
///
/// Measured against `legacy()` because that is the config `hiqlite-wal` 0.14
/// appends log entries with, and therefore the encoding whose length the
/// writer thread panics on. `standard()` is asserted alongside it as cheap
/// insurance should hiqlite ever switch.
#[test]
fn the_row_estimate_stays_above_what_the_wal_encoder_produces() {
    let rows: Vec<hiqlite::Params> = vec![
        vec![],
        vec![Param::Null, Param::Integer(i64::MIN), Param::Real(-1.5)],
        vec![
            Param::Integer(i64::MAX),
            Param::Text("☃".repeat(4096)),
            Param::Text(String::new()),
        ],
        vec![Param::Blob(vec![0xab; 8192])],
        // Integer- and text-heavy rows: the per-param slack is the whole
        // point of the estimate, so exercise many params rather than a few
        // large ones. A `files` row is this shape.
        std::iter::repeat_n(Param::Integer(i64::MAX), 64).collect(),
        std::iter::repeat_n(Param::Text("path/segment".to_owned()), 64).collect(),
        (0..64)
            .map(|index| match index % 5 {
                0 => Param::Null,
                1 => Param::Integer(-index),
                2 => Param::Real(index as f64),
                3 => Param::Text("x".repeat(index as usize)),
                _ => Param::Blob(vec![0x5a; index as usize]),
            })
            .collect(),
        // Never built by import, but the estimate claims to cover them.
        vec![
            Param::StmtOutputIndexed(usize::MAX, usize::MAX),
            Param::StmtOutputNamed(usize::MAX, "column_name".into()),
        ],
    ];
    for row in rows {
        let estimate = estimated_row_bytes(&row);
        let legacy = bincode::serde::encode_to_vec(&row, bincode::config::legacy())
            .expect("encode row the way hiqlite-wal's log store does");
        let standard = bincode::serde::encode_to_vec(&row, bincode::config::standard())
            .expect("encode row under the varint config");
        assert!(
            estimate >= legacy.len(),
            "estimate {estimate} under-counted the {}-byte legacy encoding of a \
                 {}-param row",
            legacy.len(),
            row.len()
        );
        assert!(
            estimate >= standard.len(),
            "estimate {estimate} under-counted the {}-byte standard encoding of a \
                 {}-param row",
            standard.len(),
            row.len()
        );
    }
}
