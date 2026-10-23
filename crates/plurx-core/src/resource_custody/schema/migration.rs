//! Same-file recognized v1 -> v2 -> v3 -> v4 upgrades; no missing-authority repair.
use super::{validate, DDL, DDL_V2, DDL_V3};
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
        || before.families.len() != after.families.len()
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
fn upgrade_v2_to_v3(connection: &mut Connection, witness: &Witness) -> Result<(), CustodyError> {
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
        || before.families.len() != after.families.len()
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
fn upgrade_v3_to_v4(connection: &mut Connection, witness: &Witness) -> Result<(), CustodyError> {
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
    canonical.execute_batch(DDL)?;
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
        || before.families.len() != after.families.len()
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
