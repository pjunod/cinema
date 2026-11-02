//! Exact versioned schemas. Recognized v1 upgrades in place, never by repair.
mod migration;
#[cfg(test)]
mod tests;
use super::{CustodyError, ErrorKind};
#[cfg(test)]
pub(super) use migration::fail_next_v7_after_ddl_for_test;
pub(super) use migration::upgrade;
use rusqlite::Connection;

// Frozen: old documents and retry history retain the original interpretation.
pub(super) const DDL_V1: &str = r#"
CREATE TABLE journal_meta (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 format_version INTEGER NOT NULL CHECK(format_version=1),
 journal_id BLOB NOT NULL CHECK(length(journal_id)=16),
 installation_id BLOB NOT NULL CHECK(length(installation_id)=16),
 node_id TEXT NOT NULL CHECK(length(CAST(node_id AS BLOB)) BETWEEN 1 AND 256),
 cluster_id TEXT NOT NULL CHECK(length(CAST(cluster_id AS BLOB)) BETWEEN 1 AND 256),
 next_sequence INTEGER NOT NULL CHECK(next_sequence>=0),
 current_owner_id BLOB REFERENCES owners(owner_id) CHECK(current_owner_id IS NULL OR length(current_owner_id)=16)
) STRICT;
CREATE TABLE owners (
 owner_id BLOB PRIMARY KEY NOT NULL CHECK(length(owner_id)=16),
 node_id TEXT NOT NULL, cluster_id TEXT NOT NULL,
 os_boot_json TEXT NOT NULL CHECK(length(CAST(os_boot_json AS BLOB))<=65536),
 daemon_process_json TEXT NOT NULL CHECK(length(CAST(daemon_process_json AS BLOB))<=65536),
 platform_domain_json TEXT NOT NULL CHECK(length(CAST(platform_domain_json AS BLOB))<=65536),
 state TEXT NOT NULL CHECK(state IN ('current','retired'))
) STRICT;
CREATE UNIQUE INDEX one_current_owner ON owners(state) WHERE state='current';
CREATE TABLE obligations (
 obligation_id BLOB PRIMARY KEY NOT NULL CHECK(length(obligation_id)=16),
 owner_id BLOB NOT NULL REFERENCES owners(owner_id),
 graph_id BLOB UNIQUE NOT NULL CHECK(length(graph_id)=16),
 allocation_epoch BLOB NOT NULL CHECK(length(allocation_epoch)=8),
 resources BLOB NOT NULL CHECK(length(resources)=56),
 trial_group_id BLOB CHECK(trial_group_id IS NULL OR length(trial_group_id)=16),
 phase TEXT NOT NULL CHECK(phase IN ('prepared','acquisition_possible','sealed','unknown_held','settled')),
 sequence INTEGER NOT NULL CHECK(sequence>=0),
 last_operation_id BLOB NOT NULL CHECK(length(last_operation_id)=16),
 manifest_json TEXT NOT NULL CHECK(length(CAST(manifest_json AS BLOB))<=65536),
 hold_reason TEXT,
 settlement_json TEXT CHECK(settlement_json IS NULL OR length(CAST(settlement_json AS BLOB))<=65536),
 CHECK((phase='settled')=(settlement_json IS NOT NULL)),
 CHECK((phase='unknown_held')=(hold_reason IS NOT NULL))
) STRICT;
CREATE TABLE family_roles (
 family_id BLOB NOT NULL CHECK(length(family_id)=16),
 role TEXT NOT NULL CHECK(role IN ('video','audio','companion')),
 obligation_id BLOB NOT NULL REFERENCES obligations(obligation_id),
 PRIMARY KEY(family_id,role)
) STRICT;
CREATE TABLE members (
 obligation_id BLOB NOT NULL REFERENCES obligations(obligation_id),
 member_id BLOB NOT NULL CHECK(length(member_id)=16),
 kind TEXT NOT NULL CHECK(kind IN ('containment','root','task','typed_resource')),
 adapter TEXT NOT NULL CHECK(adapter IN ('linux_cgroup_v2','windows_job','process_root','root_pipe_task','scratch','tuner_session','remote_attempt')),
 adapter_version INTEGER NOT NULL CHECK(adapter_version=1),
 phase TEXT NOT NULL CHECK(phase IN ('planned','acquisition_possible','acquired','closed')),
 identity_json TEXT NOT NULL CHECK(length(CAST(identity_json AS BLOB))<=65536),
 proof_json TEXT CHECK(proof_json IS NULL OR length(CAST(proof_json AS BLOB))<=65536),
 PRIMARY KEY(obligation_id,member_id),
 CHECK((phase='closed')=(proof_json IS NOT NULL))
) STRICT;
CREATE UNIQUE INDEX unique_member_identity ON members(member_id);
CREATE TABLE operations (
 operation_id BLOB PRIMARY KEY NOT NULL CHECK(length(operation_id)=16),
 request_json TEXT NOT NULL CHECK(length(CAST(request_json AS BLOB))<=65536),
 result_json TEXT NOT NULL CHECK(length(CAST(result_json AS BLOB))<=65536)
) STRICT;
PRAGMA user_version=1;
"#;

pub(super) const DDL_V2: &str = include_str!("schema/v2.sql");
pub(super) const DDL_V3: &str = include_str!("schema/v3.sql");
pub(super) const DDL_V4: &str = include_str!("schema/v4.sql");
pub(super) const DDL_V5: &str = include_str!("schema/v5.sql");
pub(super) const DDL_V6: &str = include_str!("schema/v6.sql");
pub(super) const DDL: &str = include_str!("schema/v7.sql");

fn shape(connection: &Connection) -> Result<Vec<(String, String, String)>, CustodyError> {
    let mut statement = connection.prepare(
        "SELECT type,name,sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name",
    )?;
    let rows = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    rows.collect::<Result<_, _>>().map_err(Into::into)
}

pub(super) fn validate(connection: &Connection) -> Result<(), CustodyError> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let ddl = match version {
        1 => DDL_V1,
        2 => DDL_V2,
        3 => DDL_V3,
        4 => DDL_V4,
        5 => DDL_V5,
        6 => DDL_V6,
        7 => DDL,
        _ => {
            return Err(CustodyError::new(
                ErrorKind::UnreadableAuthority,
                "unrecognized custody schema version",
            ))
        }
    };
    // Compare SQLite's own canonical catalog output against the exact known DDL,
    // including indexes/constraints/triggers; numeric version alone is insufficient.
    let expected = Connection::open_in_memory()?;
    expected.execute_batch(ddl)?;
    if shape(connection)? != shape(&expected)? {
        return Err(CustodyError::new(
            ErrorKind::UnreadableAuthority,
            "unrecognized custody schema shape",
        ));
    }
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if integrity != "ok" {
        return Err(CustodyError::new(
            ErrorKind::UnreadableAuthority,
            "custody integrity check failed",
        ));
    }
    let mut check = connection.prepare("PRAGMA foreign_key_check")?;
    if check.query([])?.next()?.is_some() {
        return Err(CustodyError::new(
            ErrorKind::UnreadableAuthority,
            "custody foreign key check failed",
        ));
    }
    Ok(())
}
