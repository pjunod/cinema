use crate::applied_mark::AppliedMark;
use crate::helpers::{deserialize, serialize};
use crate::migration::Migration;
use crate::query::rows::{ColumnOwned, RowOwned, ValueOwned};
use crate::store::logs;
use crate::store::state_machine::sqlite::state_machine;
use crate::store::state_machine::sqlite::state_machine::{
    Params, StateMachineData, StateMachineSqlite, StoredSnapshot,
};
use crate::store::state_machine::sqlite::transaction_env::{
    TransactionEnv, TransactionParamContext,
};
use crate::{AppliedMigration, Error, Node, NodeId, Param};
use chrono::Utc;
use flume::RecvError;
use openraft::{LogId, SnapshotMeta, StorageError, StorageIOError, StoredMembership};
use rusqlite::backup::Progress;
use rusqlite::fallible_iterator::FallibleIterator;
use rusqlite::fallible_streaming_iterator::FallibleStreamingIterator;
use rusqlite::types::Value;
use rusqlite::{Batch, CachedStatement, Rows, Transaction};
use std::borrow::Cow;
use std::default::Default;
use std::ops::Sub;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use thread_priority::ThreadPriority;
use tokio::sync::oneshot;
use tokio::{fs, runtime, task};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

#[derive(Debug)]
pub enum WriterRequest {
    Query(Query),
    Migrate(Migrate),
    Snapshot(SnapshotRequest),
    SnapshotApply((String, oneshot::Sender<Result<(), StorageError<NodeId>>>)),
    MetadataRead(oneshot::Sender<StateMachineData>),
    MetadataMembership(MetaMembershipRequest),
    MetadataApplied((Option<LogId<NodeId>>, oneshot::Sender<()>)),
    Backup(BackupRequest),
    Shutdown(oneshot::Sender<()>),
    #[cfg(feature = "validation-test-helpers")]
    RegisterPrecommitHold {
        hold: ValidationPrecommitHold,
        registered: oneshot::Sender<()>,
    },
    #[cfg(test)]
    HoldForStartupDrain {
        entered: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
    },
    #[allow(clippy::upper_case_acronyms)]
    RTT(RTTRequest),
}

/// Instance-local validation ownership; absent from production builds.
#[cfg(feature = "validation-test-helpers")]
#[derive(Debug)]
pub struct ValidationPrecommitHold {
    pub exact_statement: String,
    pub entered: oneshot::Sender<()>,
    pub release: std::sync::mpsc::Receiver<()>,
    pub until: std::time::Instant,
}

#[derive(Debug)]
pub enum Query {
    Execute(SqlExecute),
    ExecuteReturning(SqlExecuteReturning),
    Transaction(SqlTransaction),
    Batch(SqlBatch),
}

#[derive(Debug)]
pub struct SqlExecute {
    pub sql: Cow<'static, str>,
    pub params: Params,
    pub last_applied_log_id: Option<LogId<NodeId>>,
    pub tx: oneshot::Sender<Result<usize, Error>>,
}

#[derive(Debug)]
pub struct SqlExecuteReturning {
    pub sql: Cow<'static, str>,
    pub params: Params,
    pub last_applied_log_id: Option<LogId<NodeId>>,
    pub tx: oneshot::Sender<Result<Vec<Result<RowOwned, Error>>, Error>>,
}

#[derive(Debug)]
pub struct SqlTransaction {
    pub queries: Vec<state_machine::Query>,
    pub last_applied_log_id: Option<LogId<NodeId>>,
    pub tx: oneshot::Sender<Result<Vec<Result<usize, Error>>, Error>>,
}

#[derive(Debug)]
pub struct SqlBatch {
    pub sql: Cow<'static, str>,
    pub last_applied_log_id: Option<LogId<NodeId>>,
    pub tx: oneshot::Sender<Result<Vec<Result<usize, Error>>, Error>>,
}

#[derive(Debug)]
pub struct Migrate {
    pub migrations: Vec<Migration>,
    pub last_applied_log_id: Option<LogId<NodeId>>,
    pub tx: oneshot::Sender<Result<(), Error>>,
}

#[derive(Debug)]
pub struct SnapshotRequest {
    pub snapshot_id: Uuid,
    pub path: String,
    pub ack: oneshot::Sender<Result<SnapshotResponse, StorageError<NodeId>>>,
}

#[derive(Debug)]
pub struct SnapshotResponse {
    pub meta: StateMachineData,
}

struct SnapshotCopyCompletion {
    meta: StateMachineData,
    result: Result<(), std::io::Error>,
    ack: oneshot::Sender<Result<SnapshotResponse, StorageError<NodeId>>>,
}

enum WriterEvent {
    Request(Result<WriterRequest, RecvError>),
    Copy(Result<SnapshotCopyCompletion, RecvError>),
}

fn complete_snapshot_copy(
    completion: SnapshotCopyCompletion,
    sm_data: &mut StateMachineData,
    writer: &rusqlite::Connection,
) {
    // A cancelled builder cannot publish this generation. Its retained copy
    // has already released the read mark; keep the prior live snapshot id.
    if completion.ack.is_closed() {
        return;
    }
    let result = completion
        .result
        .and_then(|()| {
            // Applied entries and membership may have advanced during the copy.
            // Never replace them with the older cut's metadata.
            let mut live = sm_data.clone();
            live.last_snapshot_id = completion.meta.last_snapshot_id.clone();
            persist_metadata(writer, &live).map_err(std::io::Error::other)?;
            *sm_data = live;
            Ok(SnapshotResponse {
                meta: completion.meta,
            })
        })
        .map_err(|error| StorageError::IO {
            source: StorageIOError::write(&error),
        });
    let _ = completion.ack.send(result);
}

fn start_snapshot_copy(
    request: SnapshotRequest,
    writer: &rusqlite::Connection,
    sm_data: &StateMachineData,
    runtime: &runtime::Handle,
    completion_tx: &flume::Sender<SnapshotCopyCompletion>,
) -> bool {
    if request.ack.is_closed() {
        return false;
    }
    let mut meta = sm_data.clone();
    meta.last_snapshot_id = Some(request.snapshot_id.to_string());
    match pin_snapshot_cut(writer, &meta, sm_data) {
        Ok(reader) => {
            let completion_tx = completion_tx.clone();
            runtime.spawn_blocking(move || {
                // Release the WAL read mark before completion even if the
                // builder reply was cancelled while the copy was running.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    copy_snapshot(reader, &request.path)
                }))
                .unwrap_or_else(|_| Err(std::io::Error::other("snapshot copy panicked")));
                let _ = completion_tx.send(SnapshotCopyCompletion {
                    meta,
                    result,
                    ack: request.ack,
                });
            });
            true
        }
        Err(error) => {
            let _ = request.ack.send(Err(StorageError::IO {
                source: StorageIOError::write(&error),
            }));
            false
        }
    }
}

#[derive(Debug)]
pub struct MetaPersistRequest {
    pub data: StateMachineData,
    // pub data: Vec<u8>,
    pub ack: flume::Sender<()>, // TODO flume only needed for sync `drop()` -> convert to oneshot after being fixed
}

#[derive(Debug)]
pub struct MetaMembershipRequest {
    pub last_membership: StoredMembership<NodeId, Node>,
    pub last_applied_log_id: Option<LogId<NodeId>>,
    pub ack: oneshot::Sender<()>,
}

#[derive(Debug)]
pub struct BackupRequest {
    pub node_id: NodeId,
    pub target_folder: String,
    pub ts: i64,
    #[cfg(feature = "s3")]
    pub s3_config: Option<std::sync::Arc<crate::s3::S3Config>>,
    pub last_applied_log_id: Option<LogId<NodeId>>,
    pub ack: oneshot::Sender<Result<(), Error>>,
}

#[derive(Debug)]
pub struct RTTRequest {
    pub last_applied_log_id: Option<LogId<NodeId>>,
    pub ack: oneshot::Sender<()>,
}

