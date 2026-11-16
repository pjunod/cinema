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

/// The expand, one text for both backends. It reinstalls the claim effects
/// (open-ended source reservation slots): the replicated store installs its
/// base job schema only when a cluster is created, so an upgraded cluster
/// keeps the v1 two-slot trigger unless the expand replaces it.
pub(crate) const EXPAND: &str = concat!(
    include_str!("source_leases_expand.sql"),
    "\n-- next statement\n",
    include_str!("background_job_claim_effects.sql")
);
/// The contract: drops the v1 source-owner objects.
pub(crate) const CONTRACT: &str = include_str!("source_leases_contract.sql");
/// The claim-effects trigger the expand rewrites; part of the lease shape.
pub(crate) const CLAIM_EFFECTS: &str = "background_job_claim_effects";
/// SQLite schema version before the expand and after it.
pub(crate) const PRE_LEASE_VERSION: usize = 132;
pub(crate) const EXPANDED_VERSION: usize = 133;
/// The SQLite schema that first carries `source_leases` (import plans).
pub(crate) const SQLITE_INTRODUCED_SCHEMA: i64 = EXPANDED_VERSION as i64;

/// How long past its expiry a lease still holds capacity: absorbs a renewal
/// racing the sweep and clock skew between nodes. Not a proof window.
pub const SOURCE_LEASE_GRACE_MS: i64 = 60_000;

/// Lease alias `held` still holds source capacity at `now`.
pub(crate) fn counted(now: &str) -> String {
    format!(lease_counted!("{now}"), now = now)
}
/// Lease alias `held` serves live demand at `now`.
pub(crate) fn live(now: &str) -> String {
    format!(lease_live!("{now}"), now = now)
}
/// A playback window overlapping source key `key` refuses background reads.
pub(crate) fn foreground_window(key: &str, now: &str) -> String {
    format!(
        claim_foreground_window!("{key}", "{now}"),
        key = key,
        now = now
    )
}
/// `node`, or the mapped domain of `key`, has no reader slot left at all.
pub(crate) fn source_holders(key: &str, node: &str, now: &str) -> String {
    source_holders_except(key, node, now, "")
}
/// [`source_holders`], counting only leases `held` that also match `extra`
/// (`" AND ..."`).
pub(crate) fn source_holders_except(key: &str, node: &str, now: &str, extra: &str) -> String {
    format!(
        claim_source_holders!("{key}", "{node}", "{now}", "{extra}"),
        key = key,
        node = node,
        now = now,
        extra = extra
    )
}
/// Maintenance readers fill `node`, or the mapped domain of `key`.
pub(crate) fn maintenance_reader(key: &str, node: &str, now: &str) -> String {
    maintenance_reader_except(key, node, now, "")
}
/// [`maintenance_reader`], counting only leases `held` that match `extra`.
pub(crate) fn maintenance_reader_except(key: &str, node: &str, now: &str, extra: &str) -> String {
    format!(
        claim_maintenance_reader!("{key}", "{node}", "{now}", "{extra}"),
        key = key,
        node = node,
        now = now,
        extra = extra
    )
}
/// SQLite's `hex()` of the text's UTF-8 bytes.
pub(crate) fn hex_upper(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02X}")).collect()
}

/// A lease id is a kind prefix and the owner's identity, never free text.
pub(crate) fn lease_id_valid(lease_id: &str) -> bool {
    !lease_id.is_empty()
        && lease_id.len() <= 1024
        && lease_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'-' | b'_' | b'.'))
}

pub(crate) const SHAPE_SQL: &str = "SELECT name,COALESCE(sql,'') AS sql FROM sqlite_master WHERE (name GLOB 'source_lease*' OR name='background_job_claim_effects') AND sql IS NOT NULL ORDER BY name";

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
        .filter(|(name, _)| name.starts_with("source_lease") || name == CLAIM_EFFECTS)
        .map(|(name, sql)| (name.clone(), canonical(sql)))
        .collect()
}
fn canonical(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}
static EXPANDED_CATALOGUE: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
    super::sqlite::catalogue_through(EXPANDED_VERSION).expect("expanded catalogue")
});
static CONTRACTED_CATALOGUE: LazyLock<Vec<(String, String)>> =
    LazyLock::new(|| contracted_catalogue().expect("contracted catalogue"));

/// Whether `installed` is another extension's objects as this binary
/// installs them (`own`), or as the source-lease expand or contract left
/// those same names. The four extensions the expand rewrites (source
/// arbitration, dispatch turns, custody retention, storage-domain pending)
/// accept exactly these three shapes and refuse anything else.
pub(crate) fn accepts_extension_shape(
    installed: &[(String, String)],
    own: &[(String, String)],
) -> bool {
    let canonical_rows = |rows: &[(String, String)]| {
        let mut rows = rows
            .iter()
            .map(|(name, sql)| (name.clone(), canonical(sql)))
            .collect::<Vec<_>>();
        rows.sort();
        rows
    };
    let installed = canonical_rows(installed);
    let variant = |catalogue: &[(String, String)]| {
        canonical_rows(
            &catalogue
                .iter()
                .filter(|(name, _)| own.iter().any(|(own, _)| own == name))
                .cloned()
                .collect::<Vec<_>>(),
        )
    };
    installed == canonical_rows(own)
        || installed == variant(&EXPANDED_CATALOGUE)
        || installed == variant(&CONTRACTED_CATALOGUE)
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
/// contracted. Anything else, including lease objects beside a claim-effects
/// trigger the expand did not write, is a partial or mutated catalogue.
pub(crate) fn version(installed: &[(String, String)]) -> Result<Option<i64>, StoreError> {
    let installed = shape(installed);
    if !installed
        .iter()
        .any(|(name, _)| name.starts_with("source_lease"))
    {
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

/// Test fixtures that build a hand-made catalogue: add the dispatch-turn
/// objects (when absent) and the expand, once.
#[cfg(test)]
pub(crate) fn install_for_fixture(conn: &rusqlite::Connection) {
    let present = |name: &str| {
        conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name=?1",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .expect("fixture catalogue")
            > 0
    };
    if present("source_leases") {
        return;
    }
    if !present("dispatch_turn_ready") {
        conn.execute_batch(super::background_job_turns::SCHEMA)
            .expect("fixture dispatch turns");
    }
    // The expand restores the v85/v86 retention triggers; a hand-made
    // catalogue without queue retention keeps none.
    let retention = present("background_job_enqueue_command");
    // The lease objects only: a hand-made catalogue has no attempt ledger
    // for the claim effects to write.
    conn.execute_batch(include_str!("source_leases_expand.sql"))
        .expect("fixture source leases");
    if !retention {
        conn.execute_batch(
            "DROP TRIGGER background_job_enqueue_command; DROP TRIGGER background_job_maintenance_command;",
        )
        .expect("fixture without retention");
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
