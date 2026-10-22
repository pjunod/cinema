//! SQLite bridge for the common queue's authoritative SQL transitions.

use async_trait::async_trait;

use super::SqliteStore;
use crate::error::StoreError;
use crate::store::background_jobs::QueueSql;

#[async_trait]
impl QueueSql for SqliteStore {
    async fn queue_transaction(&self, statements: Vec<(String, String)>) -> Result<(), StoreError> {
        self.with_conn(move |connection| {
            #[cfg(test)]
            let mut timing = QueueTransactionTiming::enabled();
            let transaction = connection.unchecked_transaction()?;
            for (sql, request) in statements {
                let mut statement = {
                    #[cfg(test)]
                    let _prepare = timing.as_mut().map(|timing| {
                        timing.statement_count += 1;
                        timing.sql_bytes += sql.len();
                        QueuePhaseTiming::new(&mut timing.prepare_ns)
                    });
                    transaction.prepare_cached(&sql)?
                };
                {
                    #[cfg(test)]
                    let _step = timing
                        .as_mut()
                        .map(|timing| QueuePhaseTiming::new(&mut timing.step_ns));
                    let mut rows = statement.query([request])?;
                    while rows.next()?.is_some() {}
                }
                #[cfg(test)]
                if let Some(timing) = timing.as_mut() {
                    timing.completed_statements += 1;
                }
            }
            #[cfg(test)]
            {
                let result = {
                    let _commit = timing.as_mut().map(|timing| {
                        timing.commit_attempted = true;
                        QueuePhaseTiming::new(&mut timing.commit_ns)
                    });
                    transaction.commit().map_err(StoreError::from)
                };
                if let Some(timing) = timing.as_mut() {
                    timing.committed = result.is_ok();
                }
                result
            }
            #[cfg(not(test))]
            transaction.commit().map_err(StoreError::from)
        })
        .await
    }

    async fn queue_sql(
        &self,
        sql: String,
        request: String,
        _mutation: bool,
        _authoritative: bool,
    ) -> Result<Vec<String>, StoreError> {
        self.with_conn(move |connection| {
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map([request], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(StoreError::from)
        })
        .await
    }
}

// Timing is test-only, opt-in, and never records SQL text or bindings.
#[cfg(test)]
struct QueueTransactionTiming {
    started: std::time::Instant,
    prepare_ns: u128,
    step_ns: u128,
    commit_ns: u128,
    statement_count: usize,
    completed_statements: usize,
    sql_bytes: usize,
    commit_attempted: bool,
    committed: bool,
}

#[cfg(test)]
impl QueueTransactionTiming {
    fn enabled() -> Option<Self> {
        (std::env::var_os("PLURX_TEST_SETUP_TIMING").as_deref() == Some(std::ffi::OsStr::new("1")))
            .then(|| Self {
                started: std::time::Instant::now(),
                prepare_ns: 0,
                step_ns: 0,
                commit_ns: 0,
                statement_count: 0,
                completed_statements: 0,
                sql_bytes: 0,
                commit_attempted: false,
                committed: false,
            })
    }
}

#[cfg(test)]
impl Drop for QueueTransactionTiming {
    fn drop(&mut self) {
        use std::io::Write as _;
        let summary = serde_json::json!({
            "component": "sqlite_queue_transaction",
            "scope": "writer_lock_held",
            "transaction_ns": self.started.elapsed().as_nanos(),
            "prepare_ns": self.prepare_ns,
            "step_ns": self.step_ns,
            "commit_ns": self.commit_ns,
            // Includes a failed preparation; completed counts fully stepped statements.
            "statement_count": self.statement_count,
            "completed_statements": self.completed_statements,
            "sql_bytes": self.sql_bytes,
            "commit_attempted": self.commit_attempted,
            "committed": self.committed,
            "panicking": std::thread::panicking(),
        });
        // A closed diagnostic stream must not change transaction/error semantics.
        let _ = writeln!(
            std::io::stderr().lock(),
            "PLURX_TEST_SETUP_TIMING {summary}"
        );
    }
}

#[cfg(test)]
struct QueuePhaseTiming<'a> {
    started: std::time::Instant,
    elapsed_ns: &'a mut u128,
}