#[allow(clippy::blocks_in_conditions)]
pub fn spawn_writer(
    mut conn: rusqlite::Connection,
    this_node: NodeId,
    path_lock_file: String,
    log_statements: bool,
    do_reset_metadata: bool,
    #[cfg(feature = "backup")] local_backup_keep_days: u16,
    mark: Arc<AppliedMark>,
) -> flume::Sender<WriterRequest> {
    let (tx, rx) = flume::bounded::<WriterRequest>(1);

    let rt = runtime::Handle::current();

    thread::spawn(move || {
        let _ = ThreadPriority::Max.set_for_current();

        let mut sm_data = StateMachineData::default();
        let mut ts_last_backup = None;
        let mut shutdown_ack: Option<oneshot::Sender<()>> = None;
        let (copy_tx, copy_rx) = flume::unbounded();
        let mut copy_in_flight = false;
        let mut pending_snapshot: Option<SnapshotRequest> = None;
        #[cfg(feature = "validation-test-helpers")]
        let mut precommit_hold: Option<ValidationPrecommitHold> = None;

        // TODO should we maybe save a backup task handle in case of shutdown overlap?

        if do_reset_metadata && let Err(err) = conn.execute("DROP TABLE IF EXISTS _metadata", ()) {
            error!(
                "Error cleaning up _metadata table - ignore this warning, if the DB was empty anyway"
            );
        }

        // we want to handle our metadata manually to not interfere with migrations in apps later on
        conn.execute(
            r#"
CREATE TABLE IF NOT EXISTS _metadata
(
    key  TEXT    NOT NULL
        CONSTRAINT _metadata_pk
            PRIMARY KEY,
    data BLOB    NOT NULL
)"#,
            (),
        )
        .expect("_metadata table creation to always succeed");

        // Plurx patch 27: count committed changes to the registered tables.
        // The hook runs on this thread inside each statement step; `publish`
        // turns its bits into revisions once the work has committed.
        let hook_mark = Arc::clone(&mark);
        conn.update_hook(Some(
            move |_action, database: &str, table: &str, _rowid: i64| {
                hook_mark.note_row_change(database, table);
            },
        ))
        .expect("the writer owns its connection, so installing the update hook succeeds");
        mark.prime_schema_version(schema_version(&conn));

        'main: loop {
            if !copy_in_flight && let Some(request) = pending_snapshot.take() {
                copy_in_flight = start_snapshot_copy(request, &conn, &sm_data, &rt, &copy_tx);
            }
            let event = flume::Selector::new()
                .recv(&rx, WriterEvent::Request)
                .recv(&copy_rx, WriterEvent::Copy)
                .wait();
            let req = match event {
                WriterEvent::Request(Ok(request)) => request,
                WriterEvent::Request(Err(_)) => break,
                WriterEvent::Copy(Ok(completion)) => {
                    copy_in_flight = false;
                    complete_snapshot_copy(completion, &mut sm_data, &conn);
                    continue;
                }
                WriterEvent::Copy(Err(_)) => break,
            };
            match req {
                WriterRequest::Query(query) => match query {
                    Query::Execute(q) => {
                        sm_data.last_applied_log_id = q.last_applied_log_id;

                        if log_statements {
                            info!("Query::Execute:\n{}\n{:?}", q.sql, q.params);
                        }

                        let res = {
                            let mut stmt = match conn.prepare_cached(q.sql.as_ref()) {
                                Ok(stmt) => stmt,
                                Err(err) => {
                                    error!("Preparing cached query {}: {:?}", q.sql, err);
                                    publish_then_send(
                                        &mark,
                                        Some(&conn),
                                        sm_data.last_applied_log_id,
                                        q.tx,
                                        Err(Error::PrepareStatement(err.to_string().into())),
                                    )
                                    .expect("oneshot tx to never be dropped");
                                    continue;
                                }
                            };

                            #[cfg(debug_assertions)]
                            check_stmt_params_count(&stmt, &q.params, &q.sql);

                            // let params_len = q.params.len();
                            let mut params_err = None;
                            let mut idx = 1;
                            #[allow(clippy::explicit_counter_loop)]
                            for param in q.params {
                                if let Err(err) = stmt.raw_bind_parameter(idx, param.into_sql()) {
                                    error!(
                                        "Error binding param on position {} to query {}: {:?}",
                                        idx, q.sql, err
                                    );
                                    params_err = Some(Error::QueryParams(err.to_string().into()));
                                    break;
                                }

                                idx += 1;
                            }

                            if let Some(err) = params_err {
                                publish_then_send(
                                    &mark,
                                    Some(&conn),
                                    sm_data.last_applied_log_id,
                                    q.tx,
                                    Err(err),
                                )
                                .expect("oneshot tx to never be dropped");
                                continue;
                            }

                            stmt.raw_execute().map_err(Error::from)
                        };

                        publish_then_send(
                            &mark,
                            Some(&conn),
                            sm_data.last_applied_log_id,
                            q.tx,
                            res,
                        )
                        .expect("oneshot tx to never be dropped");
                    }

                    Query::ExecuteReturning(q) => {
                        sm_data.last_applied_log_id = q.last_applied_log_id;

                        if log_statements {
                            info!("Query::ExecuteReturning:\n{}\n{:?}", q.sql, q.params);
                        }

                        let res = {
                            let mut stmt = match conn.prepare_cached(q.sql.as_ref()) {
                                Ok(stmt) => stmt,
                                Err(err) => {
                                    error!("Preparing cached query {}: {:?}", q.sql, err);
                                    publish_then_send(
                                        &mark,
                                        Some(&conn),
                                        sm_data.last_applied_log_id,
                                        q.tx,
                                        Err(Error::PrepareStatement(err.to_string().into())),
                                    )
                                    .expect("oneshot tx to never be dropped");
                                    continue;
                                }
                            };

                            #[cfg(debug_assertions)]
                            check_stmt_params_count(&stmt, &q.params, &q.sql);

                            let columns = match ColumnOwned::mapping_cols_from_stmt(stmt.columns())
                            {
                                Ok(c) => c,
                                Err(err) => {
                                    publish_then_send(
                                        &mark,
                                        Some(&conn),
                                        sm_data.last_applied_log_id,
                                        q.tx,
                                        Err(Error::PrepareStatement(err.to_string().into())),
                                    )
                                    .expect("oneshot tx to never be dropped");
                                    continue;
                                }
                            };

                            // let params_len = q.params.len();
                            let mut params_err = None;
                            let mut idx = 1;
                            #[allow(clippy::explicit_counter_loop)]
                            for param in q.params {
                                if let Err(err) = stmt.raw_bind_parameter(idx, param.into_sql()) {
                                    error!(
                                        "Error binding param on position {} to query {}: {:?}",
                                        idx, q.sql, err
                                    );
                                    params_err = Some(Error::QueryParams(err.to_string().into()));
                                    break;
                                }

                                idx += 1;
                            }

                            if let Some(err) = params_err {
                                publish_then_send(
                                    &mark,
                                    Some(&conn),
                                    sm_data.last_applied_log_id,
                                    q.tx,
                                    Err(err),
                                )
                                .expect("oneshot tx to never be dropped");
                                continue;
                            }

                            let mut rows = stmt.raw_query();
                            let mut res = Vec::new();
                            loop {
                                match rows.next() {
                                    Ok(Some(row)) => {
                                        res.push(Ok(RowOwned::from_row_column(row, &columns)));
                                    }
                                    Ok(None) => {
                                        break;
                                    }
                                    Err(err) => {
                                        res.push(Err(Error::Sqlite(err.to_string().into())));
                                    }
                                }
                            }

                            Ok(res)
                        };

                        publish_then_send(
                            &mark,
                            Some(&conn),
                            sm_data.last_applied_log_id,
                            q.tx,
                            res,
                        )
                        .expect("oneshot tx to never be dropped");
                    }

                    Query::Transaction(req) => {
                        #[cfg(feature = "validation-test-helpers")]
                        let matches_hold = precommit_hold.as_ref().is_some_and(|hold| {
                            req.queries
                                .iter()
                                .any(|query| query.sql.as_ref() == hold.exact_statement)
                        });
                        sm_data.last_applied_log_id = req.last_applied_log_id;

                        let txn = match conn.transaction() {
                            Ok(txn) => txn,
                            Err(err) => {
                                error!("Opening database transaction: {err:?}");
                                // No statement ran, so the schema cannot have moved;
                                // the failed open still borrows `conn` here.
                                publish_then_send(
                                    &mark,
                                    None,
                                    sm_data.last_applied_log_id,
                                    req.tx,
                                    Err(Error::Transaction(err.to_string().into())),
                                )
                                .expect("oneshot tx to never be dropped");
                                continue;
                            }
                        };

                        let mut results = Vec::with_capacity(req.queries.len());
                        let mut query_err = None;
                        let mut layout_guard_failed = false;
                        let mut txn_env = TransactionEnv::default();

                        'outer: for (stmt_index, state_machine::Query { sql, params }) in
                            req.queries.into_iter().enumerate()
                        {
                            if log_statements {
                                info!("Query::Transaction:\n{sql}\n{params:?}");
                            }

                            let exact_layout_guard =
                                is_exact_source_layout_guard(stmt_index, sql.as_ref(), &params);

                            let mut stmt = match txn.prepare_cached(sql.as_ref()) {
                                Ok(stmt) => stmt,
                                Err(err) => {
                                    let err = format!("Preparing cached query {sql}: {err:?}");
                                    query_err =
                                        Some(Error::PrepareStatement(err.to_string().into()));
                                    break;
                                }
                            };

                            #[cfg(debug_assertions)]
                            check_stmt_params_count(&stmt, &params, &sql);

                            let mut idx = 1;
                            #[allow(clippy::explicit_counter_loop)]
                            for param in params {
                                let ctx = TransactionParamContext {
                                    txn: &txn,
                                    env: &mut txn_env,
                                };
                                match param.into_sql_txn_ctx(ctx) {
                                    Ok(param) => {
                                        if let Err(err) = stmt.raw_bind_parameter(idx, param) {
                                            let err = format!(
                                                "Error binding param on position {idx} to query \
                                                {sql}: {err:?}"
                                            );
                                            query_err =
                                                Some(Error::QueryParams(err.to_string().into()));
                                            break 'outer;
                                        }
                                    }
                                    Err(err) => {
                                        query_err = Some(Error::QueryParams(err));
                                        break 'outer;
                                    }
                                }

                                idx += 1;
                            }

                            let column_count = stmt.column_count();

                            if column_count > 0 {
                                // the statement is potentially "observable", because it returns columns.
                                let mut rows = stmt.raw_query();
                                match rows.next().map_err(Error::from) {
                                    Ok(Some(row)) => {
                                        let mut row_count = 1;

                                        let mut first_row: Vec<Value> =
                                            Vec::with_capacity(column_count);
                                        for col_index in 0..column_count {
                                            first_row.push(row.get(col_index).unwrap());
                                        }

                                        'remaining_rows: loop {
                                            match rows.next().map_err(Error::from) {
                                                Ok(Some(_)) => {
                                                    row_count += 1;
                                                }
                                                Ok(None) => {
                                                    break 'remaining_rows;
                                                }
                                                Err(err) => {
                                                    query_err = Some(Error::Transaction(
                                                        err.to_string().into(),
                                                    ));
                                                    break 'outer;
                                                }
                                            }
                                        }

                                        results.push(Ok(row_count));
                                        // this statement is observable because it has output columns:
                                        txn_env.push_observable_stmt(stmt_index, sql, first_row);
                                    }
                                    Ok(None) => {
                                        results.push(Ok(0));
                                    }
                                    Err(err) => {
                                        query_err =
                                            Some(Error::Transaction(err.to_string().into()));
                                        break;
                                    }
                                }
                            } else {
                                let res = stmt.raw_execute();
                                match res {
                                    Ok(r) => {
                                        results.push(Ok(r));
                                    }
                                    Err(err) => {
                                        layout_guard_failed = exact_layout_guard
                                            && is_source_layout_check_failure(&err);
                                        let err = Error::from(err);
                                        query_err =
                                            Some(Error::Transaction(err.to_string().into()));
                                        break;
                                    }
                                }
                            };
                        }

                        if let Some(err) = query_err {
                            let rollback_succeeded = match txn.rollback() {
                                Ok(()) => true,
                                Err(e) => {
                                    error!("Error during txn rollback: {:?}", e);
                                    false
                                }
                            };
                            let err = source_layout_rollback_result(
                                err,
                                layout_guard_failed,
                                rollback_succeeded,
                            );
                            publish_then_send(
                                &mark,
                                Some(&conn),
                                sm_data.last_applied_log_id,
                                req.tx,
                                Err(err),
                            )
                            .expect("oneshot tx to never be dropped");
                        } else {
                            #[cfg(feature = "validation-test-helpers")]
                            if matches_hold && let Some(hold) = precommit_hold.take() {
                                let _ = hold.entered.send(());
                                let remaining = hold
                                    .until
                                    .saturating_duration_since(std::time::Instant::now());
                                // Retain the actual uncommitted Transaction and writer.
                                // Sender drop or the finite bound always releases it.
                                let _ = hold.release.recv_timeout(remaining);
                            }
                            match txn.commit() {
                                Ok(()) => {
                                    publish_then_send(
                                        &mark,
                                        Some(&conn),
                                        sm_data.last_applied_log_id,
                                        req.tx,
                                        Ok(results),
                                    )
                                    .expect("oneshot tx to never be dropped");
                                }
                                Err(err) => {
                                    publish_then_send(
                                        &mark,
                                        Some(&conn),
                                        sm_data.last_applied_log_id,
                                        req.tx,
                                        Err(Error::Transaction(err.to_string().into())),
                                    )
                                    .expect("oneshot tx to never be dropped");
                                }
                            }
                        }
                    }

                    Query::Batch(req) => {
                        sm_data.last_applied_log_id = req.last_applied_log_id;

                        if log_statements {
                            info!("Query::Batch:\n{}", req.sql);
                        }

                        let mut batch = Batch::new(&conn, req.sql.as_ref());
                        // we can at least assume 2 statements in a batch execute
                        let mut res = Vec::with_capacity(2);

                        let mut err = None;

                        loop {
                            match batch.next() {
                                Ok(Some(mut stmt)) => {
                                    res.push(stmt.execute([]).map_err(Error::from));
                                }
                                Ok(None) => break,
                                Err(e) => {
                                    err = Some(Error::Sqlite(e.to_string().into()));
                                    break;
                                }
                            }
                        }

                        // DDL and table rebuilds travel here; the hook reports
                        // them under a temporary name or not at all.
                        mark.note_every_table();
                        if let Some(err) = err {
                            publish_then_send(
                                &mark,
                                Some(&conn),
                                sm_data.last_applied_log_id,
                                req.tx,
                                Err(err),
                            )
                            .expect("oneshot tx to never be dropped");
                        } else {
                            publish_then_send(
                                &mark,
                                Some(&conn),
                                sm_data.last_applied_log_id,
                                req.tx,
                                Ok(res),
                            )
                            .expect("oneshot tx to never be dropped");
                        }
                    }
                },

                WriterRequest::Migrate(req) => {
                    sm_data.last_applied_log_id = req.last_applied_log_id;

                    // TODO should be maybe always panic if migrations throw an error?
                    let res = migrate(&mut conn, req.migrations);
                    mark.note_every_table();

                    if let Err(err) = conn.execute("PRAGMA optimize", []) {
                        error!("Error during 'PRAGMA optimize': {}", err);
                    }

                    publish_then_send(&mark, Some(&conn), sm_data.last_applied_log_id, req.tx, res)
                        .unwrap();
                }

                WriterRequest::Snapshot(request) => {
                    if copy_in_flight {
                        // A cancelled builder releases snapshot_files before
                        // its retained copy completes. Keep one successor
                        // without blocking ordinary applies behind that copy.
                        if pending_snapshot
                            .as_ref()
                            .is_some_and(|pending| !pending.ack.is_closed())
                        {
                            let error = std::io::Error::new(
                                std::io::ErrorKind::WouldBlock,
                                "snapshot successor already queued",
                            );
                            let _ = request.ack.send(Err(StorageError::IO {
                                source: StorageIOError::write(&error),
                            }));
                        } else {
                            pending_snapshot = Some(request);
                        }
                        continue;
                    }
                    copy_in_flight = start_snapshot_copy(request, &conn, &sm_data, &rt, &copy_tx);
                }

                WriterRequest::SnapshotApply((path, ack)) => {
                    if copy_in_flight {
                        if let Ok(completion) = copy_rx.recv() {
                            complete_snapshot_copy(completion, &mut sm_data, &conn);
                        }
                        copy_in_flight = false;
                    }
                    let start = Instant::now();
                    info!("Starting snapshot restore from {}", path);
                    let result = conn.restore(
                        "main",
                        path,
                        Some(|p: Progress| {
                            debug!(
                                remaining_pages = p.remaining,
                                page_count = p.pagecount,
                                "database snapshot restore progress"
                            );
                        }),
                    );

                    let result = match result {
                        Ok(()) => {
                            if let Err(err) = conn.execute("PRAGMA optimize", []) {
                                error!("Error during 'PRAGMA optimize': {}", err);
                            }

                            let metadata = match conn.query_row(
                                "SELECT data FROM _metadata WHERE key = 'meta'",
                                (),
                                |row| row.get::<_, Vec<u8>>(0),
                            ) {
                                Ok(meta_bytes) => {
                                    deserialize(&meta_bytes).map_err(|err| StorageError::IO {
                                        source: StorageIOError::read_state_machine(&err),
                                    })
                                }
                                Err(err) => Err(StorageError::IO {
                                    source: StorageIOError::read_state_machine(&err),
                                }),
                            };
                            match metadata {
                                Ok(metadata) => {
                                    sm_data = metadata;
                                    info!(
                                        "Snapshot restore finished after {} ms",
                                        start.elapsed().as_millis()
                                    );
                                    Ok(())
                                }
                                Err(error) => Err(error),
                            }
                        }
                        Err(err) => Err(StorageError::IO {
                            source: StorageIOError::write_state_machine(&err),
                        }),
                    };

                    // A restore replaced every table. The state machine's
                    // replacement guard keeps the generation odd until the
                    // read pool sees the new image; the bumps make sure no
                    // revision taken before it can match after it.
                    if result.is_ok() {
                        mark.note_every_table();
                    }
                    let _ = publish_then_send(
                        &mark,
                        Some(&conn),
                        sm_data.last_applied_log_id,
                        ack,
                        result,
                    );
                }

                WriterRequest::MetadataRead(ack) => {
                    if sm_data.last_applied_log_id.is_none() {
                        let mut stmt = conn
                            .prepare_cached("SELECT data FROM _metadata WHERE key = 'meta'")
                            .expect("Metadata read prepare to always succeed");

                        match stmt.query_row((), |row| {
                            let bytes: Vec<u8> = row.get(0)?;
                            Ok(bytes)
                        }) {
                            Ok(bytes) => {
                                sm_data = deserialize(&bytes).unwrap();
                                // Plurx patch 27: the persisted index is what
                                // the read pool sees after a restart.
                                mark.publish(
                                    sm_data.last_applied_log_id.map(|log_id| log_id.index),
                                );
                            }
                            Err(err) => {
                                warn!("No metadata exists inside the DB yet");
                            }
                        }
                    }

                    ack.send(sm_data.clone()).unwrap();
                }

                WriterRequest::MetadataMembership(req) => {
                    sm_data.last_membership = req.last_membership;
                    sm_data.last_applied_log_id = req.last_applied_log_id;
                    publish_then_send(&mark, Some(&conn), sm_data.last_applied_log_id, req.ack, ())
                        .unwrap();
                }

                WriterRequest::MetadataApplied((log_id, ack)) => {
                    sm_data.last_applied_log_id = log_id;
                    let _ =
                        publish_then_send(&mark, Some(&conn), sm_data.last_applied_log_id, ack, ());
                }

                WriterRequest::Backup(req) => {
                    sm_data.last_applied_log_id = req.last_applied_log_id;

                    // TODO include a TS in the req to skip backups if they are replayed after
                    // a restart
                    let now = Utc::now();
                    if let Some(ts) = ts_last_backup
                        && ts > now.sub(chrono::Duration::seconds(60))
                    {
                        info!(
                            "Received duplicate backup request within the last 60 seconds - ignoring it"
                        );
                        let _ = publish_then_send(
                            &mark,
                            Some(&conn),
                            sm_data.last_applied_log_id,
                            req.ack,
                            Ok(()),
                        );
                        continue;
                    }

                    info!("VACUUMing the database");
                    let start = Instant::now();
                    match conn.execute("VACUUM", ()) {
                        Ok(_) => {
                            info!("VACUUM finished after {} ms", start.elapsed().as_millis());
                        }
                        Err(err) => error!("Error during VACUUM: {}", err),
                    }

                    // only the current leader should push the backup
                    #[cfg(feature = "s3")]
                    let s3_config = if this_node == req.node_id {
                        req.s3_config
                    } else {
                        None
                    };

                    if let Err(err) = create_backup(
                        &conn,
                        req.node_id,
                        req.ts,
                        req.target_folder.clone(),
                        #[cfg(feature = "s3")]
                        s3_config,
                        #[cfg(feature = "s3")]
                        &rt,
                    ) {
                        error!("Error creating backup: {:?}", err);
                        let _ = publish_then_send(
                            &mark,
                            Some(&conn),
                            sm_data.last_applied_log_id,
                            req.ack,
                            Err(err),
                        );
                        continue;
                    }

                    #[cfg(feature = "backup")]
                    rt.spawn(async move {
                        if let Err(err) = crate::backup::backup_local_cleanup(
                            req.target_folder,
                            local_backup_keep_days,
                        )
                        .await
                        {
                            error!("Error during local backup cleanup: {:?}", err);
                        }
                    });

                    if let Err(err) = conn.execute("PRAGMA optimize", []) {
                        error!("Error during 'PRAGMA optimize': {}", err);
                    }

                    ts_last_backup = Some(now);
                    let _ = publish_then_send(
                        &mark,
                        Some(&conn),
                        sm_data.last_applied_log_id,
                        req.ack,
                        Ok(()),
                    );
                }

                WriterRequest::RTT(req) => {
                    sm_data.last_applied_log_id = req.last_applied_log_id;
                    publish_then_send(&mark, Some(&conn), sm_data.last_applied_log_id, req.ack, ())
                        .unwrap();
                }

                WriterRequest::Shutdown(ack) => {
                    shutdown_ack = Some(ack);
                    break;
                }
                #[cfg(test)]
                WriterRequest::HoldForStartupDrain { entered, release } => {
                    let _ = entered.send(());
                    let _ = release.blocking_recv();
                }
                #[cfg(feature = "validation-test-helpers")]
                WriterRequest::RegisterPrecommitHold { hold, registered } => {
                    precommit_hold = Some(hold);
                    let _ = registered.send(());
                }
            }
        }

        warn!("SQL writer is shutting down");
        if copy_in_flight && let Ok(completion) = copy_rx.recv() {
            complete_snapshot_copy(completion, &mut sm_data, &conn);
        }
        if let Some(request) = pending_snapshot
            && start_snapshot_copy(request, &conn, &sm_data, &rt, &copy_tx)
            && let Ok(completion) = copy_rx.recv()
        {
            complete_snapshot_copy(completion, &mut sm_data, &conn);
        }

        // make sure metadata is persisted before shutting down
        persist_metadata(&conn, &sm_data).expect("Error persisting metadata");

        if let Err(err) = conn.execute("PRAGMA optimize", []) {
            error!("Error during 'PRAGMA optimize': {}", err);
        }

        StateMachineSqlite::remove_lock_file(&path_lock_file);

        if let Some(ack) = shutdown_ack {
            ack.send(())
                .expect("Shutdown handler to always wait for ack from statemachine");
        }
    });

    tx
}

