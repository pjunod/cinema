//! Same-file recognized v1 through v7 upgrades; no missing-authority repair.
use super::{validate, DDL, DDL_V2, DDL_V3, DDL_V4, DDL_V5, DDL_V6};
use crate::resource_custody::{
    codec, storage::Witness, transaction, CommitCertainty, CustodyError, ErrorKind,
};
use rusqlite::{Connection, TransactionBehavior};

const TABLES: [&str; 6] = [
    "journal_meta",
    "owners",
    "obligations",
    "family_roles",
    "members",
    "operations",
];

/// Caller holds the existing installation/daemon lock and original file guards.
/// No owner registration or launch authority may be exposed before this returns.
pub(in crate::resource_custody) fn upgrade(
    connection: &mut Connection,
    witness: &Witness,
) -> Result<(), CustodyError> {
    validate(connection)?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version == 1 {
        upgrade_v1_to_v2(connection, witness)?;
    }
    if version <= 2 {
        upgrade_v2_to_v3(connection, witness)?;
    }
    if version <= 3 {
        upgrade_v3_to_v4(connection, witness)?;
    }
    if version <= 4 {
        upgrade_v4_to_v5(connection, witness)?;
    }
    if version <= 5 {
        upgrade_v5_to_v6(connection, witness)?;
    }
    if version <= 6 {
        upgrade_v6_to_v7(connection, witness)?;
    }
    Ok(())
}

pub(super) fn upgrade_v1_to_v2(
    connection: &mut Connection,
    witness: &Witness,
) -> Result<(), CustodyError> {
    validate(connection)?;
    let foreign_keys: bool = connection.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    if !foreign_keys {
        return Err(codec::invalid());
    }
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Recognize/validate again INSIDE the transaction, including every historical
    // document and operation. A version stamp or syntactically valid DDL is not
    // enough. This runs before the first persistent mutation.
    validate(&tx)?;
    let before = transaction::snapshot(&tx, witness)?;
    for table in &TABLES {
        tx.execute_batch(&format!(
            "CREATE TEMP TABLE custody_upgrade_{table} AS SELECT * FROM main.{table};"
        ))?;
    }
    // Exact v1 has no inbound FK to either rebuilt table. The recognized catalog
    // above excludes extra tables/triggers/indexes. Keep foreign_keys ON.
    tx.execute_batch("DROP TABLE members; DROP TABLE journal_meta;")?;
    let canonical = Connection::open_in_memory()?;
    canonical.execute_batch(DDL_V2)?;
    for name in [
        "journal_meta",
        "members",
        "unique_member_identity",
        "retained_asset_owners",
    ] {
        let sql: String =
            canonical.query_row("SELECT sql FROM sqlite_schema WHERE name=?1", [name], |r| {
                r.get(0)
            })?;
        // This is the independent, compiled-in canonical schema, never catalog
        // string replacement or execution of SQL read from the authority file.
        tx.execute_batch(&sql)?;
    }
    tx.execute_batch(
        "INSERT INTO journal_meta SELECT singleton,2,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id FROM temp.custody_upgrade_journal_meta;
         INSERT INTO members SELECT * FROM temp.custody_upgrade_members;
         PRAGMA user_version=2;"
    )?;
    validate(&tx)?;
    for table in &TABLES {
        let projection = if *table == "journal_meta" {
            "singleton,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id"
        } else {
            "*"
        };
        // SQL BINARY equality retains original TEXT/BLOB bytes, NULLs and u64
        // blobs. Both directions plus row counts also detect duplicates/loss.
        let unchanged: bool = tx.query_row(&format!(
            "SELECT NOT EXISTS(SELECT {projection} FROM main.{table} EXCEPT SELECT {projection} FROM temp.custody_upgrade_{table}) AND NOT EXISTS(SELECT {projection} FROM temp.custody_upgrade_{table} EXCEPT SELECT {projection} FROM main.{table}) AND (SELECT count(*) FROM main.{table})=(SELECT count(*) FROM temp.custody_upgrade_{table})"
        ), [], |r| r.get(0))?;
        if !unchanged {
            return Err(codec::invalid());
        }
    }
    let after = transaction::snapshot(&tx, witness)?;
    if before.total != after.total
        || before.trial_groups != after.trial_groups
        || before.obligations.len() != after.obligations.len()
        || !before
            .family_authority
            .same_complete_rows(&after.family_authority)
        || before.protected_paths != after.protected_paths
    {
        return Err(codec::invalid());
    }
    for table in &TABLES {
        tx.execute_batch(&format!("DROP TABLE temp.custody_upgrade_{table};"))?;
    }
    tx.commit().map_err(|_| {
        let mut error = CustodyError::new(
            ErrorKind::Storage,
            "custody schema upgrade commit uncertain",
        );
        error.certainty = CommitCertainty::Unknown;
        error
    })
}

