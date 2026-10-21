//! Capacity authority carried by the SAME successful planning statement as the
//! source and raw settings. Catalog presence or parser-valid intent alone is not
//! proof of an installed settings protocol. No cache survives a failed read.
use std::collections::BTreeMap;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::StoreError;
use crate::resource_settings::{ReadState, ResourceSettings, StoredLimit};

use super::{keys, resource_settings_schema::OBJECTS};

#[cfg(test)]
mod tests;

pub(super) const CAPACITY_KEYS: [&str; 4] = [
    keys::MAX_HW_SESSIONS,
    keys::SW_POOL_THREADS,
    keys::MAX_HW_SESSIONS_PROVENANCE,
    keys::SW_POOL_THREADS_PROVENANCE,
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CapacityInstallation {
    Ready {
        revision: i64,
    },
    Absent,
    Unrecognized,
    InvalidMarker,
    /// The successful fallback cannot inspect the optional marker, even when
    /// its catalog happens to match after a concurrent installation/restore.
    UnverifiedMarker,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlanningCapacity {
    pub installation: CapacityInstallation,
    pub saved_modes: ResourceSettings,
    pub verified_modes: ResourceSettings,
    /// Versioned admission identity, not recipe-content identity. Includes raw
    /// presence/bytes, shape and marker, so equal generation after restore does
    /// not alias another resource authority.
    pub binding: [u8; 32],
}

impl PlanningCapacity {
    /// Conservative construction for synthetic/unverified snapshots. This
    /// cannot turn a companion record into installation authority.
    pub fn unverified(values: &BTreeMap<String, String>) -> Self {
        Self::from_statement(values, "[]", None)
    }

    pub(super) fn from_statement(
        values: &BTreeMap<String, String>,
        catalog: &str,
        marker: Option<&str>,
    ) -> Self {
        let shape = serde_json::from_str::<Vec<(String, String, String, Option<String>)>>(catalog);
        let shape_matches = shape.as_ref().is_ok_and(|rows| {
            rows.len() == OBJECTS.len()
                && OBJECTS.iter().all(|(name, sql)| {
                    rows.iter().any(|(n, kind, table, body)| {
                        n == name
                            && kind
                                == if *name == "resource_settings_schema" {
                                    "table"
                                } else {
                                    "trigger"
                                }
                            && table
                                == if *name == "resource_settings_schema" {
                                    "resource_settings_schema"
                                } else {
                                    "settings"
                                }
                            && body
                                .as_deref()
                                .is_some_and(|body| body.trim_end_matches(';') == *sql)
                    })
                })
        });
        // JSON emitted by fixed SQL preserves integer types and row cardinality;
        // invalid storage classes become null, never a lossy integer cast.
        let revision = marker
            .and_then(|raw| serde_json::from_str::<Vec<(i64, i64, i64, bool)>>(raw).ok())
            .filter(|rows| rows.len() == 1)
            .and_then(|rows| {
                let (singleton, version, revision, unclaimed) = rows[0];
                (singleton == 1 && version == 1 && revision >= 0 && unclaimed).then_some(revision)
            });
        let installation = if shape.as_ref().is_ok_and(Vec::is_empty) {
            CapacityInstallation::Absent
        } else if !shape_matches {
            CapacityInstallation::Unrecognized
        } else if let Some(revision) = revision {
            CapacityInstallation::Ready { revision }
        } else if marker.is_some() {
            CapacityInstallation::InvalidMarker
        } else {
            CapacityInstallation::UnverifiedMarker
        };
        let saved_modes = ResourceSettings::from_snapshot(values);
        let verified_modes = if matches!(installation, CapacityInstallation::Ready { .. }) {
            saved_modes.clone()
        } else {
            let numeric_only = CAPACITY_KEYS[..2]
                .iter()
                .filter_map(|key| {
                    values
                        .get(*key)
                        .map(|value| ((*key).to_owned(), value.clone()))
                })
                .collect();
            ResourceSettings::from_snapshot(&numeric_only)
        };
        let raw: Vec<_> = CAPACITY_KEYS
            .iter()
            .map(|key| (*key, values.get(*key)))
            .collect();
        let encoded = serde_json::to_vec(&(
            "plurx:planning-capacity:v1",
            raw,
            &installation,
            catalog,
            marker,
        ))
        .expect("capacity read identity serializes");
        Self {
            installation,
            saved_modes,
            verified_modes,
            binding: Sha256::digest(encoded).into(),
        }
    }

    /// Uses ONLY verified Automatic to ignore the rollback cache. Unknown
    /// installation retains legacy numeric/zero semantics; this is not a grant
    /// of learned expansion, even when the stored mode is Automatic.
    pub fn admission_value<'a>(
        &self,
        values: &'a BTreeMap<String, String>,
        key: &str,
    ) -> Option<&'a str> {
        let limit: Option<&StoredLimit> = match key {
            keys::MAX_HW_SESSIONS => Some(&self.verified_modes.hardware_sessions),
            keys::SW_POOL_THREADS => Some(&self.verified_modes.software_pool_threads),
            _ => None,
        };
        if limit.is_some_and(|limit| limit.state == ReadState::Automatic) {
            None
        } else {
            values.get(key).map(String::as_str)
        }
    }
}

pub(super) fn selected_keys(keys: &[&str]) -> Result<String, StoreError> {
    let mut all = keys.to_vec();
    all.extend(CAPACITY_KEYS);
    all.sort_unstable();
    all.dedup();
    super::selected_settings_json(&all)
}

