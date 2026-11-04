use super::*;

fn fixture() -> (tempfile::TempDir, PathBuf, StorageManifest) {
    let directory = tempfile::tempdir().expect("owned native fixture");
    let path = directory.path().join("plurx.db");
    let manifest = StorageManifest {
        version: 1,
        mode: StorageMode::Sqlite,
        database: "plurx.db".into(),
        instance_id: "retained-instance".into(),
        node_id: "retained-node".into(),
        generation: uuid::Uuid::new_v4().to_string(),
        schema_version: SQLITE_SCHEMA_VERSION,
    };
    let connection = Connection::open(&path).expect("source");
    connection.execute_batch(r#"CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT NOT NULL); INSERT INTO settings VALUES('instance.id','retained-instance');
        CREATE TABLE storage_identity(singleton INTEGER PRIMARY KEY,instance_id TEXT NOT NULL,node_id TEXT NOT NULL,generation TEXT NOT NULL) STRICT;
        CREATE TABLE records(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT NOT NULL,amount REAL,bytes BLOB,missing TEXT) STRICT;
        INSERT INTO records VALUES(5,'unicode ☃ / quote "" \\ line',1.5,x'0001ff',NULL);
        INSERT INTO records VALUES(900,'deleted',NULL,NULL,NULL); DELETE FROM records WHERE id=900;
        CREATE TABLE compound(a TEXT,b INTEGER,payload BLOB,PRIMARY KEY(a,b)) WITHOUT ROWID, STRICT;
        INSERT INTO compound VALUES('first',-9223372036854775808,x'00ff'); INSERT INTO compound VALUES('last',9223372036854775807,x'ff00');
        CREATE TABLE item_identity_watermark(singleton INTEGER PRIMARY KEY,high_water INTEGER NOT NULL,importing INTEGER NOT NULL) STRICT; INSERT INTO item_identity_watermark VALUES(1,12345,0);
        CREATE TABLE sharing_purpose_transaction_guard(singleton INTEGER PRIMARY KEY,passed INTEGER NOT NULL) STRICT; INSERT INTO sharing_purpose_transaction_guard VALUES(1,1);
        CREATE TABLE sharing_source_schema_transaction_guard(singleton INTEGER PRIMARY KEY,passed INTEGER NOT NULL) STRICT; INSERT INTO sharing_source_schema_transaction_guard VALUES(1,1);"#).expect("native fixture schema");
    connection
        .execute(
            "INSERT INTO storage_identity VALUES(1,?1,?2,?3)",
            rusqlite::params![manifest.instance_id, manifest.node_id, manifest.generation],
        )
        .expect("source binding");
    connection
        .pragma_update(None, "user_version", SQLITE_SCHEMA_VERSION)
        .expect("current native schema");
    drop(connection);
    (directory, path, manifest)
}

fn inspect(path: &Path, manifest: &StorageManifest) -> Result<SourceInventory, StoreError> {
    inspect_source(path, &super::super::sha256_file(path)?, manifest)
}

fn copy_fixture_table(source: &Path, target: &Connection, table: &Table) {
    let columns = write_columns(table).expect("insert projection");
    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        quoted(&table.name),
        columns
            .iter()
            .map(|name| quoted(name))
            .collect::<Vec<_>>()
            .join(","),
        (1..=columns.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let mut cursor = None;
    loop {
        let chunk = source_page(source, table, cursor).expect("bounded typed source page");
        if chunk.rows.is_empty() {
            break;
        }
        cursor = chunk.cursor;
        for row in chunk.rows {
            let row = row
                .iter()
                .map(sqlite_value)
                .collect::<Result<Vec<_>, _>>()
                .expect("typed bindings");
            target
                .execute(&sql, rusqlite::params_from_iter(row))
                .expect("apply exact row");
        }
    }
}

#[test]
fn current_native_census_preserves_typed_rows_ids_and_deleted_high_water() {
    let (_directory, path, manifest) = fixture();
    let source = inspect(&path, &manifest).expect("verified current native image");
    assert!(
        !source.principal_layout,
        "pre-Source native snapshots retain their real predecessor layout"
    );
    let target = Connection::open_in_memory().expect("target codec fixture");
    target.execute_batch("CREATE TABLE records(missing TEXT,bytes BLOB,amount REAL,name TEXT NOT NULL,id INTEGER PRIMARY KEY AUTOINCREMENT) STRICT;
        CREATE TABLE compound(a TEXT,b INTEGER,payload BLOB,PRIMARY KEY(a,b)) WITHOUT ROWID, STRICT;
        CREATE TABLE item_identity_watermark(singleton INTEGER PRIMARY KEY,high_water INTEGER NOT NULL,importing INTEGER NOT NULL) STRICT;").expect("same logical target with different physical column order");
    for name in ["records", "compound", "item_identity_watermark"] {
        copy_fixture_table(&path, &target, &source.tables[name]);
    }
    target
        .execute("DELETE FROM sqlite_sequence", [])
        .expect("replace derived import sequence");
    copy_fixture_table(&path, &target, &source.tables["sqlite_sequence"]);
    let target_tables = source_inventory(&target).expect("target inventory");
    for name in [
        "records",
        "compound",
        "item_identity_watermark",
        "sqlite_sequence",
    ] {
        let actual = source_digest(&target, &target_tables[name]).expect("target typed digest");
        assert_eq!(
            actual,
            (
                source.tables[name].row_count,
                source.tables[name].digest.clone()
            ),
            "{name}"
        );
    }
    target
        .execute("INSERT INTO records(name) VALUES('after promotion')", [])
        .expect("preserved allocator");
    assert_eq!(target.last_insert_rowid(), 901);
    assert_eq!(
        target
            .query_row(
                "SELECT high_water FROM item_identity_watermark",
                [],
                |row| row.get::<_, i64>(0)
            )
            .expect("native import fixture"),
        12345
    );
}

#[test]
fn unknown_durable_native_rows_and_columns_are_refused() {
    let (_directory, path, manifest) = fixture();
    let source = inspect(&path, &manifest).expect("source");
    let mut target = source.tables.clone();
    target.remove("records");
    assert!(
        plan_tables(&source.tables, &target).is_err(),
        "unknown nonempty native table must not disappear"
    );
    let mut target = source.tables.clone();
    target
        .get_mut("records")
        .expect("native import fixture")
        .columns
        .retain(|column| column.name != "bytes");
    assert!(
        plan_tables(&source.tables, &target).is_err(),
        "BLOB data needs an exact replicated column"
    );
    let mut source = source.tables;
    source
        .get_mut("storage_identity")
        .expect("native import fixture")
        .columns
        .push(Column {
            name: "future_custody".into(),
            kind: "TEXT".into(),
            pk: 0,
            hidden: 0,
        });
    assert!(
        plan_tables(&source, &target).is_err(),
        "observation classification cannot discard future durable fields"
    );
}

#[test]
fn persistent_installation_guards_are_application_data_and_copy_unchanged() {
    let (_directory, path, manifest) = fixture();
    let source = inspect(&path, &manifest)
        .expect("persistent passed receipts are valid, not pending transactions");
    let (plans, census) = plan_tables(&source.tables, &source.tables).expect("exact shared layout");
    for name in [
        "sharing_purpose_transaction_guard",
        "sharing_source_schema_transaction_guard",
    ] {
        assert!(plans
            .iter()
            .any(|table| table.name == name && table.row_count == 1));
        assert_eq!(
            census
                .iter()
                .find(|entry| entry.table == name)
                .expect("native import fixture")
                .disposition,
            "copied-replicated-data"
        );
    }
    assert_eq!(
        census
            .iter()
            .find(|entry| entry.table == "storage_identity")
            .expect("native import fixture")
            .disposition,
        "retired-local-observation"
    );
}

#[test]
fn immutable_native_provenance_refuses_wrong_generation_hash_and_sidecars() {
    let (_directory, path, manifest) = fixture();
    let mut wrong = manifest.clone();
    wrong.generation = uuid::Uuid::new_v4().to_string();
    assert!(inspect(&path, &wrong).is_err());
    assert!(inspect_source(&path, &"0".repeat(64), &manifest).is_err());
    std::fs::write(path.with_file_name("plurx.db-wal"), b"uncheckpointed")
        .expect("fixture sidecar");
    assert!(inspect(&path, &manifest).is_err());
}

#[test]
fn active_source_custody_refuses_promotion_but_ended_history_is_preserved() {
    let (_directory, path, manifest) = fixture();
    let connection = Connection::open(&path).expect("source");
    connection.execute_batch("CREATE TABLE sharing_source_session_bindings(reservation_state TEXT); INSERT INTO sharing_source_session_bindings VALUES('held');").expect("held custody fixture");
    drop(connection);
    assert!(inspect(&path, &manifest).is_err());
    let connection = Connection::open(&path).expect("native import fixture");
    connection
        .execute(
            "UPDATE sharing_source_session_bindings SET reservation_state='released'",
            [],
        )
        .expect("native import fixture");
    drop(connection);
    assert!(
        inspect(&path, &manifest).is_ok(),
        "released retained history is durable data"
    );
}

#[test]
fn parent_first_items_keep_causal_fk_order_and_refuse_cycles() {
    let directory = tempfile::tempdir().expect("native import fixture");
    let path = directory.path().join("parents.db");
    let connection = Connection::open(&path).expect("native import fixture");
    connection.execute_batch("CREATE TABLE items(id INTEGER PRIMARY KEY,parent_id INTEGER); INSERT INTO items VALUES(2,9),(9,NULL),(1,2);").expect("native import fixture");
    assert_eq!(
        parent_first_items(&path).expect("native import fixture"),
        vec![9, 2, 1]
    );
    connection
        .execute("UPDATE items SET parent_id=1 WHERE id=9", [])
        .expect("native import fixture");
    assert!(parent_first_items(&path).is_err());
}

#[test]
fn source_key_cursor_preserves_i64_extremes_and_binary_values() {
    let raw = serde_json::json!([
        ["integer", "-9223372036854775808"],
        ["integer", "9223372036854775807"],
        ["blob", "00FF"],
        ["text", "☃"],
        ["null", null]
    ]);
    let values = decode_params(&raw).expect("typed cursor");
    assert!(matches!(values[0], Param::Integer(i64::MIN)));
    assert!(matches!(values[1], Param::Integer(i64::MAX)));
    assert!(matches!(&values[2],Param::Blob(value) if value==&[0,255]));
    assert!(encoded_bytes(&values).expect("native import fixture") > 0);
}

#[test]
fn only_closed_fts_families_are_rebuilt_and_unknown_virtual_data_refuses() {
    let (_directory, path, manifest) = fixture();
    let source = inspect(&path, &manifest).expect("source");
    let mut table = source.tables["records"].clone();
    table.kind = "virtual".into();
    for name in ["items_fts", "items_fts_vocab", "classification_fts"] {
        table.name = name.into();
        assert!(derived(&table));
    }
    table.kind = "shadow".into();
    for name in [
        "items_fts_config",
        "classification_fts_config",
        "classification_fts_content",
    ] {
        table.name = name.into();
        assert!(derived(&table));
    }
    for kind in ["virtual", "shadow"] {
        table.kind = kind.into();
        table.name = "classification_fts_future_custody".into();
        assert!(!derived(&table));
        assert!(
            plan_tables(
                &BTreeMap::from([(table.name.clone(), table.clone())]),
                &BTreeMap::new()
            )
            .is_err(),
            "unknown nonempty generated-looking domain data cannot disappear"
        );
    }
}