/// Preserve every old SQL value byte-for-byte. The only new interpretation is
/// that an old allocation conservatively remains an active bounded producer.
pub(super) fn upgrade_v2_to_v3(
    connection: &mut Connection,
    witness: &Witness,
) -> Result<(), CustodyError> {
    validate(connection)?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != 2 {
        return Err(codec::invalid());
    }
    let foreign_keys: bool = connection.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    if !foreign_keys {
        return Err(codec::invalid());
    }
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    validate(&tx)?;
    let before = transaction::snapshot(&tx, witness)?;
    for table in TABLES.into_iter().chain(["retained_asset_owners"]) {
        tx.execute_batch(&format!(
            "CREATE TEMP TABLE custody_upgrade_v3_{table} AS SELECT * FROM main.{table};"
        ))?;
    }
    // The original v2 obligations DDL is unchanged by this migration. SQLite's
    // ADD COLUMN produces the same canonical catalog text as v3.sql.
    let canonical = Connection::open_in_memory()?;
    canonical.execute_batch(DDL_V3)?;
    let journal_meta: String = canonical.query_row(
        "SELECT sql FROM sqlite_schema WHERE name='journal_meta'",
        [],
        |r| r.get(0),
    )?;
    tx.execute_batch("DROP TABLE journal_meta;")?;
    tx.execute_batch(&journal_meta)?;
    tx.execute_batch(
        "INSERT INTO journal_meta SELECT singleton,3,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id FROM temp.custody_upgrade_v3_journal_meta;"
    )?;
    tx.execute_batch("ALTER TABLE obligations ADD COLUMN memory_claim TEXT NOT NULL DEFAULT 'bounded_active' CHECK(memory_claim IN ('bounded_active','bounded_resident','unknown_producer'));")?;
    tx.execute_batch("PRAGMA user_version=3;")?;
    validate(&tx)?;
    for table in TABLES.into_iter().chain(["retained_asset_owners"]) {
        let projection = match table {
            "journal_meta" => "singleton,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id",
            "obligations" => "obligation_id,owner_id,graph_id,allocation_epoch,resources,trial_group_id,phase,sequence,last_operation_id,manifest_json,hold_reason,settlement_json",
            _ => "*",
        };
        let unchanged: bool = tx.query_row(&format!(
            "SELECT NOT EXISTS(SELECT {projection} FROM main.{table} EXCEPT SELECT {projection} FROM temp.custody_upgrade_v3_{table}) AND NOT EXISTS(SELECT {projection} FROM temp.custody_upgrade_v3_{table} EXCEPT SELECT {projection} FROM main.{table}) AND (SELECT count(*) FROM main.{table})=(SELECT count(*) FROM temp.custody_upgrade_v3_{table})"
        ), [], |r| r.get(0))?;
        if !unchanged {
            return Err(codec::invalid());
        }
    }
    let all_default: bool = tx.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM obligations WHERE memory_claim!='bounded_active')",
        [],
        |r| r.get(0),
    )?;
    if !all_default {
        return Err(codec::invalid());
    }
    let after = transaction::snapshot(&tx, witness)?;
    if before.total != after.total
        || before.trial_groups != after.trial_groups
        || before.obligations.len() != after.obligations.len()
        || !before
            .family_authority
            .same_complete_rows(&after.family_authority)
        || before.retained_assets.len() != after.retained_assets.len()
        || before.protected_paths != after.protected_paths
    {
        return Err(codec::invalid());
    }
    for table in TABLES.into_iter().chain(["retained_asset_owners"]) {
        tx.execute_batch(&format!("DROP TABLE temp.custody_upgrade_v3_{table};"))?;
    }
    tx.commit().map_err(|_| {
        let mut error = CustodyError::new(
            ErrorKind::Storage,
            "custody schema upgrade commit uncertain",
        );
        error.certainty = CommitCertainty::Unknown;
        error
    })
}

