//! Test-only coordinated-upgrade control process. This is not a migration
//! coordinator: it opens only runner-owned fixtures and starts no HTTP/media
//! workers. Production installation and capability advertisements stay absent.
#[cfg(not(feature = "hiqlite-store"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("qualification requires --features hiqlite-store".into())
}

#[cfg(feature = "hiqlite-store")]
mod qualification {
    use hiqlite::{params, Client, Row};
    use plurx_core::cluster::membership::{
        sharing_member_admission_guard_schema, sharing_member_transition_absence_predicate,
    };
    use plurx_core::cluster::migration::select_daemon_store;
    use plurx_core::config::Config;
    use plurx_core::store::{
        SqliteStore, AUTH_SCHEMA_VERSION, MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA,
        SQLITE_SCHEMA_VERSION,
    };
    use serde_json::{json, Value};
    use std::collections::BTreeMap;
    use std::path::Path;
    use tokio::io::{AsyncBufReadExt, BufReader};

    type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
    const TABLES: [&str; 11] = [
        "media_session_requests",
        "media_playback_pointers",
        "media_sessions",
        "media_session_preparations",
        "media_playback_desired",
        "media_session_producer_recovery",
        "library_channel_session_recipes",
        "job_leases",
        "media_session_terminal_acks",
        "sharing_relay_upstream",
        "sharing_delivery_grants",
    ];
    struct TextRow(String);
    impl From<&mut Row<'_>> for TextRow {
        fn from(row: &mut Row<'_>) -> Self {
            Self(row.get("value"))
        }
    }

    fn fixture_sql() -> String {
        // Existing daemon setup owns user 1. Keep the frozen fixture file
        // unchanged, seed only its remaining retained rows, and mark routes
        // terminal before any daemon restart. This does not prove active-media
        // drain, which requires the completed writer implementation.
        let fixture = include_str!("../tests/fixtures/session-principal-local.sql");
        let (_, body) = fixture.split_once(';').expect("fixture's user insert");
        let now = chrono::Utc::now().timestamp_millis();
        let future = now + 3_600_000;
        format!("{}\nUPDATE media_session_requests SET state='resolved',response_json='{{}}',claim_expires_at_ms={future},updated_at_ms={now};\nUPDATE media_sessions SET state='ended',terminal_reason='admin_stop',drain_deadline_ms=NULL,lease_expires_at_ms={future},updated_at_ms={now};\nUPDATE media_session_preparations SET deadline_ms={future},created_at_ms={now},updated_at_ms={now};\nUPDATE media_session_terminal_acks SET expires_at_ms={future},updated_at_ms={now};\nUPDATE job_leases SET expires_at_ms={future},updated_at_ms={now} WHERE resource='session:00000000-0000-4000-a000-000000000072';\nUPDATE sharing_delivery_grants SET deadline_ms={future};\nUPDATE media_playback_desired SET updated_at_ms={now};\nUPDATE media_playback_pointers SET updated_at_ms={now};\nUPDATE media_session_producer_recovery SET created_at_ms={now},updated_at_ms={now};\nUPDATE library_channel_session_recipes SET created_at_ms={now};",body.replace("'live'","'00000000-0000-4000-a000-000000000072'").replace("('ended',","('00000000-0000-4000-a000-000000000073',").replace("'staged'","'00000000-0000-4000-a000-000000000074'").replace("session:live","session:00000000-0000-4000-a000-000000000072"))
    }

    async fn snapshot(client: &Client) -> Result<Value> {
        let mut result = BTreeMap::new();
        for table in TABLES {
            let columns = client
                .query_consistent_map::<TextRow, _>(
                    format!("SELECT name AS value FROM pragma_table_info('{table}') ORDER BY cid"),
                    params!(),
                )
                .await?
                .into_iter()
                .map(|row| row.0)
                .collect::<Vec<_>>();
            let fields = columns
                .iter()
                .map(|column| format!("'{column}',\"{column}\""))
                .collect::<Vec<_>>()
                .join(",");
            let rows = client
                .query_consistent_map::<TextRow, _>(
                    format!("SELECT json_object({fields}) AS value FROM {table} ORDER BY 1"),
                    params!(),
                )
                .await?
                .into_iter()
                .map(|row| serde_json::from_str::<Value>(&row.0))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            result.insert(table, json!({"columns":columns,"rows":rows}));
        }
        Ok(json!(result))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn terminal_retention_fixture_obeys_production_sqlite_constraints() {
            let directory = tempfile::tempdir().expect("fixture directory");
            let path = directory.path().join("plurx.db");
            drop(SqliteStore::open(&path).expect("production schema"));
            let connection = rusqlite::Connection::open(&path).expect("fixture connection");
            connection.execute("INSERT INTO users (id, username, password_hash, is_admin, created_at) VALUES (1, 'owner', 'fixture-hash', 1, 1)", []).expect("daemon-owned user");
            connection
                .execute_batch(&fixture_sql())
                .expect("retained fixture must obey real schema checks");
            let ended: i64 = connection.query_row("SELECT COUNT(*) FROM media_sessions WHERE state='ended' AND terminal_reason='admin_stop' AND drain_deadline_ms IS NULL", [], |row| row.get(0)).expect("terminal inventory");
            assert_eq!(ended, 2);
            let unresolved: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM media_session_requests WHERE state != 'resolved'",
                    [],
                    |row| row.get(0),
                )
                .expect("request inventory");
            assert_eq!(unresolved, 0);
            let observed = chrono::Utc::now().timestamp_millis();
            for (table, column) in [
                ("media_session_requests", "updated_at_ms"),
                ("media_sessions", "updated_at_ms"),
                ("media_session_preparations", "created_at_ms"),
                ("media_playback_desired", "updated_at_ms"),
                ("media_playback_pointers", "updated_at_ms"),
                ("media_session_producer_recovery", "created_at_ms"),
                ("library_channel_session_recipes", "created_at_ms"),
                ("media_session_terminal_acks", "updated_at_ms"),
            ] {
                let oldest: i64 = connection
                    .query_row(&format!("SELECT MIN({column}) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .expect("seeded retention clock");
                assert!(
                    (observed - 10_000..=observed).contains(&oldest),
                    "stale fixture clock {table}.{column}"
                );
            }
        }
    }

    pub async fn run() -> Result<()> {
        let args = std::env::args().skip(1).collect::<Vec<_>>();
        match args.as_slice() {
            [mode, path] if mode == "sqlite-init" => {
                drop(SqliteStore::open(Path::new(path))?);
                println!(
                    "QUALIFICATION {}",
                    json!({"sqlite_version":SQLITE_SCHEMA_VERSION,"replicated_version":AUTH_SCHEMA_VERSION})
                );
                Ok(())
            }
            [mode, path] if mode == "node" => {
                let config = Config::load(Some(Path::new(path)))?;
                let selected = select_daemon_store(&config).await?;
                let client = selected
                    .local_client()
                    .ok_or("fixture unexpectedly fell back to SQLite")?;
                tokio::time::timeout(
                    std::time::Duration::from_secs(60),
                    client.wait_until_healthy_db(),
                )
                .await?;
                println!(
                    "QUALIFICATION {}",
                    json!({"ready":true,"node_id":selected.identity.node_id})
                );
                let mut input = BufReader::new(tokio::io::stdin()).lines();
                while let Some(command) = input.next_line().await? {
                    let response = match command.as_str() {
                        "seed" => {
                            for outcome in client.batch(fixture_sql()).await? {
                                outcome?;
                            }
                            json!({"seeded":true})
                        }
                        "snapshot" => json!({"snapshot":snapshot(&client).await?}),
                        "factory" => {
                            let metrics = client.metrics_db().await?;
                            if metrics.membership_config.voter_ids().count() != 3 {
                                return Err("factory fixture requires three actual voters".into());
                            }
                            for result in client
                                .txn(
                                    sharing_member_admission_guard_schema()
                                        .into_iter()
                                        .map(|sql| (sql, params!()))
                                        .collect::<Vec<_>>(),
                                )
                                .await?
                            {
                                result?;
                            }
                            json!({"factory":true,"voters":3})
                        }
                        "rebuild" => {
                            client.execute("CREATE TABLE qualification_transition_guard(value INTEGER NOT NULL CHECK(value=1))",params!()).await?;
                            let mut statements=vec![(format!("INSERT INTO qualification_transition_guard SELECT CASE WHEN {} THEN 1 ELSE 0 END",sharing_member_transition_absence_predicate()),params!())];
                            statements.extend(
                                MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA
                                    .split("-- next statement\n")
                                    .map(|sql| {
                                        (sql.trim().trim_end_matches(';').to_owned(), params!())
                                    }),
                            );
                            for result in client.txn(statements).await? {
                                result?;
                            }
                            // No Store methods are called after a shape change:
                            // its cached shape is fixed until coordinated restart.
                            json!({"rebuild":true,"shared_admission":false,"capabilities_advertised":false})
                        }
                        "future" => {
                            client
                                .execute(
                                    "UPDATE cluster_meta SET schema_version=$1 WHERE singleton=1",
                                    params!(AUTH_SCHEMA_VERSION + 1),
                                )
                                .await?;
                            json!({"future_replicated_version":AUTH_SCHEMA_VERSION+1})
                        }
                        "quit" => break,
                        _ => return Err("unknown qualification command".into()),
                    };
                    println!("QUALIFICATION {response}");
                }
                selected.shutdown().await?;
                Ok(())
            }
            _ => Err("use sqlite-init <new database> or node <runner-owned config>".into()),
        }
    }
}

#[cfg(feature = "hiqlite-store")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    qualification::run().await
}
