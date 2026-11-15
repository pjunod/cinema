//! The replicated client the cluster membership manager issues its SQL
//! through.
//!
//! The Store's own operations go through `TimedClient`, which counts each one
//! by class and by the Store method that issued it. The membership manager
//! talks to hiqlite directly (it must keep its own timeouts and retries), so
//! before this type its quorum reads, local reads and writes reached no
//! metric at all. Every SQL method here takes a `label: &'static str` first —
//! the name of the function that issued it — times the call into the same
//! process-wide class counters `TimedClient` uses, attributes it to the
//! current HTTP request or to background work, and delegates unchanged: no
//! timeout, retry, validation or consistency change is added.
//!
//! Each delegated future is boxed. The manager's call graphs are deep and a
//! debug build lays every awaited future out inline in its caller, so adding
//! the timing layer around an inline hiqlite future overflowed a test
//! thread's 2 MiB stack; the box keeps each caller's state no larger than it was
//! before the layer, for one allocation on a path that runs tens of times a
//! second.
//!
//! The raw [`Client`] is a private field and no method hands it out, so an
//! unlabelled SQL call on this type does not compile. The few non-SQL
//! handles the manager needs are forwarded by name.

use std::borrow::Cow;

use hiqlite::{Client, Params, Row};

use super::hiqlite::{time_counted_operation, StoreOperationClass};

/// The database Raft metrics [`CountedClient::metrics_db`] forwards.
pub type DbRaftMetrics = hiqlite::DbRaftMetrics;

/// A hiqlite [`Client`] whose replicated SQL is counted by a static label.
/// See the module documentation.
#[derive(Clone)]
pub struct CountedClient {
    client: Client,
    membership: bool,
}

impl CountedClient {
    /// The membership manager's client: its operations are also counted in
    /// `plurx_cluster_membership_operations_by_method_total`.
    #[must_use]
    pub(crate) fn membership(client: Client) -> Self {
        Self {
            client,
            membership: true,
        }
    }

    /// A counted client outside the membership manager: the store's
    /// construction-time provisioning helper it shares with the manager, and
    /// the cluster's quorum watermark sampler. Counted by class and label,
    /// not as membership operations.
    #[must_use]
    pub(crate) fn outside_membership(client: Client) -> Self {
        Self {
            client,
            membership: false,
        }
    }

    /// A linearizable read (`authority_read`).
    pub async fn query_consistent_map<T, S>(
        &self,
        label: &'static str,
        sql: S,
        params: Params,
    ) -> Result<Vec<T>, hiqlite::Error>
    where
        T: for<'a, 'r> From<&'a mut Row<'r>> + Send + 'static,
        S: Into<Cow<'static, str>>,
    {
        time_counted_operation(
            StoreOperationClass::AuthorityRead,
            label,
            self.membership,
            Box::pin(self.client.query_consistent_map(sql, params)),
            |_| true,
        )
        .await
    }

    /// A read of this node's replica (`local_read`).
    pub async fn query_map<T, S>(
        &self,
        label: &'static str,
        sql: S,
        params: Params,
    ) -> Result<Vec<T>, hiqlite::Error>
    where
        T: for<'a, 'r> From<&'a mut Row<'r>> + Send + 'static,
        S: Into<Cow<'static, str>>,
    {
        time_counted_operation(
            StoreOperationClass::LocalRead,
            label,
            self.membership,
            Box::pin(self.client.query_map(sql, params)),
            |_| true,
        )
        .await
    }

    /// One replicated statement (`write`).
    pub async fn execute<S>(
        &self,
        label: &'static str,
        sql: S,
        params: Params,
    ) -> Result<usize, hiqlite::Error>
    where
        S: Into<Cow<'static, str>>,
    {
        time_counted_operation(
            StoreOperationClass::Write,
            label,
            self.membership,
            Box::pin(self.client.execute(sql, params)),
            |_| true,
        )
        .await
    }

    /// One replicated transaction (`write`). A statement that failed inside a
    /// committed transaction records the operation's outcome as an error.
    pub async fn txn<C, Q>(
        &self,
        label: &'static str,
        statements: Q,
    ) -> Result<Vec<Result<usize, hiqlite::Error>>, hiqlite::Error>
    where
        Q: IntoIterator<Item = (C, Params)>,
        C: Into<Cow<'static, str>>,
    {
        time_counted_operation(
            StoreOperationClass::Write,
            label,
            self.membership,
            Box::pin(self.client.txn(statements)),
            |results| results.iter().all(Result::is_ok),
        )
        .await
    }

    /// The leader's quorum-confirmed commit watermark: a linearizable read
    /// with no SQL (`authority_read`).
    pub async fn db_quorum_watermark(
        &self,
        label: &'static str,
    ) -> Result<hiqlite::DbQuorumWatermark, hiqlite::Error> {
        time_counted_operation(
            StoreOperationClass::AuthorityRead,
            label,
            self.membership,
            Box::pin(self.client.db_quorum_watermark()),
            |_| true,
        )
        .await
    }

    /// This node's database Raft metrics. Not a replicated operation: a local
    /// node reads its own watch channel.
    pub async fn metrics_db(&self) -> Result<DbRaftMetrics, hiqlite::Error> {
        self.client.metrics_db().await
    }