/// Add only the append-only recovered-holder overlay. Historical obligations,
/// operations, owners and retained custody keep their original bytes and owner.
pub(super) fn upgrade_v3_to_v4(
    connection: &mut Connection,
    witness: &Witness,
) -> Result<(), CustodyError> {
    validate(connection)?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let foreign_keys: bool = connection.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    if version != 3 || !foreign_keys {
        return Err(codec::invalid());
    }
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    validate(&tx)?;
    let before = transaction::snapshot(&tx, witness)?;
    tx.execute_batch(
        "CREATE TEMP TABLE custody_upgrade_v4_meta AS SELECT * FROM main.journal_meta;",
    )?;
    let canonical = Connection::open_in_memory()?;
    canonical.execute_batch(DDL_V4)?;
    let meta_sql: String = canonical.query_row(
        "SELECT sql FROM sqlite_schema WHERE name='journal_meta'",
        [],
        |r| r.get(0),
    )?;
    let overlay_sql: String = canonical.query_row(
        "SELECT sql FROM sqlite_schema WHERE name='recovered_s_holders'",
        [],
        |r| r.get(0),
    )?;
    tx.execute_batch("DROP TABLE journal_meta;")?;
    tx.execute_batch(&meta_sql)?;
    tx.execute_batch(
        "INSERT INTO journal_meta SELECT singleton,4,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id FROM temp.custody_upgrade_v4_meta;",
    )?;
    tx.execute_batch(&overlay_sql)?;
    tx.execute_batch("PRAGMA user_version=4;")?;
    validate(&tx)?;
    let unchanged: bool = tx.query_row(
        "SELECT NOT EXISTS(SELECT singleton,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id FROM main.journal_meta EXCEPT SELECT singleton,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id FROM temp.custody_upgrade_v4_meta) AND NOT EXISTS(SELECT singleton,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id FROM temp.custody_upgrade_v4_meta EXCEPT SELECT singleton,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id FROM main.journal_meta) AND (SELECT count(*) FROM main.journal_meta)=(SELECT count(*) FROM temp.custody_upgrade_v4_meta)",
        [], |r| r.get(0),
    )?;
    if !unchanged {
        return Err(codec::invalid());
    }
    let after = transaction::snapshot(&tx, witness)?;
    if before.total != after.total
        || before.trial_groups != after.trial_groups
        || before.obligations.len() != after.obligations.len()
        || !before
            .family_authority
            .same_complete_rows(&after.family_authority)
        || before.retained_assets.len() != after.retained_assets.len()
        || before.protected_paths != after.protected_paths
    {
        return Err(codec::invalid());
    }
    tx.execute_batch("DROP TABLE temp.custody_upgrade_v4_meta;")?;
    tx.commit().map_err(|_| {
        let mut error = CustodyError::new(
            ErrorKind::Storage,
            "custody schema upgrade commit uncertain",
        );
        error.certainty = CommitCertainty::Unknown;
        error
    })
}