/// Exactly two full statement templates. The fallback mentions NO optional
/// table, not even in a CASE subquery (SQLite resolves all names at prepare).
/// FILE_COLS comes from each backend's fixed source projection, never a caller.
pub(super) fn statement(file_cols: &str, fallback: bool) -> String {
    let evidence = evidence_projection(fallback);
    format!(
        "WITH input AS (SELECT $1 AS file_id,$2 AS keys) SELECT {file_cols}, probe_json AS planning_probe, \
         (SELECT generation FROM playback_input_generation WHERE singleton=1) AS planning_generation, \
         (SELECT json_group_object(key,value) FROM settings WHERE key IN (SELECT value FROM json_each((SELECT keys FROM input)))) AS planning_settings, \
         (SELECT title FROM items WHERE items.id=files.item_id) AS planning_item_title, \
         {evidence} FROM files WHERE id=(SELECT file_id FROM input)"
    )
}

/// Settings-only readers/importers need the same atomic proof, not a catalog
/// preflight whose result can race a later settings/marker read.
pub(super) fn settings_statement() -> String {
    let evidence = evidence_projection(false);
    format!("SELECT (SELECT json_group_object(key,value) FROM settings WHERE key IN \
        ('transcode.max_hw_sessions','transcode.software_pool_threads','transcode.max_hw_sessions.provenance','transcode.software_pool_threads.provenance')) AS planning_settings, {evidence}")
}

pub(super) fn verified_snapshot(
    encoded: &str,
    catalog: &str,
    marker: Option<&str>,
) -> Result<Option<crate::resource_settings::CapacitySnapshot>, StoreError> {
    let values =
        serde_json::from_str(encoded).map_err(|error| StoreError::Database(error.to_string()))?;
    let capacity = PlanningCapacity::from_statement(&values, catalog, marker);
    Ok(match capacity.installation {
        CapacityInstallation::Ready { revision } => {
            Some(crate::resource_settings::CapacitySnapshot { revision, values })
        }
        _ => None,
    })
}

fn evidence_projection(fallback: bool) -> String {
    let catalog_limit = OBJECTS.len() + 1;
    let marker = if fallback {
        "NULL".to_owned()
    } else {
        let recognized = OBJECTS.iter().map(|(name, sql)| {
            let (kind, table) = if *name == "resource_settings_schema" {
                ("table", "resource_settings_schema")
            } else { ("trigger", "settings") };
            format!("(name='{name}' AND type='{kind}' AND tbl_name='{table}' AND rtrim(sql,';')='{}')", sql.replace('\'', "''"))
        }).collect::<Vec<_>>().join(" OR ");
        let shape = format!("(SELECT count(*) FROM sqlite_master WHERE name GLOB 'resource_settings_*')={} AND (SELECT count(*) FROM sqlite_master WHERE {recognized})={}", OBJECTS.len(), OBJECTS.len());
        format!("CASE WHEN {shape} THEN (SELECT json_group_array(json_array(\
            CASE WHEN typeof(capacity_marker.singleton)='integer' THEN capacity_marker.singleton END,\
            CASE WHEN typeof(capacity_marker.version)='integer' THEN capacity_marker.version END,\
            CASE WHEN typeof(capacity_marker.revision)='integer' THEN capacity_marker.revision END,\
            json(CASE WHEN capacity_marker.write_claim IS NULL THEN 'true' ELSE 'false' END))) \
          FROM (SELECT resource_marker.singleton,resource_marker.version,resource_marker.revision,resource_marker.write_claim FROM main.resource_settings_schema AS resource_marker LIMIT 2) AS capacity_marker) ELSE NULL END")
    };
    format!(
        "(SELECT json_group_array(json_array(\
            CASE WHEN length(name)<=128 THEN name END,type,\
            CASE WHEN length(tbl_name)<=128 THEN tbl_name END,\
            CASE WHEN length(sql)<=4096 THEN sql END)) FROM \
          (SELECT name,type,tbl_name,sql FROM sqlite_master WHERE name GLOB 'resource_settings_*' ORDER BY name,type,tbl_name,sql LIMIT {catalog_limit})) AS planning_capacity_catalog, \
         {marker} AS planning_capacity_marker"
    )
}

/// Restrict recovery to optional-marker name-resolution failures. In
/// particular busy/corruption/I/O/raft failures never silently become legacy.
const OPTIONAL_NAMES: [&str; 6] = [
    "no such table: resource_settings_schema",
    "no such table: main.resource_settings_schema",
    "no such column: resource_marker.singleton",
    "no such column: resource_marker.version",
    "no such column: resource_marker.revision",
    "no such column: resource_marker.write_claim",
];

pub(super) fn sqlite_missing_optional(error: &rusqlite::Error) -> bool {
    match error {
        rusqlite::Error::SqliteFailure(code, Some(message))
            if code.extended_code == rusqlite::ffi::SQLITE_ERROR =>
        {
            OPTIONAL_NAMES.contains(&message.as_str())
        }
        rusqlite::Error::SqlInputError { error, msg, .. }
            if error.extended_code == rusqlite::ffi::SQLITE_ERROR =>
        {
            OPTIONAL_NAMES.contains(&msg.as_str())
        }
        _ => false,
    }
}

#[cfg(feature = "hiqlite-store")]
pub(super) fn hiqlite_missing_optional(error: &hiqlite::Error, submitted_sql: &str) -> bool {
    let hiqlite::Error::Sqlite(payload) = error else {
        return false;
    };
    OPTIONAL_NAMES.iter().any(|message| {
        // Pinned vendor error.rs serializes SqliteFailure as Display + Debug
        // optional message. Compare the WHOLE payload, not the SQL it contains.
        payload.as_ref()
            == format!(
                "{} {:?}",
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                Some(message)
            )
            || payload
                .strip_prefix(&format!("{message} in {submitted_sql} at offset "))
                .and_then(|offset| offset.parse::<usize>().ok())
                .is_some_and(|offset| offset < submitted_sql.len())
    })
}