/// Publish what this entry applied, then reply. Every arm that applies a log
/// entry replies through here, early `continue` paths included, so a reply is
/// never sent before the mark shows the entry: after commit (or rollback) and
/// before the caller can observe the acknowledgement.
fn publish_then_send<T>(
    mark: &AppliedMark,
    conn: Option<&rusqlite::Connection>,
    applied: Option<LogId<NodeId>>,
    tx: oneshot::Sender<T>,
    value: T,
) -> Result<(), T> {
    if let Some(conn) = conn {
        mark.note_schema_version(schema_version(conn));
    }
    mark.publish(applied.map(|log_id| log_id.index));
    tx.send(value)
}

/// The schema cookie: SQLite increments it on every DDL statement, including
/// the `DROP` and `RENAME` of a table rebuild the update hook never reports.
fn schema_version(conn: &rusqlite::Connection) -> Option<i64> {
    conn.prepare_cached("PRAGMA schema_version")
        .and_then(|mut stmt| stmt.query_row([], |row| row.get(0)))
        .ok()
}

#[inline]
fn persist_metadata(
    conn: &rusqlite::Connection,
    metadata: &StateMachineData,
) -> Result<(), rusqlite::Error> {
    let meta_bytes = serialize(metadata).unwrap();
    let mut stmt = conn.prepare("REPLACE INTO _metadata (key, data) VALUES ('meta', $1)")?;
    stmt.execute([meta_bytes])?;
    Ok(())
}