/// Add a distinct producer-custodian overlay. The historical producer row,
/// including its original owner and typed proof chain, retains its bytes.
pub(super) fn upgrade_v4_to_v5(
    connection: &mut Connection,
    witness: &Witness,
) -> Result<(), CustodyError> {
    validate(connection)?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let foreign_keys: bool = connection.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    if version != 4 || !foreign_keys {
        return Err(codec::invalid());
    }
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    validate(&tx)?;
    let before = transaction::snapshot(&tx, witness)?;
    tx.execute_batch(
        "CREATE TEMP TABLE custody_upgrade_v5_meta AS SELECT * FROM main.journal_meta;",
    )?;
    let canonical = Connection::open_in_memory()?;
    canonical.execute_batch(DDL_V5)?;
    let meta_sql: String = canonical.query_row(
        "SELECT sql FROM sqlite_schema WHERE name='journal_meta'",
        [],
        |r| r.get(0),
    )?;
    let overlay_sql: String = canonical.query_row(
        "SELECT sql FROM sqlite_schema WHERE name='recovered_r_holders'",
        [],
        |r| r.get(0),
    )?;
    tx.execute_batch("DROP TABLE journal_meta;")?;
    tx.execute_batch(&meta_sql)?;
    tx.execute_batch(
        "INSERT INTO journal_meta SELECT singleton,5,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id FROM temp.custody_upgrade_v5_meta;",
    )?;
    tx.execute_batch(&overlay_sql)?;
    tx.execute_batch("PRAGMA user_version=5;")?;
    validate(&tx)?;
    let unchanged: bool = tx.query_row(
        "SELECT NOT EXISTS(SELECT singleton,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id FROM main.journal_meta EXCEPT SELECT singleton,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id FROM temp.custody_upgrade_v5_meta) AND NOT EXISTS(SELECT singleton,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id FROM temp.custody_upgrade_v5_meta EXCEPT SELECT singleton,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id FROM main.journal_meta) AND (SELECT count(*) FROM main.journal_meta)=(SELECT count(*) FROM temp.custody_upgrade_v5_meta)",
        [],
        |r| r.get(0),
    )?;
    if !unchanged {
        return Err(codec::invalid());
    }
    let after = transaction::snapshot(&tx, witness)?;
    if before.total != after.total
        || before.trial_groups != after.trial_groups
        || before.obligations.len() != after.obligations.len()
        || !before
            .family_authority
            .same_complete_rows(&after.family_authority)
        || before.retained_assets.len() != after.retained_assets.len()
        || before.recovered_s_holders != after.recovered_s_holders
        || before.recovered_r_holders != after.recovered_r_holders
        || before.protected_paths != after.protected_paths
    {
        return Err(codec::invalid());
    }
    tx.execute_batch("DROP TABLE temp.custody_upgrade_v5_meta;")?;
    tx.commit().map_err(|_| {
        let mut error = CustodyError::new(
            ErrorKind::Storage,
            "custody schema upgrade commit uncertain",
        );
        error.certainty = CommitCertainty::Unknown;
        error
    })
}

