use crate::client::stream::{ClientExecutePayload, ClientStreamReq};
use crate::network::api::ApiStreamResponsePayload;
use crate::query::rows::RowOwned;
use crate::store::state_machine::sqlite::state_machine::{Query, QueryWrite};
use crate::{Client, Error, Params, Response};
use std::borrow::Cow;
use tokio::sync::oneshot;

/// A committed write's result together with the Raft log index of the entry
/// that carried it.
///
/// Plurx patch. `log_index` is `Some` when this node was the leader that
/// committed the entry, or when the leader serving this client's stream
/// negotiated the `x-hiqlite-write-ack` stream header. It is `None`
/// when the write was answered by a leader (or proxy) that predates the
/// negotiation: the write still committed, but its position in the log is not
/// known here, and a caller that fences reads on it must treat that as "not
/// provable" rather than as zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteAck<T> {
    pub result: T,
    pub log_index: Option<u64>,
}

impl Client {
    /// Execute any modifying / non-read-only query on the database.
    /// Returns the affected rows on success.
    ///
    /// ```rust, notest
    /// client
    ///     .execute(
    ///         "INSERT INTO test (id, num, description) VALUES ($1, $2, $3)",
    ///         params!("id1", 123, "my description"),
    ///     )
    ///     .await?;
    /// ```
    pub async fn execute<S>(&self, sql: S, params: Params) -> Result<usize, Error>
    where
        S: Into<Cow<'static, str>>,
    {
        self.rate_limit_db().await?;

        let sql = Query {
            sql: sql.into(),
            params,
        };

        self.retry_db_after_leader_change(|| self.execute_req(sql.clone()))
            .await
    }

    /// [`Client::execute`], also reporting the Raft log index of the committed
    /// entry when this node can know it. See [`WriteAck`].
    pub async fn execute_acked<S>(&self, sql: S, params: Params) -> Result<WriteAck<usize>, Error>
    where
        S: Into<Cow<'static, str>>,
    {
        self.rate_limit_db().await?;

        let sql = Query {
            sql: sql.into(),
            params,
        };

        let (result, log_index) = self
            .retry_db_after_leader_change(|| self.execute_ack_req(sql.clone()))
            .await?;
        Ok(WriteAck { result, log_index })
    }

    #[inline(always)]
    async fn execute_req(&self, sql: Query) -> Result<usize, Error> {
        self.execute_ack_req(sql).await.map(|(result, _)| result)
    }

    #[inline(always)]
    async fn execute_ack_req(&self, sql: Query) -> Result<(usize, Option<u64>), Error> {
        if let Some(state) = self.is_leader_db_with_state().await {
            let res = state
                .raft_db
                .raft
                .client_write(QueryWrite::Execute(sql))
                .await?;
            let log_index = res.log_id.index;
            let resp: Response = res.data;
            match resp {
                Response::Execute(res) => res.result.map(|rows| (rows, Some(log_index))),
                _ => unreachable!(),
            }
        } else {
            let (ack, rx) = oneshot::channel();
            self.inner
                .tx_client_db
                .send_async(ClientStreamReq::Execute(ClientExecutePayload {
                    request_id: self.new_request_id(),
                    sql,
                    ack,
                }))
                .await
                .map_err(|err| Error::Error(err.to_string().into()))?;
            let res = rx
                .await
                .map_err(|_| Error::Connect("client stream manager stopped".into()))??;
            match res {
                ApiStreamResponsePayload::Execute(res) => res.map(|rows| (rows, None)),
                ApiStreamResponsePayload::ExecuteAcked(res) => {
                    res.map(|(rows, log_index)| (rows, Some(log_index)))
                }
                _ => unreachable!(),
            }
        }
    }

    /// Execute a query on the database that includes a `RETURNING` statement.
    ///
    /// Returns the rows mapped to the output type on success. This only works for types that
    /// `impl From<&mut hiqlite::Row<'_>>`
    pub async fn execute_returning_map<S, T>(
        &self,
        sql: S,
        params: Params,
    ) -> Result<Vec<Result<T, Error>>, Error>
    where
        S: Into<Cow<'static, str>>,
        T: for<'a, 'r> From<&'a mut crate::Row<'r>> + Send + 'static,
    {
        self.rate_limit_db().await?;

        let rows: Vec<Result<crate::Row, Error>> = self.execute_returning::<S>(sql, params).await?;
        let mut res: Vec<Result<T, Error>> = Vec::with_capacity(rows.len());
        for row in rows {
            res.push(row.map(|mut row| T::from(&mut row)))
        }
        Ok(res)
    }

