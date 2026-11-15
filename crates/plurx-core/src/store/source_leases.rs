//! One lease table for every source reader: who reads source media, on which
//! node, until when. A lease's expiry is a column; a row holds source
//! capacity while `expires_at_ms > now - SOURCE_LEASE_GRACE_MS`, so capacity
//! frees on time whether or not the sweep runs.
//!
//! The expand (v2) adds the table, the owner triggers that write and renew
//! job and analysis leases inside their owners' own transactions, replaces
//! the dependents that read the v1 owner tables, and carries live holders
//! over. The contract (v3) drops the v1 objects once every running binary
//! reads leases. See `docs/server/SOURCE-LEASE-TABLE-AND-NODE-CAPACITY-BUILD.md`.
use crate::error::StoreError;
use std::sync::LazyLock;

/// The expand, as the replicated installer runs it.
pub(crate) const EXPAND: &str = include_str!("source_leases_expand.sql");
/// The contract: drops the v1 source-owner objects.
pub(crate) const CONTRACT: &str = include_str!("source_leases_contract.sql");
/// SQLite applies its base job schema once, at its own migration; the expand
/// therefore reinstalls the claim effects (open-ended source reservation
/// slots). The replicated store reinstalls them with its base schema.
pub(crate) const SQLITE_EXPAND: &str = concat!(
    include_str!("source_leases_expand.sql"),
    "\n-- next statement\n",
    include_str!("background_job_claim_effects.sql")
);
/// SQLite schema version before the expand and after it.
pub(crate) const PRE_LEASE_VERSION: usize = 132;
pub(crate) const EXPANDED_VERSION: usize = 133;

pub(crate) const SHAPE_SQL: &str = "SELECT name,COALESCE(sql,'') AS sql FROM sqlite_master WHERE name GLOB 'source_lease*' AND sql IS NOT NULL ORDER BY name";

/// Statements of an extension text, one per `-- next statement`.
pub(crate) fn statements(sql: &str) -> Vec<String> {
    sql.split("-- next statement")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn shape(catalogue: &[(String, String)]) -> Vec<(String, String)> {
    catalogue
        .iter()
        .filter(|(name, _)| name.starts_with("source_lease"))
        .map(|(name, sql)| (name.clone(), canonical(sql)))
        .collect()
}
fn canonical(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}
static EXPANDED: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
    shape(&super::sqlite::catalogue_through(EXPANDED_VERSION).expect("expanded catalogue"))
});
static CONTRACTED: LazyLock<Vec<(String, String)>> =
    LazyLock::new(|| shape(&contracted_catalogue().expect("contracted catalogue")));

/// The expanded catalogue with the contract applied.
pub(crate) fn contracted_catalogue() -> Result<Vec<(String, String)>, StoreError> {
    let conn = super::sqlite::SqliteStore::open_migrated_through(EXPANDED_VERSION)?;
    conn.execute_batch(CONTRACT)?;
    super::sqlite::catalogue(&conn)
}

/// `None`: no lease objects (v1 only). `Some(2)`: expanded. `Some(3)`:
/// contracted. Anything else is a partial or mutated catalogue.
pub(crate) fn version(installed: &[(String, String)]) -> Result<Option<i64>, StoreError> {
    let installed = shape(installed);
    if installed.is_empty() {
        Ok(None)
    } else if installed == *EXPANDED {
        Ok(Some(2))
    } else if installed == *CONTRACTED {
        Ok(Some(3))
    } else {
        Err(StoreError::Migration(
            "source lease schema partial or incompatible".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sqlite_migrations_are_the_expand_then_the_contract() {
        let pre = super::super::sqlite::catalogue_through(PRE_LEASE_VERSION).expect("pre");
        assert_eq!(version(&pre).expect("pre-lease"), None);
        let expanded = super::super::sqlite::catalogue_through(EXPANDED_VERSION).expect("v2");
        assert_eq!(version(&expanded).expect("expanded"), Some(2));
        let contracted = contracted_catalogue().expect("v3");
        assert_eq!(version(&contracted).expect("contracted"), Some(3));
        assert!(
            expanded
                .iter()
                .any(|(name, _)| name == "source_lease_bridge_insert")
                && !contracted
                    .iter()
                    .any(|(name, _)| name.starts_with("source_lease_bridge")),
            "the bridge lives only between expand and contract"
        );
        let mut partial = expanded.clone();
        partial.retain(|(name, _)| name != "source_lease_job_renew");
        assert!(
            version(&partial).is_err(),
            "a missing owner trigger is refused"
        );
    }
}