/// Add the covering obligation/role lookup without changing any historical
/// custody row. Full row comparisons guard even settled holder history.
pub(super) fn upgrade_v5_to_v6(
    connection: &mut Connection,
    witness: &Witness,
) -> Result<(), CustodyError> {
    validate(connection)?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let foreign_keys: bool = connection.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    if version != 5 || !foreign_keys {
        return Err(codec::invalid());
    }
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    validate(&tx)?;
    let tables = TABLES.into_iter().chain([
        "retained_asset_owners",
        "recovered_s_holders",
        "recovered_r_holders",
    ]);
    for table in tables.clone() {
        tx.execute_batch(&format!(
            "CREATE TEMP TABLE custody_upgrade_v6_{table} AS SELECT * FROM main.{table};"
        ))?;
    }
    let canonical = Connection::open_in_memory()?;
    canonical.execute_batch(DDL_V6)?;
    let index_sql: String = canonical.query_row(
        "SELECT sql FROM sqlite_schema WHERE name='family_roles_by_obligation_role'",
        [],
        |row| row.get(0),
    )?;
    // The index changes no table row and is rolled back with this transaction.
    // The complete history is validated after the exact projection is built.
    tx.execute_batch(&index_sql)?;
    for name in [
        "asset_operation_membership",
        "asset_operation_by_asset_sequence",
    ] {
        let sql: String = canonical.query_row(
            "SELECT sql FROM sqlite_schema WHERE name=?1",
            [name],
            |row| row.get(0),
        )?;
        tx.execute_batch(&sql)?;
    }
    transaction::backfill_asset_membership(&tx)?;
    let meta_sql: String = canonical.query_row(
        "SELECT sql FROM sqlite_schema WHERE name='journal_meta'",
        [],
        |row| row.get(0),
    )?;
    tx.execute_batch("DROP TABLE journal_meta;")?;
    tx.execute_batch(&meta_sql)?;
    tx.execute_batch(
        "INSERT INTO journal_meta SELECT singleton,6,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id FROM temp.custody_upgrade_v6_journal_meta;
         PRAGMA user_version=6;",
    )?;
    validate(&tx)?;
    for table in tables.clone() {
        let projection = if table == "journal_meta" {
            "singleton,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id"
        } else {
            "*"
        };
        let unchanged: bool = tx.query_row(
            &format!(
                "SELECT NOT EXISTS(SELECT {projection} FROM main.{table} EXCEPT SELECT {projection} FROM temp.custody_upgrade_v6_{table}) AND NOT EXISTS(SELECT {projection} FROM temp.custody_upgrade_v6_{table} EXCEPT SELECT {projection} FROM main.{table}) AND (SELECT count(*) FROM main.{table})=(SELECT count(*) FROM temp.custody_upgrade_v6_{table})"
            ),
            [],
            |row| row.get(0),
        )?;
        if !unchanged {
            return Err(codec::invalid());
        }
    }
    // Every old table still has byte-exact rows. The global projection pass
    // proves that asset+sequence index order is the same subsequence of the
    // immutable result_json rowid stream that legacy folds replayed. Thus this
    // one complete v6 snapshot exercises the original fold checks on every
    // historical asset without the old per-obligation full-history rescan.
    transaction::snapshot(&tx, witness)?;
    for table in tables {
        tx.execute_batch(&format!("DROP TABLE temp.custody_upgrade_v6_{table};"))?;
    }
    tx.commit().map_err(|_| {
        let mut error = CustodyError::new(
            ErrorKind::Storage,
            "custody schema upgrade commit uncertain",
        );
        error.certainty = CommitCertainty::Unknown;
        error
    })
}