#[cfg(debug_assertions)]
pub(crate) fn check_stmt_params_count(stmt: &CachedStatement, params: &[Param], sql: &str) {
    if stmt.parameter_count() != params.len() {
        error!(
            r#"

!!!!!!!!!!!!!!!!!!!!!!!
!!!!!!!!!!!!!!!!!!!!!!!
!!!!!!!!!!!!!!!!!!!!!!!

Parameter count of Statement:

{}

does not match the given parameter count!

Expected: {}
Got:      {}

!!!!!!!!!!!!!!!!!!!!!!!
!!!!!!!!!!!!!!!!!!!!!!!
!!!!!!!!!!!!!!!!!!!!!!!
"#,
            sql,
            stmt.parameter_count(),
            params.len()
        );
    }
}

#[inline]
fn pin_snapshot_cut(
    writer: &rusqlite::Connection,
    cut: &StateMachineData,
    live: &StateMachineData,
) -> Result<rusqlite::Connection, std::io::Error> {
    let result = (|| -> Result<_, rusqlite::Error> {
        let journal: String = writer.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        if !journal.eq_ignore_ascii_case("wal") {
            return Err(rusqlite::Error::InvalidQuery);
        }
        persist_metadata(writer, cut)?;
        let reader = rusqlite::Connection::open_with_flags(
            writer
                .path()
                .ok_or(rusqlite::Error::InvalidPath(std::path::PathBuf::new()))?,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        reader.execute_batch("BEGIN")?;
        // BEGIN alone is deferred. Reading this row establishes the exact WAL
        // read mark while the single writer still owns the cut.
        let _: Vec<u8> =
            reader.query_row("SELECT data FROM _metadata WHERE key = 'meta'", [], |row| {
                row.get(0)
            })?;
        Ok(reader)
    })();
    // The live database must not claim an unsuccessful copy's new id.
    let restore = persist_metadata(writer, live);
    restore.map_err(std::io::Error::other)?;
    result.map_err(std::io::Error::other)
}

fn copy_snapshot(reader: rusqlite::Connection, path: &str) -> Result<(), std::io::Error> {
    #[cfg(test)]
    let page_limit = snapshot_copy_test_hook(path);
    let mut destination = rusqlite::Connection::open(path).map_err(std::io::Error::other)?;
    #[cfg(test)]
    if let Some(limit) = page_limit {
        destination
            .pragma_update(None, "max_page_count", limit)
            .map_err(std::io::Error::other)?;
    }
    let backup =
        rusqlite::backup::Backup::new(&reader, &mut destination).map_err(std::io::Error::other)?;
    backup
        .run_to_completion(256, Duration::from_millis(1), None)
        .map_err(std::io::Error::other)
}

#[cfg(test)]
struct SnapshotCopyTestHook {
    pinned: flume::Sender<()>,
    release: flume::Receiver<()>,
    page_limit: Option<i64>,
}

#[cfg(test)]
static SNAPSHOT_COPY_TEST_HOOKS: std::sync::Mutex<
    std::collections::BTreeMap<String, SnapshotCopyTestHook>,
> = std::sync::Mutex::new(std::collections::BTreeMap::new());

#[cfg(test)]
fn snapshot_copy_test_hook(path: &str) -> Option<i64> {
    let hook = {
        let mut hooks = SNAPSHOT_COPY_TEST_HOOKS.lock().unwrap();
        let key = hooks
            .keys()
            .find(|prefix| path.starts_with(prefix.as_str()))
            .cloned();
        key.and_then(|key| hooks.remove(&key))
    };
    hook.and_then(|hook| {
        let _ = hook.pinned.send(());
        hook.release.recv().unwrap();
        hook.page_limit
    })
}

#[cfg(test)]
pub(crate) fn inject_snapshot_copy_full(prefix: String) {
    let (pinned, _) = flume::bounded(1);
    let (release, release_rx) = flume::bounded(1);
    release.send(()).unwrap();
    SNAPSHOT_COPY_TEST_HOOKS.lock().unwrap().insert(
        prefix,
        SnapshotCopyTestHook {
            pinned,
            release: release_rx,
            page_limit: Some(1),
        },
    );
}

fn create_backup(
    conn: &rusqlite::Connection,
    node_id: NodeId,
    ts: i64,
    target_folder: String,
    #[cfg(feature = "s3")] s3_config: Option<std::sync::Arc<crate::s3::S3Config>>,
    #[cfg(feature = "s3")] rt: &runtime::Handle,
) -> Result<(), Error> {
    // - build target db file name with node id and timestamp
    // - vacuum into target file
    // - connect to vacuumed db and reset metadata
    // - if we have an s3 target, encrypt and push it

    let file = format!("backup_node_{node_id}_{ts}.sqlite");
    let path_full = format!("{target_folder}/{file}");
    info!("Creating database backup into {path_full}");

    conn.execute(&format!("VACUUM main INTO '{path_full}'"), ())?;

    // connect to the backup and reset metadata
    // make sure connection is dropped before starting encrypt + push
    {
        let conn_bkp = rusqlite::Connection::open(&path_full)?;
        persist_metadata(&conn_bkp, &StateMachineData::default());
    }

    info!("Database backup finished");

    #[cfg(feature = "s3")]
    if let Some(s3) = s3_config {
        rt.spawn(async move {
            info!("Background task for database encryption and S3 backup task has been started");

            match s3.push(&path_full, &file).await {
                Ok(_) => {
                    info!("Push backup to S3 has been finished");
                }
                Err(err) => {
                    error!("Error pushing Backup to S3: {}", err);
                }
            }
        });
    }

    Ok(())
}

fn migrate(conn: &mut rusqlite::Connection, mut migrations: Vec<Migration>) -> Result<(), Error> {
    info!("Applying database migrations");

    create_migrations_table(conn)?;

    let mut last_applied = last_applied_migration(conn, &migrations)?;
    debug!("Last applied migration: {}", last_applied);
    migrations.retain(|m| m.id > last_applied);
    debug!(
        "Leftover migrations to apply: {:?}",
        migrations.iter().map(|m| format!("{}_{}", m.id, m.name))
    );

    for migration in migrations {
        if migration.id != last_applied + 1 {
            panic!(
                "Migration index has a gap between {} and {}",
                last_applied, migration.id
            );
        }
        last_applied = migration.id;

        let txn = conn.transaction()?;
        apply_migration(txn, migration)?;
    }

    Ok(())
}

#[inline]
fn create_migrations_table(conn: &rusqlite::Connection) -> Result<(), Error> {
    conn.execute(
        r#"
    CREATE TABLE IF NOT EXISTS _migrations
    (
        id   INTEGER    NOT NULL
            CONSTRAINT _migrations_pk
                PRIMARY KEY,
        name TEXT       NOT NULL,
        ts   INTEGER    NOT NULL,
        hash TEXT       NOT NULL
    )
    "#,
        [],
    )?;

    Ok(())
}

/// Validates the already applied migrations against the given ones and returns the
/// start index for new to apply migrations, if everything was ok.
#[inline]
fn last_applied_migration(
    conn: &rusqlite::Connection,
    migrations: &[Migration],
) -> Result<u32, Error> {
    if migrations.is_empty() {
        return Err(Error::Error("Received empty migrations".into()));
    }

    // We need the first id to skip all other existing migrations in the DB.
    // The client is optimized to reduce requests and strip out already existing ones.
    let first_id = migrations.first().as_ref().unwrap().id;

    if first_id > 1 {
        // double check, that we actually have the correct amount of migrations already applied.
        let mut stmt = conn.prepare("SELECT COUNT(*) AS count FROM _migrations WHERE id < $1")?;
        let count: u32 = stmt.query_row([first_id], |row| {
            let count: u32 = row.get("count")?;
            Ok(count)
        })?;
        if count < first_id - 1 {
            panic!(
                "Received optimized migrations starting at id '{first_id}' but found only \
                {count} already applied"
            );
        }
    }

    let mut stmt = conn.prepare("SELECT * FROM _migrations WHERE id >= $1 ORDER BY id ASC")?;
    let already_applied: Vec<AppliedMigration> = stmt
        .query_map([first_id], |row| {
            Ok(AppliedMigration {
                id: row.get(0)?,
                name: row.get(1)?,
                ts: row.get(2)?,
                hash: row.get(3)?,
            })
        })?
        .map(|r| r.expect("_migrations table corrupted"))
        .collect();

    // We can safely set the last_applied here because we checked it would have thrown an error
    // earlier otherwise already.
    let applied_offset = (first_id - 1) as usize;
    let mut last_applied = first_id - 1;
    for applied in already_applied {
        if last_applied + 1 != applied.id {
            panic!(
                "Applied migrations order mismatch: expected {}, got {}",
                last_applied + 1,
                applied.id
            );
        }
        last_applied = applied.id;

        match migrations.get(last_applied as usize - 1 - applied_offset) {
            None => panic!("Missing migration with id {last_applied}"),
            Some(migration) => {
                if applied.id != migration.id {
                    panic!(
                        "Migration id mismatch: applied {}, given {}\n{migrations:?}",
                        applied.id, migration.id
                    );
                }

                if applied.name != migration.name {
                    panic!(
                        "Name for migration {} has changed: applied {}, given {}\n{migrations:?}",
                        migration.id, applied.name, migration.name
                    );
                }

                if applied.hash != migration.hash {
                    panic!(
                        "Hash for migration {} has changed: applied {}, given {}\n{migrations:?}",
                        migration.id, applied.hash, migration.hash
                    );
                }
            }
        }
    }

    Ok(last_applied)
}

#[inline]
fn apply_migration(txn: rusqlite::Transaction, migration: Migration) -> Result<(), Error> {
    info!(
        "Applying database migration {} {}",
        migration.id, migration.name
    );

    let sql = String::from_utf8_lossy(&migration.content);
    let mut batch = Batch::new(&txn, &sql);

    while let Some(mut stmt) = batch.next()? {
        stmt.execute([])?;
    }

    {
        let mut stmt = txn.prepare(
            r#"
        INSERT INTO _migrations (id, name, ts, hash)
        VALUES ($1, $2, $3, $4)
        "#,
        )?;
        stmt.execute((
            migration.id,
            migration.name,
            Utc::now().timestamp(),
            migration.hash,
        ))?;
    }

    txn.commit()?;
    Ok(())
}

#[cfg(test)]
mod snapshot_cut_tests {
    use super::*;
    use openraft::{CommittedLeaderId, Membership};
    use std::collections::BTreeSet;

    fn log(index: u64) -> Option<LogId<NodeId>> {
        Some(LogId::new(CommittedLeaderId::new(1, 1), index))
    }

    async fn execute(writer: &flume::Sender<WriterRequest>, sql: String, index: u64) {
        let (tx, rx) = oneshot::channel();
        writer
            .send_async(WriterRequest::Query(Query::Execute(SqlExecute {
                sql: sql.into(),
                params: vec![],
                last_applied_log_id: log(index),
                tx,
            })))
            .await
            .unwrap();
        rx.await.unwrap().unwrap();
    }

    async fn metadata(writer: &flume::Sender<WriterRequest>) -> StateMachineData {
        let (tx, rx) = oneshot::channel();
        writer
            .send_async(WriterRequest::MetadataRead(tx))
            .await
            .unwrap();
        rx.await.unwrap()
    }

    fn image_metadata(connection: &rusqlite::Connection) -> StateMachineData {
        let bytes: Vec<u8> = connection
            .query_row("SELECT data FROM _metadata WHERE key='meta'", [], |row| {
                row.get(0)
            })
            .unwrap();
        deserialize(&bytes).unwrap()
    }

    async fn fixture() -> (std::path::PathBuf, flume::Sender<WriterRequest>) {
        let root = std::env::temp_dir().join(format!("hiqlite-cut-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let connection = rusqlite::Connection::open(root.join("live.db")).unwrap();
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA wal_autocheckpoint=0;",
            )
            .unwrap();
        let writer = spawn_writer(
            connection,
            1,
            root.join("lock").to_string_lossy().into_owned(),
            false,
            false,
            #[cfg(feature = "backup")]
            30,
            AppliedMark::new::<&str>(&[]),
        );
        execute(
            &writer,
            "CREATE TABLE rows_at_cut (id INTEGER PRIMARY KEY, payload BLOB)".into(),
            1,
        )
        .await;
        execute(
            &writer,
            "INSERT INTO rows_at_cut VALUES (1, zeroblob(8192))".into(),
            2,
        )
        .await;
        (root, writer)
    }

    async fn shutdown(root: std::path::PathBuf, writer: flume::Sender<WriterRequest>) {
        let (ack, rx) = oneshot::channel();
        writer
            .send_async(WriterRequest::Shutdown(ack))
            .await
            .unwrap();
        rx.await.unwrap();
        drop(writer);
        std::fs::remove_dir_all(root).unwrap();
    }

    async fn paused_copy(
        writer: &flume::Sender<WriterRequest>,
        path: &str,
        page_limit: Option<i64>,
    ) -> (
        flume::Sender<()>,
        oneshot::Receiver<Result<SnapshotResponse, StorageError<NodeId>>>,
    ) {
        let (pinned, pinned_rx) = flume::bounded(1);
        let (release, release_rx) = flume::bounded(1);
        SNAPSHOT_COPY_TEST_HOOKS.lock().unwrap().insert(
            path.to_owned(),
            SnapshotCopyTestHook {
                pinned,
                release: release_rx,
                page_limit,
            },
        );
        let (ack, rx) = oneshot::channel();
        writer
            .send_async(WriterRequest::Snapshot(SnapshotRequest {
                snapshot_id: Uuid::now_v7(),
                path: path.into(),
                ack,
            }))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), pinned_rx.recv_async())
            .await
            .unwrap()
            .unwrap();
        (release, rx)
    }

    #[tokio::test]
    async fn fixed_readmark_copy_allows_1000_applies_and_membership_without_cut_drift() {
        let (root, writer) = fixture().await;
        let first_membership =
            StoredMembership::new(log(3), Membership::new(vec![BTreeSet::from([1])], None));
        let (ack, rx) = oneshot::channel();
        writer
            .send_async(WriterRequest::MetadataMembership(MetaMembershipRequest {
                last_membership: first_membership.clone(),
                last_applied_log_id: log(3),
                ack,
            }))
            .await
            .unwrap();
        rx.await.unwrap();
        let path = root.join("copy.db").to_string_lossy().into_owned();
        let (release, copy) = paused_copy(&writer, &path, None).await;
        tokio::time::timeout(Duration::from_secs(15), async {
            for index in 4..1004 {
                execute(
                    &writer,
                    format!("INSERT INTO rows_at_cut VALUES ({index}, zeroblob(8192))"),
                    index,
                )
                .await;
            }
            let (ack, rx) = oneshot::channel();
            writer
                .send_async(WriterRequest::MetadataMembership(MetaMembershipRequest {
                    last_membership: StoredMembership::new(
                        log(1004),
                        Membership::new(vec![BTreeSet::from([1, 2])], None),
                    ),
                    last_applied_log_id: log(1004),
                    ack,
                }))
                .await
                .unwrap();
            rx.await.unwrap();
        })
        .await
        .expect("applies must finish while copy remains paused");
        let checkpoint = rusqlite::Connection::open(root.join("live.db")).unwrap();
        checkpoint.busy_timeout(Duration::ZERO).unwrap();
        let busy: i64 = checkpoint
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy, 1, "read mark must prevent WAL reset during copy");
        release.send(()).unwrap();
        let response = tokio::time::timeout(Duration::from_secs(10), copy)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(response.meta.last_applied_log_id, log(3));
        assert_eq!(response.meta.last_membership, first_membership);
        let image = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(
            image
                .query_row("SELECT count(*) FROM rows_at_cut", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        let meta = image_metadata(&image);
        assert_eq!(meta.last_applied_log_id, response.meta.last_applied_log_id);
        assert_eq!(meta.last_membership, response.meta.last_membership);
        assert_eq!(meta.last_snapshot_id, response.meta.last_snapshot_id);
        let live = metadata(&writer).await;
        assert_eq!(live.last_applied_log_id, log(1004));
        assert_ne!(live.last_membership, first_membership);
        assert_eq!(live.last_snapshot_id, response.meta.last_snapshot_id);
        let busy: i64 = checkpoint
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy, 0, "completion must release read mark");
        assert_eq!(
            std::fs::metadata(root.join("live.db-wal")).unwrap().len(),
            0
        );
        drop(image);
        drop(checkpoint);
        shutdown(root, writer).await;
    }

    #[tokio::test]
    async fn full_destination_copy_keeps_live_metadata_and_database_intact() {
        let (root, writer) = fixture().await;
        let before = metadata(&writer).await;
        let path = root.join("full.db").to_string_lossy().into_owned();
        let (release, copy) = paused_copy(&writer, &path, Some(1)).await;
        execute(
            &writer,
            "INSERT INTO rows_at_cut VALUES (3, zeroblob(8192))".into(),
            3,
        )
        .await;
        release.send(()).unwrap();
        assert!(
            copy.await.unwrap().is_err(),
            "SQLite max_page_count must cause a real SQLITE_FULL error"
        );
        let live = metadata(&writer).await;
        assert_eq!(live.last_snapshot_id, before.last_snapshot_id);
        assert_eq!(live.last_applied_log_id, log(3));
        let connection = rusqlite::Connection::open(root.join("live.db")).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM rows_at_cut", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
        let busy: i64 = connection
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy, 0);
        drop(connection);
        shutdown(root, writer).await;
    }

    #[tokio::test]
    async fn cancelled_copy_reply_retains_reader_and_queues_successor_without_blocking_apply() {
        let (root, writer) = fixture().await;
        let first_path = root.join("first.db").to_string_lossy().into_owned();
        let (release, first) = paused_copy(&writer, &first_path, None).await;
        drop(first);
        let second_path = root.join("second.db").to_string_lossy().into_owned();
        let (ack, second) = oneshot::channel();
        writer
            .send_async(WriterRequest::Snapshot(SnapshotRequest {
                snapshot_id: Uuid::now_v7(),
                path: second_path.clone(),
                ack,
            }))
            .await
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(5),
            execute(
                &writer,
                "INSERT INTO rows_at_cut VALUES (3, zeroblob(8192))".into(),
                3,
            ),
        )
        .await
        .expect("queued successor must not block applies");
        release.send(()).unwrap();
        let response = tokio::time::timeout(Duration::from_secs(10), second)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(response.meta.last_applied_log_id, log(3));
        let image = rusqlite::Connection::open(second_path).unwrap();
        assert_eq!(
            image
                .query_row("SELECT count(*) FROM rows_at_cut", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(image_metadata(&image).last_applied_log_id, log(3));
        let connection = rusqlite::Connection::open(root.join("live.db")).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(image);
        drop(connection);
        shutdown(root, writer).await;
    }

    #[tokio::test]
    #[ignore = "requires PLURX_K02_ENOSPC_DIR on an agent-owned bounded filesystem"]
    async fn real_enospc_on_an_owned_bounded_filesystem_preserves_live_state() {
        use std::io::Write;
        let bounded = std::path::PathBuf::from(
            std::env::var_os("PLURX_K02_ENOSPC_DIR")
                .expect("explicit owned bounded filesystem is mandatory"),
        );
        let available = fs4::available_space(&bounded).unwrap();
        assert!(
            fs4::total_space(&bounded).unwrap() <= 32 * 1024 * 1024,
            "refuse to fill a host filesystem even if its remaining space is low"
        );
        assert!(
            available > 0 && available < 32 * 1024 * 1024,
            "refuse a broad or already full filesystem"
        );
        let (root, writer) = fixture().await;
        execute(
            &writer,
            "INSERT INTO rows_at_cut VALUES (3, zeroblob(67108864))".into(),
            3,
        )
        .await;
        let before = metadata(&writer).await;
        let path = bounded.join(format!("snapshot-{}.db", Uuid::now_v7()));
        let (ack, rx) = oneshot::channel();
        writer
            .send_async(WriterRequest::Snapshot(SnapshotRequest {
                snapshot_id: Uuid::now_v7(),
                path: path.to_string_lossy().into_owned(),
                ack,
            }))
            .await
            .unwrap();
        let error = tokio::time::timeout(Duration::from_secs(30), rx)
            .await
            .unwrap()
            .unwrap()
            .expect_err("64MiB source cannot fit bounded destination");
        assert!(
            format!("{error}").contains("full"),
            "must fail for storage capacity, not another cause: {error}"
        );
        // Confirm the operating system's actual ENOSPC on this isolated volume,
        // not merely a simulated page limit. No host filesystem is filled.
        let confirmation = bounded.join(format!("enospc-{}.bin", Uuid::now_v7()));
        let os_error = match std::fs::File::create(&confirmation) {
            Ok(mut file) => file.write_all(&vec![0; 32 * 1024 * 1024]).unwrap_err(),
            Err(error) => error,
        };
        assert_eq!(os_error.raw_os_error(), Some(28));
        let live = metadata(&writer).await;
        assert_eq!(live.last_snapshot_id, before.last_snapshot_id);
        assert_eq!(live.last_applied_log_id, before.last_applied_log_id);
        let connection = rusqlite::Connection::open(root.join("live.db")).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        assert_eq!(
            connection
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        println!(
            "bounded ENOSPC receipt: available_before={available}, source_blob_bytes=67108864, snapshot_error={error}, os_errno=28, integrity=ok, readmark_released=true"
        );
        drop(connection);
        if confirmation.exists() {
            std::fs::remove_file(confirmation).unwrap();
        }
        if path.exists() {
            std::fs::remove_file(path).unwrap();
        }
        shutdown(root, writer).await;
    }
}

fn is_source_layout_check_failure(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::SqliteFailure(code, Some(message))
        if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_CHECK
        && message == "CHECK constraint failed: plurx_source_layout_guard_v1")
}