    /// [`Client::execute_returning_map`], also reporting the Raft log index of
    /// the committed entry when this node can know it. See [`WriteAck`].
    pub async fn execute_returning_map_acked<S, T>(
        &self,
        sql: S,
        params: Params,
    ) -> Result<WriteAck<Vec<Result<T, Error>>>, Error>
    where
        S: Into<Cow<'static, str>>,
        T: for<'a, 'r> From<&'a mut crate::Row<'r>> + Send + 'static,
    {
        self.rate_limit_db().await?;

        let sql = Query {
            sql: sql.into(),
            params,
        };
        let (rows, log_index) = self
            .retry_db_after_leader_change(|| self.execute_returning_ack_req(sql.clone()))
            .await?;
        let mut result: Vec<Result<T, Error>> = Vec::with_capacity(rows.len());
        for row in rows {
            result.push(row.map(|row| T::from(&mut crate::Row::Owned(row))));
        }
        Ok(WriteAck { result, log_index })
    }

    /// Execute a query on the database that includes a `RETURNING` statement.
    ///
    /// Returns the row mapped to the output type on success. This only works for types that
    /// `impl From<&mut hiqlite::Row<'_>>`.
    ///
    /// Throws an error if not exactly 1 row has been returned.
    pub async fn execute_returning_map_one<S, T>(&self, sql: S, params: Params) -> Result<T, Error>
    where
        S: Into<Cow<'static, str>>,
        T: for<'a, 'r> From<&'a mut crate::Row<'r>> + Send + 'static,
    {
        self.rate_limit_db().await?;

        let mut rows = self.execute_returning_map::<S, T>(sql, params).await?;
        if rows.is_empty() {
            Err(Error::QueryReturnedNoRows("no rows returned".into()))
        } else if rows.len() > 1 {
            Err(Error::Sqlite(
                format!("cannot map {} rows into one", rows.len()).into(),
            ))
        } else {
            rows.swap_remove(0)
        }
    }

    /// Execute a query on the database that includes a `RETURNING` statement.
    /// Returns the raw rows on success.
    pub async fn execute_returning<S>(
        &self,
        sql: S,
        params: Params,
    ) -> Result<Vec<Result<crate::Row<'_>, Error>>, Error>
    where
        S: Into<Cow<'static, str>>,
    {
        self.rate_limit_db().await?;

        let sql = Query {
            sql: sql.into(),
            params,
        };

        let rows = self
            .retry_db_after_leader_change(|| self.execute_returning_req(sql.clone()))
            .await?;

        let mut res: Vec<Result<crate::Row, Error>> = Vec::with_capacity(rows.len());
        for row in rows {
            res.push(row.map(crate::Row::Owned))
        }
        Ok(res)
    }

    /// Execute a query on the database that includes a `RETURNING` statement.
    ///
    /// Returns a single raw row. Will throw an error if rows returned is not exactly 1.
    pub async fn execute_returning_one<S>(
        &self,
        sql: S,
        params: Params,
    ) -> Result<crate::Row<'_>, Error>
    where
        S: Into<Cow<'static, str>>,
    {
        self.rate_limit_db().await?;

        let mut rows = self.execute_returning(sql, params).await?;
        if rows.is_empty() {
            Err(Error::QueryReturnedNoRows("no rows returned".into()))
        } else if rows.len() > 1 {
            Err(Error::Sqlite(
                format!("cannot map {} rows into one", rows.len()).into(),
            ))
        } else {
            rows.swap_remove(0)
        }
    }

    #[inline]
    pub(crate) async fn execute_returning_req(
        &self,
        sql: Query,
    ) -> Result<Vec<Result<RowOwned, Error>>, Error> {
        self.execute_returning_ack_req(sql)
            .await
            .map(|(rows, _)| rows)
    }

    #[inline]
    async fn execute_returning_ack_req(
        &self,
        sql: Query,
    ) -> Result<(Vec<Result<RowOwned, Error>>, Option<u64>), Error> {
        if let Some(state) = self.is_leader_db_with_state().await {
            let res = state
                .raft_db
                .raft
                .client_write(QueryWrite::ExecuteReturning(sql))
                .await?;
            let log_index = res.log_id.index;
            let resp: Response = res.data;
            match resp {
                Response::ExecuteReturning(res) => res.result.map(|rows| (rows, Some(log_index))),
                _ => unreachable!(),
            }
        } else {
            let (ack, rx) = oneshot::channel();
            self.inner
                .tx_client_db
                .send_async(ClientStreamReq::ExecuteReturning(ClientExecutePayload {
                    request_id: self.new_request_id(),
                    sql,
                    ack,
                }))
                .await
                .map_err(|err| Error::Error(err.to_string().into()))?;
            let res = rx
                .await
                .map_err(|_| Error::Connect("client stream manager stopped".into()))??;
            match res {
                ApiStreamResponsePayload::ExecuteReturning(res) => res.map(|rows| (rows, None)),
                ApiStreamResponsePayload::ExecuteReturningAcked(res) => {
                    res.map(|(rows, log_index)| (rows, Some(log_index)))
                }
                _ => unreachable!(),
            }
        }
    }
}