/// Admit only the exact v6 authority. Widen two SQL CHECK domains to the
/// already-implemented typed adapters and paired planning-M claim. Original
/// identities, bytes, memory vectors, phase/history and native proof do not
/// change. Owners, immutable operation rowids and asset projection stay in place.
pub(super) fn upgrade_v6_to_v7(
    connection: &mut Connection,
    witness: &Witness,
) -> Result<(), CustodyError> {
    validate(connection)?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let foreign_keys: bool = connection.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    if version != 6 || !foreign_keys {
        return Err(codec::invalid());
    }
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Both recognized shape and complete semantic history are asserted again
    // under the actual writer transaction, before any persistent DDL changes.
    validate(&tx)?;
    let before = transaction::snapshot(&tx, witness)?;
    const ALL: &[&str] = &[
        "journal_meta",
        "owners",
        "obligations",
        "family_roles",
        "members",
        "operations",
        "retained_asset_owners",
        "recovered_s_holders",
        "recovered_r_holders",
        "asset_operation_membership",
    ];
    for table in ALL {
        tx.execute_batch(&format!(
            "CREATE TEMP TABLE custody_upgrade_v7_{table} AS SELECT * FROM main.{table};"
        ))?;
    }
    // Rebuild all inbound-FK children before obligations, keeping FK checking
    // enabled throughout. Unknown triggers/objects were rejected by validate.
    tx.execute_batch("DROP TABLE members; DROP TABLE family_roles; DROP TABLE retained_asset_owners; DROP TABLE recovered_s_holders; DROP TABLE recovered_r_holders; DROP TABLE obligations; DROP TABLE journal_meta;")?;
    let canonical = Connection::open_in_memory()?;
    canonical.execute_batch(DDL)?;
    for name in [
        "journal_meta",
        "obligations",
        "family_roles",
        "family_roles_by_obligation_role",
        "members",
        "unique_member_identity",
        "retained_asset_owners",
        "recovered_s_holders",
        "recovered_r_holders",
    ] {
        let sql: String =
            canonical.query_row("SELECT sql FROM sqlite_schema WHERE name=?1", [name], |r| {
                r.get(0)
            })?;
        tx.execute_batch(&sql)?;
    }
    #[cfg(test)]
    if V7_DDL_FAILURE.with(|flag| flag.replace(false)) {
        tx.execute_batch("SELECT custody_v7_missing_rollback_function();")?;
    }
    tx.execute_batch("INSERT INTO journal_meta SELECT singleton,7,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id FROM temp.custody_upgrade_v7_journal_meta;")?;
    for table in [
        "obligations",
        "family_roles",
        "members",
        "retained_asset_owners",
        "recovered_s_holders",
        "recovered_r_holders",
    ] {
        tx.execute_batch(&format!(
            "INSERT INTO main.{table} SELECT * FROM temp.custody_upgrade_v7_{table};"
        ))?;
    }
    tx.execute_batch("PRAGMA user_version=7;")?;
    validate(&tx)?;
    for table in ALL {
        let projection = if *table == "journal_meta" {
            "singleton,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id"
        } else {
            "*"
        };
        let unchanged: bool = tx.query_row(&format!(
            "SELECT NOT EXISTS(SELECT {projection} FROM main.{table} EXCEPT SELECT {projection} FROM temp.custody_upgrade_v7_{table}) AND NOT EXISTS(SELECT {projection} FROM temp.custody_upgrade_v7_{table} EXCEPT SELECT {projection} FROM main.{table}) AND (SELECT count(*) FROM main.{table})=(SELECT count(*) FROM temp.custody_upgrade_v7_{table})"
        ), [], |r| r.get(0))?;
        if !unchanged {
            return Err(codec::invalid());
        }
    }
    let no_broken_fk: bool = tx.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM pragma_foreign_key_check)",
        [],
        |r| r.get(0),
    )?;
    if !no_broken_fk {
        return Err(codec::invalid());
    }
    let after = transaction::snapshot(&tx, witness)?;
    if before.total != after.total
        || before.trial_groups != after.trial_groups
        || before.obligations.len() != after.obligations.len()
        || !before
            .family_authority
            .same_complete_rows(&after.family_authority)
        || before.protected_paths != after.protected_paths
    {
        return Err(codec::invalid());
    }
    for table in ALL {
        tx.execute_batch(&format!("DROP TABLE temp.custody_upgrade_v7_{table};"))?;
    }
    tx.commit().map_err(|_| {
        let mut error = CustodyError::new(
            ErrorKind::Storage,
            "custody schema upgrade commit uncertain",
        );
        error.certainty = CommitCertainty::Unknown;
        error
    })
}

#[cfg(test)]
thread_local! {
    static V7_DDL_FAILURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(in crate::resource_custody) struct V7FailureGuard;
#[cfg(test)]
impl V7FailureGuard {
    pub(in crate::resource_custody) fn was_reached(&self) -> bool {
        V7_DDL_FAILURE.with(|flag| !flag.get())
    }
}
#[cfg(test)]
impl Drop for V7FailureGuard {
    fn drop(&mut self) {
        V7_DDL_FAILURE.with(|flag| flag.set(false));
    }
}
#[cfg(test)]
pub(in crate::resource_custody) fn fail_next_v7_after_ddl_for_test() -> V7FailureGuard {
    V7_DDL_FAILURE.with(|flag| assert!(!flag.replace(true)));
    V7FailureGuard
}