fn source_layout_rollback_result(
    original: Error,
    exact_guard_failed: bool,
    rollback_succeeded: bool,
) -> Error {
    if exact_guard_failed && rollback_succeeded {
        Error::Transaction(Error::SOURCE_LAYOUT_ROLLBACK_CERTIFICATE.into())
    } else {
        original
    }
}

fn is_exact_source_layout_guard(index: usize, sql: &str, params: &[Param]) -> bool {
    index == 0
        && sql == Error::SOURCE_LAYOUT_GUARD_SQL
        && matches!(params, [Param::Integer(81 | 82)])
}

#[cfg(test)]
mod source_layout_rollback_tests {
    use super::*;

    #[test]
    fn source_layout_certificate_requires_exact_first_guard_and_completed_rollback() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE cluster_meta(singleton INTEGER PRIMARY KEY,schema_version INTEGER); INSERT INTO cluster_meta VALUES(1,82); CREATE TABLE application(value INTEGER);").unwrap();
        conn.execute_batch(Error::SOURCE_LAYOUT_GUARD_DDL).unwrap();
        // A matching layout inserts zero guard rows and can commit application work.
        let txn = conn.transaction().unwrap();
        assert_eq!(
            txn.execute(Error::SOURCE_LAYOUT_GUARD_SQL, [82]).unwrap(),
            0
        );
        txn.execute("INSERT INTO application VALUES(1)", [])
            .unwrap();
        txn.commit().unwrap();
        let txn = conn.transaction().unwrap();
        let failure = txn
            .execute(Error::SOURCE_LAYOUT_GUARD_SQL, [81])
            .unwrap_err();
        assert!(is_source_layout_check_failure(&failure));
        let exact =
            is_exact_source_layout_guard(0, Error::SOURCE_LAYOUT_GUARD_SQL, &[Param::Integer(81)]);
        let original = Error::Transaction(Error::from(failure).to_string().into());
        let result = source_layout_rollback_result(original, exact, txn.rollback().is_ok());
        assert!(result.is_source_layout_guard_rollback());
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM application", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM sharing_source_dispatch_guard",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        for (index, sql, params) in [
            (1, Error::SOURCE_LAYOUT_GUARD_SQL, vec![Param::Integer(81)]),
            (0, "SELECT 1", vec![Param::Integer(81)]),
            (0, Error::SOURCE_LAYOUT_GUARD_SQL, vec![Param::Integer(80)]),
            (
                0,
                Error::SOURCE_LAYOUT_GUARD_SQL,
                vec![Param::Text("81".into())],
            ),
            (
                0,
                Error::SOURCE_LAYOUT_GUARD_SQL,
                vec![Param::Integer(81), Param::Integer(82)],
            ),
        ] {
            assert!(!is_exact_source_layout_guard(index, sql, &params));
        }
        assert!(
            !source_layout_rollback_result(Error::Transaction("original".into()), true, false)
                .is_source_layout_guard_rollback()
        );
        assert!(
            !source_layout_rollback_result(Error::Transaction("original".into()), false, true)
                .is_source_layout_guard_rollback()
        );
        conn.execute_batch(
            "CREATE TABLE other(value INTEGER CONSTRAINT other_check CHECK(value=1));",
        )
        .unwrap();
        let other = conn.execute("INSERT INTO other VALUES(0)", []).unwrap_err();
        assert!(!is_source_layout_check_failure(&other));
    }
}

