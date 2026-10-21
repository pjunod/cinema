//! Contract-feature-only fault boundary around REAL production transactions.
//! No alternative Store, physical proof, production bypass or second database.
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Notify;

use super::{
    schema,
    transactions::{self, Backend},
    *,
};

pub enum TransactionFault {
    LostAcknowledgement,
    FinalAssertion,
    BeforeCommit {
        submitted: Arc<Notify>,
        release: Arc<Notify>,
    },
}

struct Fault<'a, B> {
    store: &'a B,
    fault: TransactionFault,
}

#[async_trait]
impl<B: Backend> Backend for Fault<'_, B> {
    async fn attempt_layout(&self) -> Result<schema::Layout, StoreError> {
        self.store.attempt_layout().await
    }
    async fn attempt_read(&self, sql: String) -> Result<Vec<String>, StoreError> {
        self.store.attempt_read(sql).await
    }
    async fn attempt_transaction(&self, mut sql: Vec<String>) -> Result<(), StoreError> {
        match &self.fault {
            TransactionFault::FinalAssertion => sql.push(schema::assertion("0")),
            TransactionFault::BeforeCommit { submitted, release } => {
                submitted.notify_one();
                release.notified().await;
            }
            TransactionFault::LostAcknowledgement => {}
        }
        self.store.attempt_transaction(sql).await?;
        if matches!(self.fault, TransactionFault::LostAcknowledgement) {
            return Err(StoreError::Database(
                "contract lost acknowledgement AFTER actual COMMIT".into(),
            ));
        }
        Ok(())
    }
    async fn attempt_activate(
        &self,
        plan: &transactions::AdoptionPlan,
    ) -> Result<Option<MediaSessionActivationOutcome>, StoreError> {
        transactions::commit_adoption(self, plan).await
    }
}

/// Used only by the contract-feature target with its isolated three voters.
pub async fn claim_hiqlite(
    store: &crate::store::HiqliteAuthStore,
    input: &AttemptClaim,
    fault: TransactionFault,
) -> Result<AttemptMutation, StoreError> {
    transactions::claim(&Fault { store, fault }, input).await
}

pub async fn prepared_hiqlite(
    store: &crate::store::HiqliteAuthStore,
    input: &PreparedAttempt,
    fault: TransactionFault,
) -> Result<AttemptMutation, StoreError> {
    transactions::prepared(&Fault { store, fault }, input).await
}

pub async fn adopt_hiqlite(
    store: &crate::store::HiqliteAuthStore,
    input: &AdaptiveAdoption,
    fault: TransactionFault,
) -> Result<AttemptAdoption, StoreError> {
    transactions::adopt(&Fault { store, fault }, input).await
}

/// Prefix for the existing feature-only EXCLUSIVE contract reset. This is not
/// a normal mutation or a route to Closed. It destroys only the test database's
/// retained fixture history, reinstalls all guards in the SAME transaction and
/// refuses unknown/partial shapes rather than treating them as empty.
pub fn reset_statements(layout: schema::Layout) -> Vec<String> {
    let mut statements = vec![schema::assertion(&schema::ready_guard(layout))];
    statements.extend(
        schema::trigger_objects(layout)
            .into_iter()
            .map(|(name, _)| format!("DROP TRIGGER {name}")),
    );
    for table in [
        "media_resource_operations",
        "media_resource_attempts",
        "media_resource_requests",
    ] {
        statements.push(format!("DELETE FROM {table}"));
    }
    statements.push(
        "UPDATE media_resource_schema SET revision=0 WHERE singleton=1 AND mutation_claim IS NULL"
            .into(),
    );
    statements.extend(
        schema::trigger_objects(layout)
            .into_iter()
            .map(|(_, sql)| sql),
    );
    statements.push(schema::assertion(&schema::ready_guard(layout)));
    statements
}
