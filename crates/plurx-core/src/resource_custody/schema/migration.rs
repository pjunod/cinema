//! Same-file v1 -> v2 upgrade. This is not missing-authority repair.
use super::{validate, DDL};
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
    if version == 2 {
        return Ok(());
    }
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
    canonical.execute_batch(DDL)?;
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