#[cfg(test)]
mod applied_mark_tests {
    use super::*;
    use crate::AppliedMarkHandle;
    use openraft::CommittedLeaderId;

    fn log(index: u64) -> Option<LogId<NodeId>> {
        Some(LogId::new(CommittedLeaderId::new(1, 1), index))
    }

    struct Fixture {
        root: std::path::PathBuf,
        writer: flume::Sender<WriterRequest>,
        mark: AppliedMarkHandle,
        /// Revisions once the fixture's own `CREATE TABLE`s have applied
        /// (each moved every slot: a schema change).
        base: (u64, u64),
    }

    impl Fixture {
        async fn start() -> Self {
            let root = std::env::temp_dir().join(format!("hiqlite-mark-{}", Uuid::now_v7()));
            std::fs::create_dir_all(&root).unwrap();
            let mut fixture = Self::open(root);
            for (index, sql) in [
                "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
                "CREATE TABLE media_sessions (id TEXT PRIMARY KEY, state TEXT NOT NULL)",
                "CREATE TABLE items (id INTEGER PRIMARY KEY, title TEXT NOT NULL)",
            ]
            .into_iter()
            .enumerate()
            {
                fixture
                    .execute(sql, u64::try_from(index).unwrap() + 1)
                    .await
                    .unwrap();
            }
            fixture.base = fixture.revisions();
            fixture
        }