    /// Hiqlite patch 30: stop this node's own database Raft election timer,
    /// for a node that is stopping. Process-local; not a replicated
    /// operation.
    pub fn disable_local_db_elections(&self) -> Result<(), hiqlite::Error> {
        self.client.disable_local_db_elections()
    }

    /// Hiqlite patch 31: hold this node's database Raft proposals, waiting
    /// for in-flight ones until `deadline` (`None` when they do not finish by
    /// then). Process-local; not a replicated operation.
    pub async fn hold_local_db_proposals(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<Option<hiqlite::ProposalHold>, hiqlite::Error> {
        self.client.hold_local_db_proposals(deadline).await
    }

    /// Hiqlite patch 31: wait, by `deadline`, for this node's own database
    /// Raft metrics to meet `condition`. Process-local; not a replicated
    /// operation.
    pub async fn wait_local_db_metrics<F>(
        &self,
        deadline: tokio::time::Instant,
        condition: F,
    ) -> Result<DbRaftMetrics, hiqlite::Error>
    where
        F: Fn(&DbRaftMetrics) -> bool + Send,
    {
        self.client.wait_local_db_metrics(deadline, condition).await
    }

    /// Hiqlite's validation-only precommit hold, for the membership tests
    /// that drive a real transaction into it. Not compiled into a daemon.
    #[cfg(all(test, feature = "cluster-read-cost-validation"))]
    pub(crate) async fn validation_hold_transaction_before_commit(
        &self,
        exact_statement: String,
        until: std::time::Instant,
    ) -> Result<
        (
            tokio::sync::oneshot::Receiver<()>,
            std::sync::mpsc::Sender<()>,
        ),
        hiqlite::Error,
    > {
        self.client
            .validation_hold_transaction_before_commit(exact_statement, until)
            .await
    }

    /// The node clock admission installed at local startup. Process-local.
    pub fn local_membership_admission(
        &self,
    ) -> Result<
        std::sync::Arc<dyn hiqlite::membership_admission::MembershipAdmission>,
        hiqlite::Error,
    > {
        self.client.local_membership_admission()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct Probe;
    impl From<&mut Row<'_>> for Probe {
        fn from(_: &mut Row<'_>) -> Self {
            Self
        }
    }

    /// A client of a node that is not there: each call is polled long enough
    /// to start its timer and then ends, in an error or by being dropped.
    /// Either way the operation is counted, which is what is under test.
    async fn unreachable() -> Client {
        Client::remote(
            vec!["127.0.0.1:1".to_owned()],
            false,
            false,
            "unused".to_owned(),
            true,
            None,
        )
        .await
        .expect("construct a remote client")
    }

    async fn bounded<T>(operation: impl std::future::Future<Output = T>) {
        let _ = tokio::time::timeout(Duration::from_millis(200), operation).await;
    }

    fn counted(table: &[(&'static str, [u64; 3])], label: &str) -> Option<[u64; 3]> {
        table
            .iter()
            .find(|(method, _)| *method == label)
            .map(|(_, counts)| *counts)
    }

    /// Each method records its own consistency class under the caller's
    /// label, outside a request as background work, and the membership
    /// client's calls are membership operations while the store's are not.
    /// The labels are unique to this test, so concurrent tests cannot move
    /// them.
    #[tokio::test]
    async fn each_counted_call_lands_in_its_class_under_its_label() {
        let raw = unreachable().await;
        let client = CountedClient::membership(raw.clone());
        bounded(client.query_consistent_map::<Probe, _>(
            "counted_client_test_consistent",
            "SELECT 1",
            hiqlite::params!(),
        ))
        .await;
        bounded(client.query_map::<Probe, _>(
            "counted_client_test_local",
            "SELECT 1",
            hiqlite::params!(),
        ))
        .await;
        bounded(client.execute(
            "counted_client_test_execute",
            "SELECT 1",
            hiqlite::params!(),
        ))
        .await;
        bounded(client.txn(
            "counted_client_test_txn",
            [("SELECT 1", hiqlite::params!())],
        ))
        .await;
        bounded(client.db_quorum_watermark("counted_client_test_watermark")).await;
        let store = CountedClient::outside_membership(raw);
        bounded(store.query_map::<Probe, _>(
            "counted_client_test_store_local",
            "SELECT 1",
            hiqlite::params!(),
        ))
        .await;

        let membership = crate::store::cluster_membership_operations_by_method();
        let background = crate::store::background_store_operations_by_method();
        for (label, expected) in [
            ("counted_client_test_consistent", [0, 1, 0]),
            ("counted_client_test_local", [1, 0, 0]),
            ("counted_client_test_execute", [0, 0, 1]),
            ("counted_client_test_txn", [0, 0, 1]),
            ("counted_client_test_watermark", [0, 1, 0]),
        ] {
            assert_eq!(counted(&membership, label), Some(expected), "{label}");
            assert_eq!(counted(&background, label), Some(expected), "{label}");
        }
        assert_eq!(
            counted(&background, "counted_client_test_store_local"),
            Some([1, 0, 0])
        );
        assert_eq!(
            counted(&membership, "counted_client_test_store_local"),
            None,
            "the store's provisioning client is not the membership manager"
        );
    }
}
