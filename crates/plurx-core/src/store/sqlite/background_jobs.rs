//! SQLite bridge for the common queue's authoritative SQL transitions.

use async_trait::async_trait;

use super::SqliteStore;
use crate::error::StoreError;
use crate::store::background_jobs::QueueSql;

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
                    transaction.prepare(&sql)?
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