        /// A writer with a fresh mark over the database under `root`.
        fn open(root: std::path::PathBuf) -> Self {
            let connection = rusqlite::Connection::open(root.join("live.db")).unwrap();
            connection
                .execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
                .unwrap();
            let mark = AppliedMark::new(&["settings", "media_sessions"]);
            let handle = mark.handle();
            let writer = spawn_writer(
                connection,
                1,
                root.join("lock").to_string_lossy().into_owned(),
                false,
                false,
                #[cfg(feature = "backup")]
                30,
                mark,
            );
            Self {
                root,
                writer,
                mark: handle,
                base: (0, 0),
            }
        }

        /// Stop the writer (it persists its metadata) and start a new one,
        /// with a new mark, over the same file: a process restart.
        async fn restart(self) -> Self {
            let root = self.stop().await;
            Self::open(root)
        }

        async fn execute(&self, sql: &'static str, index: u64) -> Result<usize, Error> {
            let (tx, rx) = oneshot::channel();
            self.writer
                .send_async(WriterRequest::Query(Query::Execute(SqlExecute {
                    sql: sql.into(),
                    params: vec![],
                    last_applied_log_id: log(index),
                    tx,
                })))
                .await
                .unwrap();
            rx.await.unwrap()
        }

        /// Slot movements since the fixture's schema was created.
        fn revisions(&self) -> (u64, u64) {
            (
                self.mark.revision(self.mark.slot("settings").unwrap()) - self.base.0,
                self.mark
                    .revision(self.mark.slot("media_sessions").unwrap())
                    - self.base.1,
            )
        }

        fn reader(&self) -> rusqlite::Connection {
            rusqlite::Connection::open(self.root.join("live.db")).unwrap()
        }

        async fn stop(self) -> std::path::PathBuf {
            let (ack, rx) = oneshot::channel();
            self.writer
                .send_async(WriterRequest::Shutdown(ack))
                .await
                .unwrap();
            rx.await.unwrap();
            drop(self.writer);
            self.root
        }

        async fn shutdown(self) {
            std::fs::remove_dir_all(self.stop().await).unwrap();
        }

        async fn transaction(&self, queries: &[&'static str], index: u64) -> Result<(), Error> {
            let (tx, rx) = oneshot::channel();
            self.writer
                .send_async(WriterRequest::Query(Query::Transaction(SqlTransaction {
                    queries: queries
                        .iter()
                        .map(|sql| state_machine::Query {
                            sql: (*sql).into(),
                            params: vec![],
                        })
                        .collect(),
                    last_applied_log_id: log(index),
                    tx,
                })))
                .await
                .unwrap();
            rx.await.unwrap().map(|_| ())
        }
    }

    /// Plurx's schema steps on `media_sessions` travel as transactions
    /// (`schema_migration_transaction`), and the Source-layout rebuild is
    /// create-copy-drop-rename: the hook sees only the copy, under the new
    /// table's temporary name. The schema cookie moves every slot.
    #[tokio::test]
    async fn a_transaction_that_rebuilds_a_registered_table_moves_every_slot() {
        let fixture = Fixture::start().await;
        fixture
            .execute("INSERT INTO media_sessions VALUES ('s1', 'active')", 4)
            .await
            .unwrap();
        assert_eq!(fixture.revisions(), (0, 1));
        fixture
            .transaction(
                &[
                    "CREATE TABLE media_sessions_new (id TEXT PRIMARY KEY, state TEXT NOT NULL, epoch INTEGER NOT NULL DEFAULT 0)",
                    "INSERT INTO media_sessions_new (id, state) SELECT id, state FROM media_sessions",
                    "DROP TABLE media_sessions",
                    "ALTER TABLE media_sessions_new RENAME TO media_sessions",
                ],
                5,
            )
            .await
            .unwrap();
        assert_eq!(fixture.revisions(), (1, 2), "the rebuild moved every slot");
        assert_eq!(fixture.mark.applied_index(), 5);
        fixture.shutdown().await;
    }