#[cfg(test)]
impl<'a> QueuePhaseTiming<'a> {
    fn new(elapsed_ns: &'a mut u128) -> Self {
        Self {
            started: std::time::Instant::now(),
            elapsed_ns,
        }
    }
}

#[cfg(test)]
impl Drop for QueuePhaseTiming<'_> {
    fn drop(&mut self) {
        *self.elapsed_ns += self.started.elapsed().as_nanos();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const INSERT_VALUE: &str = "INSERT INTO queue_cache_values(id,value)
        VALUES(json_extract($1,'$.id'),json_extract($1,'$.value')) RETURNING id,value";

    async fn install_fixture(store: &SqliteStore) {
        store.with_conn(|connection| {
            connection.execute_batch(
                "CREATE TABLE queue_cache_values(id INTEGER PRIMARY KEY,value TEXT NOT NULL CHECK(value<>'reject'));
                 CREATE TABLE queue_cache_events(value TEXT NOT NULL);
                 CREATE TRIGGER queue_cache_audit AFTER INSERT ON queue_cache_values
                 BEGIN INSERT INTO queue_cache_events(value) VALUES(NEW.value); END;",
            )?;
            Ok(())
        }).await.expect("real queue cache fixture");
    }

    fn insert(id: i64, value: &str) -> (String, String) {
        (
            INSERT_VALUE.into(),
            json!({"id":id,"value":value}).to_string(),
        )
    }

    async fn values(store: &SqliteStore) -> Vec<(i64, String)> {
        store
            .with_conn(|connection| {
                let mut statement =
                    connection.prepare("SELECT id,value FROM queue_cache_values ORDER BY id")?;
                let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
                rows.collect::<Result<Vec<_>, _>>()
                    .map_err(StoreError::from)
            })
            .await
            .expect("actual queue cache values")
    }

    async fn events(store: &SqliteStore) -> Vec<String> {
        store
            .with_conn(|connection| {
                let mut statement =
                    connection.prepare("SELECT value FROM queue_cache_events ORDER BY rowid")?;
                let rows = statement.query_map([], |row| row.get(0))?;
                rows.collect::<Result<Vec<_>, _>>()
                    .map_err(StoreError::from)
            })
            .await
            .expect("actual queue cache trigger effects")
    }

    #[tokio::test]
    async fn cached_queue_transactions_rebind_values_and_preserve_mixed_order() {
        let store = SqliteStore::open_in_memory().expect("store");
        install_fixture(&store).await;
        let update = "UPDATE queue_cache_values SET value=value||json_extract($1,'$.suffix')
            WHERE id=json_extract($1,'$.id') RETURNING id,value";
        store
            .queue_transaction(vec![
                insert(1, "first"),
                (update.into(), json!({"id":1,"suffix":":mixed"}).to_string()),
                insert(2, "second"),
            ])
            .await
            .expect("mixed first transaction");
        store
            .queue_transaction(vec![
                insert(3, "third"),
                (update.into(), json!({"id":2,"suffix":":fresh"}).to_string()),
            ])
            .await
            .expect("same cached SQL with fresh bindings");
        assert_eq!(
            values(&store).await,
            vec![
                (1, "first:mixed".into()),
                (2, "second:fresh".into()),
                (3, "third".into())
            ]
        );
        assert_eq!(events(&store).await, vec!["first", "second", "third"]);
    }

    #[tokio::test]
    async fn cached_queue_transactions_fully_execute_returning_and_streamed_rows() {
        let store = SqliteStore::open_in_memory().expect("store");
        install_fixture(&store).await;
        let returning = "INSERT INTO queue_cache_values(id,value)
            SELECT json_extract(value,'$.id'),json_extract(value,'$.value') FROM json_each($1) ORDER BY CAST(key AS INTEGER)
            RETURNING id,value";
        store
            .queue_transaction(vec![(
                returning.into(),
                json!([
                    {"id":1,"value":"one"},{"id":2,"value":"two"},{"id":3,"value":"three"}
                ])
                .to_string(),
            )])
            .await
            .expect("all returning inserts");
        assert_eq!(values(&store).await.len(), 3);
        assert_eq!(events(&store).await, vec!["one", "two", "three"]);

        const LATE_ERROR: &str =
            "SELECT CASE WHEN value=3 THEN json('not-json') ELSE value END FROM json_each($1)";
        // Establish that this is a real error after two successful rows, not
        // a preparation or first-step error that an early exit could catch.
        store
            .with_conn(|connection| {
                let mut statement = connection.prepare(LATE_ERROR)?;
                let mut rows = statement.query(["[1,2,3]"])?;
                assert_eq!(rows.next()?.expect("first row").get::<_, i64>(0)?, 1);
                assert_eq!(rows.next()?.expect("second row").get::<_, i64>(0)?, 2);
                assert!(rows.next().is_err(), "third row must fail");
                Ok(())
            })
            .await
            .expect("late-row control");
        let error = store
            .queue_transaction(vec![
                insert(4, "must-rollback"),
                (LATE_ERROR.into(), "[1,2,3]".into()),
            ])
            .await
            .expect_err("all rows must be stepped, including the failing third row");
        assert!(error.to_string().contains("malformed JSON"), "{error}");
        assert_eq!(values(&store).await.len(), 3);
        assert_eq!(events(&store).await, vec!["one", "two", "three"]);
        store
            .queue_transaction(vec![(
                returning.into(),
                json!([
                    {"id":4,"value":"four"},{"id":5,"value":"five"}
                ])
                .to_string(),
            )])
            .await
            .expect("cached returning SQL after failure");
        assert_eq!(
            values(&store).await,
            vec![
                (1, "one".into()),
                (2, "two".into()),
                (3, "three".into()),
                (4, "four".into()),
                (5, "five".into()),
            ]
        );
    }

    #[tokio::test]
    async fn cached_queue_transactions_rollback_rows_and_triggers_then_recover() {
        let store = SqliteStore::open_in_memory().expect("store");
        install_fixture(&store).await;
        store
            .queue_transaction(vec![insert(1, "warm")])
            .await
            .expect("warm cache");
        for (later_error, expected_error) in [
            (insert(3, "reject"), "CHECK constraint failed"),
            (
                (
                    "INSERT INTO missing_queue_cache_table(value) VALUES($1)".into(),
                    "absent".into(),
                ),
                "no such table: missing_queue_cache_table",
            ),
        ] {
            let error = store
                .queue_transaction(vec![insert(2, "must-rollback"), later_error])
                .await
                .expect_err("later row fails after the earlier write and trigger");
            assert!(error.to_string().contains(expected_error), "{error}");
            assert_eq!(values(&store).await, vec![(1, "warm".into())]);
            assert_eq!(events(&store).await, vec!["warm"]);
        }
        store
            .queue_transaction(vec![insert(2, "recovered"), insert(3, "fresh")])
            .await
            .expect("cached statement remains usable after rollback");
        assert_eq!(
            values(&store).await,
            vec![
                (1, "warm".into()),
                (2, "recovered".into()),
                (3, "fresh".into())
            ]
        );
        assert_eq!(events(&store).await, vec!["warm", "recovered", "fresh"]);
    }

    #[tokio::test]
    async fn cached_queue_transactions_observe_replaced_and_new_refusal_triggers() {
        let store = SqliteStore::open_in_memory().expect("store");
        install_fixture(&store).await;
        store
            .queue_transaction(vec![insert(1, "old")])
            .await
            .expect("warm old trigger");
        store
            .with_conn(|connection| {
                connection.execute_batch(
                    "DROP TRIGGER queue_cache_audit;
                 CREATE TRIGGER queue_cache_audit AFTER INSERT ON queue_cache_values
                 BEGIN INSERT INTO queue_cache_events(value) VALUES('new:'||NEW.value); END;",
                )?;
                Ok(())
            })
            .await
            .expect("replace actual schema trigger");
        store
            .queue_transaction(vec![insert(2, "changed")])
            .await
            .expect("new trigger on warmed SQL");
        assert_eq!(events(&store).await, vec!["old", "new:changed"]);
        store
            .with_conn(|connection| {
                connection.execute_batch(
                    "CREATE TRIGGER queue_cache_refuse BEFORE INSERT ON queue_cache_values
                 WHEN NEW.value='blocked' BEGIN SELECT RAISE(ABORT,'new schema refusal'); END;",
                )?;
                Ok(())
            })
            .await
            .expect("install new schema refusal");
        let error = store
            .queue_transaction(vec![insert(3, "blocked")])
            .await
            .expect_err("new refusal trigger must apply to warmed SQL");
        assert!(error.to_string().contains("new schema refusal"), "{error}");
        assert_eq!(
            values(&store).await,
            vec![(1, "old".into()), (2, "changed".into())]
        );
        assert_eq!(events(&store).await, vec!["old", "new:changed"]);
        store
            .queue_transaction(vec![insert(3, "allowed")])
            .await
            .expect("new schema accepts fresh binding");
        assert_eq!(
            events(&store).await,
            vec!["old", "new:changed", "new:allowed"]
        );
    }

    #[tokio::test]
    async fn cached_queue_transactions_are_durable_and_isolated_between_stores() {
        let directory = tempfile::tempdir().expect("fixture directory");
        let path_a = directory.path().join("a.sqlite");
        let path_b = directory.path().join("b.sqlite");
        let store_a = SqliteStore::open(&path_a).expect("store a");
        let store_b = SqliteStore::open(&path_b).expect("store b");
        install_fixture(&store_a).await;
        install_fixture(&store_b).await;
        store_a
            .queue_transaction(vec![insert(1, "a-one")])
            .await
            .expect("warm a");
        store_b
            .queue_transaction(vec![insert(1, "b-one")])
            .await
            .expect("warm b");
        store_a
            .queue_transaction(vec![insert(2, "a-two")])
            .await
            .expect("reuse a");
        store_b
            .queue_transaction(vec![insert(2, "b-two")])
            .await
            .expect("reuse b");
        assert_eq!(
            values(&store_a).await,
            vec![(1, "a-one".into()), (2, "a-two".into())]
        );
        assert_eq!(
            values(&store_b).await,
            vec![(1, "b-one".into()), (2, "b-two".into())]
        );
        drop(store_a);
        drop(store_b);
        let reopened_a = SqliteStore::open(&path_a).expect("durable reopen a");
        let reopened_b = SqliteStore::open(&path_b).expect("durable reopen b");
        assert_eq!(events(&reopened_a).await, vec!["a-one", "a-two"]);
        assert_eq!(events(&reopened_b).await, vec!["b-one", "b-two"]);
        reopened_a
            .queue_transaction(vec![insert(3, "a-three")])
            .await
            .expect("fresh cache after reopen");
        assert_eq!(
            values(&reopened_a).await,
            vec![
                (1, "a-one".into()),
                (2, "a-two".into()),
                (3, "a-three".into())
            ]
        );
        assert_eq!(
            values(&reopened_b).await,
            vec![(1, "b-one".into()), (2, "b-two".into())]
        );
        assert_eq!(events(&reopened_b).await, vec!["b-one", "b-two"]);
    }
}