    /// The single-statement form: `ALTER TABLE ... ADD COLUMN` reports no row.
    #[tokio::test]
    async fn an_executed_alter_table_moves_every_slot() {
        let fixture = Fixture::start().await;
        fixture
            .execute(
                "ALTER TABLE media_sessions ADD COLUMN epoch INTEGER NOT NULL DEFAULT 0",
                4,
            )
            .await
            .unwrap();
        assert_eq!(fixture.revisions(), (1, 1));
        // An ordinary row change afterwards moves only its own slot again.
        fixture
            .execute("INSERT INTO settings VALUES ('a', '1')", 5)
            .await
            .unwrap();
        assert_eq!(fixture.revisions(), (2, 1));
        fixture.shutdown().await;
    }

    /// After a restart the mark starts at 0; the state machine's metadata read
    /// publishes the persisted index before any entry applies, so a caught-up
    /// node does not report 0 (and fail every floor) until the next write.
    #[tokio::test]
    async fn a_restart_publishes_the_persisted_applied_index() {
        let fixture = Fixture::start().await;
        fixture
            .execute("INSERT INTO settings VALUES ('a', '1')", 7)
            .await
            .unwrap();
        let fixture = fixture.restart().await;
        assert_eq!(fixture.mark.applied_index(), 0, "a fresh mark");
        let (ack, rx) = oneshot::channel();
        fixture
            .writer
            .send_async(WriterRequest::MetadataRead(ack))
            .await
            .unwrap();
        let metadata = rx.await.unwrap();
        assert_eq!(metadata.last_applied_log_id.map(|id| id.index), Some(7));
        assert_eq!(fixture.mark.applied_index(), 7);
        fixture.shutdown().await;
    }

    #[tokio::test]
    async fn a_registered_write_moves_its_own_slot_and_an_unregistered_one_moves_none() {
        let fixture = Fixture::start().await;
        assert_eq!(fixture.revisions(), (0, 0));
        assert_eq!(fixture.mark.applied_index(), 3);

        fixture
            .execute("INSERT INTO items (title) VALUES ('x')", 4)
            .await
            .unwrap();
        assert_eq!(fixture.revisions(), (0, 0));
        assert_eq!(fixture.mark.applied_index(), 4);

        fixture
            .execute("INSERT INTO settings VALUES ('a', '1')", 5)
            .await
            .unwrap();
        assert_eq!(fixture.revisions(), (1, 0));
        fixture
            .execute(
                "INSERT INTO settings VALUES ('a', '2') ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                6,
            )
            .await
            .unwrap();
        assert_eq!(fixture.revisions(), (2, 0), "an upsert is a row change");
        fixture
            .execute("DELETE FROM media_sessions WHERE id = 'none'", 7)
            .await
            .unwrap();
        assert_eq!(
            fixture.revisions(),
            (2, 0),
            "a statement that changes no row moves nothing"
        );

        // A failed entry is still applied: its index is published, no slot moves.
        assert!(
            fixture
                .execute("INSERT INTO missing VALUES (1)", 8)
                .await
                .is_err()
        );
        assert_eq!(fixture.revisions(), (2, 0));
        assert_eq!(fixture.mark.applied_index(), 8);
        fixture.shutdown().await;
    }

    #[tokio::test]
    async fn batch_and_migrate_move_every_slot() {
        let fixture = Fixture::start().await;
        let (tx, rx) = oneshot::channel();
        fixture
            .writer
            .send_async(WriterRequest::Query(Query::Batch(SqlBatch {
                sql: "CREATE TABLE settings_rebuilt (key TEXT PRIMARY KEY); DROP TABLE settings_rebuilt;"
                    .into(),
                last_applied_log_id: log(4),
                tx,
            })))
            .await
            .unwrap();
        rx.await.unwrap().unwrap();
        assert_eq!(fixture.revisions(), (1, 1));
        assert_eq!(fixture.mark.applied_index(), 4);

        let (tx, rx) = oneshot::channel();
        fixture
            .writer
            .send_async(WriterRequest::Migrate(Migrate {
                migrations: vec![],
                last_applied_log_id: log(5),
                tx,
            }))
            .await
            .unwrap();
        // An empty migration set is refused, but the entry is still applied:
        // the arm cannot know what a failed migration left behind.
        assert!(rx.await.unwrap().is_err());
        assert_eq!(fixture.revisions(), (2, 2));
        assert_eq!(fixture.mark.applied_index(), 5);
        fixture.shutdown().await;
    }

    /// The precommit hold keeps the real uncommitted transaction on the writer:
    /// while it is held the row is invisible and neither the slot nor the
    /// applied index has moved; once the reply arrives both have. That the
    /// publish comes *before* the reply is pinned by the source test
    /// `every_applying_reply_publishes_first`: swapped, this test would
    /// usually still pass, because the publish follows the send at once.
    #[cfg(feature = "validation-test-helpers")]
    #[tokio::test]
    async fn the_slot_moves_only_once_the_entry_has_committed() {
        let fixture = Fixture::start().await;
        let statement = "INSERT INTO media_sessions VALUES ('s1', 'active')";
        let (entered, entered_rx) = oneshot::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let (registered, registered_rx) = oneshot::channel();
        fixture
            .writer
            .send_async(WriterRequest::RegisterPrecommitHold {
                hold: ValidationPrecommitHold {
                    exact_statement: statement.to_owned(),
                    entered,
                    release: release_rx,
                    until: Instant::now() + Duration::from_secs(10),
                },
                registered,
            })
            .await
            .unwrap();
        registered_rx.await.unwrap();

        let (tx, mut reply) = oneshot::channel();
        fixture
            .writer
            .send_async(WriterRequest::Query(Query::Transaction(SqlTransaction {
                queries: vec![state_machine::Query {
                    sql: statement.into(),
                    params: vec![],
                }],
                last_applied_log_id: log(9),
                tx,
            })))
            .await
            .unwrap();
        entered_rx.await.unwrap();

        let visible = |reader: &rusqlite::Connection| -> i64 {
            reader
                .query_row("SELECT count(*) FROM media_sessions", [], |row| row.get(0))
                .unwrap()
        };
        let reader = fixture.reader();
        assert_eq!(visible(&reader), 0, "held: the row is not committed");
        assert_eq!(fixture.revisions(), (0, 0), "held: the slot has not moved");
        assert_eq!(
            fixture.mark.applied_index(),
            3,
            "held: the index has not moved"
        );
        assert!(reply.try_recv().is_err(), "held: no reply yet");

        release.send(()).unwrap();
        reply.await.unwrap().unwrap();
        assert_eq!(fixture.revisions(), (0, 1), "the reply follows the publish");
        assert_eq!(fixture.mark.applied_index(), 9);
        assert_eq!(visible(&reader), 1);
        fixture.shutdown().await;
    }

    /// Every reply from an arm that applies a log entry goes through
    /// `publish_then_send`; only the snapshot-successor refusal and the
    /// metadata read, neither of which applies an entry, reply directly.
    #[test]
    fn every_applying_reply_publishes_first() {
        let source = include_str!("writer.rs");
        let body = source
            .split_once("'main: loop {")
            .expect("writer loop")
            .1
            .split_once("WriterRequest::Shutdown(ack) => {")
            .expect("end of writer arms")
            .0;
        let allowed = [
            "request.ack.send(Err(StorageError::IO {",
            "ack.send(sm_data.clone()).unwrap();",
            "let _ = entered.send(());",
            "let _ = registered.send(());",
            "let _ = hold.entered.send(());",
        ];
        let mut direct = Vec::new();
        for (offset, _) in body.match_indices(".send(") {
            let line_start = body[..offset].rfind('\n').map_or(0, |at| at + 1);
            let line_end = body[offset..]
                .find('\n')
                .map_or(body.len(), |at| offset + at);
            let line = body[line_start..line_end].trim();
            if !allowed.iter().any(|ok| line.contains(ok)) {
                direct.push(line.to_owned());
            }
        }
        assert_eq!(
            direct,
            Vec::<String>::new(),
            "replies that skip the publish"
        );
        let published = body.matches("publish_then_send(").count();
        assert!(
            published >= 19,
            "every applying arm replies through the publish: {published}"
        );
        let without_schema = body
            .split("publish_then_send(")
            .skip(1)
            .filter(|call| {
                call.trim_start()
                    .trim_start_matches("&mark,")
                    .trim_start()
                    .starts_with("None,")
            })
            .count();
        assert_eq!(
            without_schema, 1,
            "only the failed transaction open, which ran no SQL, skips the schema check"
        );
        assert!(
            source.contains(
                "        mark.note_schema_version(schema_version(conn));\n    }\n    mark.publish(applied.map(|log_id| log_id.index));\n    tx.send(value)"
            ),
            "the helper publishes before it sends"
        );
        let batch = body
            .split_once("Query::Batch(req) => {")
            .unwrap()
            .1
            .split_once("WriterRequest::Migrate(req) => {")
            .unwrap();
        assert!(batch.0.contains("mark.note_every_table();"));
        assert!(
            batch
                .1
                .split_once("WriterRequest::Snapshot(request)")
                .unwrap()
                .0
                .contains("mark.note_every_table();")
        );
    }
}
