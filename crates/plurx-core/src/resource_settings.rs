//! Stored resource-setting provenance, separate from resolved node defaults.
//! This reader does not grant capacity or write/initialize settings. Numeric
//! and provenance records must be bound exactly; an old numeric writer cannot
//! inherit a stale Automatic decision after changing the stored value.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{error::StoreError, store::keys};

#[derive(Clone, Debug)]
pub struct CapacitySnapshot {
    pub revision: i64,
    pub values: BTreeMap<String, String>,
}

/// Explicit operator intent only. Omitted fields are not rewritten. Automatic
/// preserves the stored conservative rollback cache, never a resolved local
/// core count; selecting a mode is not a capacity permit.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum LimitChoice {
    Automatic,
    Explicit { value: u64 },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapacityUpdate {
    pub expected_revision: i64,
    pub hardware_sessions: Option<LimitChoice>,
    pub software_pool_threads: Option<LimitChoice>,
}

impl CapacityUpdate {
    pub fn validate(&self, snapshot: &CapacitySnapshot) -> Result<(), StoreError> {
        self.writes(snapshot).map(|_| ())
    }

    pub(crate) fn writes(
        &self,
        snapshot: &CapacitySnapshot,
    ) -> Result<Vec<(String, String)>, StoreError> {
        if self.expected_revision < 0
            || (self.hardware_sessions.is_none() && self.software_pool_threads.is_none())
        {
            return Err(StoreError::Task(
                "capacity update requires a nonnegative revision and explicit dirty choice".into(),
            ));
        }
        let mut numeric = Vec::new();
        let mut provenance = Vec::new();
        for (key, provenance_key, choice) in [
            (
                keys::MAX_HW_SESSIONS,
                keys::MAX_HW_SESSIONS_PROVENANCE,
                self.hardware_sessions,
            ),
            (
                keys::SW_POOL_THREADS,
                keys::SW_POOL_THREADS_PROVENANCE,
                self.software_pool_threads,
            ),
        ] {
            let Some(choice) = choice else {
                continue;
            };
            let (mode, bound_value) = match choice {
                LimitChoice::Automatic => {
                    let bound = snapshot.values.get(key).cloned();
                    if bound.as_deref().is_some_and(|value| {
                        value.len() > 32
                            || !value.trim().parse::<i64>().is_ok_and(|value| value >= 0)
                    }) {
                        return Err(StoreError::Task(
                            "invalid rollback numeric cache; save a valid explicit limit first"
                                .into(),
                        ));
                    }
                    ("automatic", bound)
                }
                LimitChoice::Explicit { value } => {
                    if i64::try_from(value).is_err() {
                        return Err(StoreError::Task(
                            "capacity limit exceeds signed storage range".into(),
                        ));
                    }
                    let value = value.to_string();
                    numeric.push((key.to_owned(), value.clone()));
                    ("explicit", Some(value))
                }
            };
            provenance.push((
                provenance_key.to_owned(),
                serde_json::json!({"schema":1,"mode":mode,"bound_value":bound_value}).to_string(),
            ));
        }
        // Every numeric mutation invalidates old provenance in the database.
        // Typed provenance is installed LAST in the same guarded transaction.
        numeric.extend(provenance);
        Ok(numeric)
    }
}

/// Equal fields from a capability-unaware GET/PATCH echo are not new operator
/// intent. Actual changed numbers remain legacy writes, invalidated by SQL.
pub fn legacy_capacity_patch(
    values: &BTreeMap<String, String>,
    hardware: Option<i64>,
    software: Option<i64>,
    hardware_default: i64,
    software_default: i64,
) -> Vec<(&'static str, String)> {
    [
        (keys::MAX_HW_SESSIONS, hardware, hardware_default),
        (keys::SW_POOL_THREADS, software, software_default),
    ]
    .into_iter()
    .filter_map(|(key, requested, default)| {
        let requested = requested?;
        let current = values
            .get(key)
            .and_then(|value| value.trim().parse::<i64>().ok())
            .filter(|value| *value >= 0)
            .unwrap_or(default);
        (requested != current).then(|| (key, requested.to_string()))
    })
    .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadState {
    Absent,
    LegacyAmbiguous,
    Explicit,
    Automatic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ZeroMeaning {
    HardwareDisabled,
    SerializedSoftware,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StoredLimit {
    pub state: ReadState,
    pub stored_present: bool,
    pub stored_value: Option<u64>,
    pub provenance_valid: bool,
    pub zero_means: ZeroMeaning,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResourceSettings {
    pub hardware_sessions: StoredLimit,
    pub software_pool_threads: StoredLimit,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SavedMode {
    LegacyAmbiguous,
    Explicit,
    Automatic,
}

/// A versioned record binds the ORIGINAL numeric bytes, not a resolved local
/// default. Typed writer transactions must write this after the numeric key;
/// persistent invalidation triggers still remain necessary for same-value old
/// writes and unversioned imports. This parser alone is not that migration.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedProvenance {
    schema: u8,
    mode: SavedMode,
    bound_value: Option<String>,
}

impl ResourceSettings {
    pub fn from_snapshot(values: &BTreeMap<String, String>) -> Self {
        let get = |key: &str| values.get(key).map(String::as_str);
        Self {
            hardware_sessions: read(
                get(keys::MAX_HW_SESSIONS),
                get(keys::MAX_HW_SESSIONS_PROVENANCE),
                ZeroMeaning::HardwareDisabled,
            ),
            software_pool_threads: read(
                get(keys::SW_POOL_THREADS),
                get(keys::SW_POOL_THREADS_PROVENANCE),
                ZeroMeaning::SerializedSoftware,
            ),
        }
    }
}

fn read(numeric: Option<&str>, provenance: Option<&str>, zero_means: ZeroMeaning) -> StoredLimit {
    let stored_value = numeric
        .filter(|value| value.len() <= 32)
        .and_then(|value| value.trim().parse::<i64>().ok())
        .and_then(|value| u64::try_from(value).ok());
    let parsed = provenance
        .filter(|value| value.len() <= 4096)
        .and_then(|value| serde_json::from_str::<SavedProvenance>(value).ok())
        .filter(|record| record.schema == 1 && record.bound_value.as_deref() == numeric);
    let numeric_valid = numeric.is_none() || stored_value.is_some();
    let (state, provenance_valid) = match (numeric, provenance, parsed) {
        (None, None, _) => (ReadState::Absent, true),
        (Some(_), None, _) => (ReadState::LegacyAmbiguous, false),
        (_, _, Some(record)) if numeric_valid => match record.mode {
            SavedMode::Automatic => (ReadState::Automatic, true),
            SavedMode::Explicit if numeric.is_some() => (ReadState::Explicit, true),
            SavedMode::LegacyAmbiguous if numeric.is_some() => (ReadState::LegacyAmbiguous, true),
            _ => (ReadState::LegacyAmbiguous, false),
        },
        _ => (ReadState::LegacyAmbiguous, false),
    };
    StoredLimit {
        state,
        stored_present: numeric.is_some(),
        stored_value,
        provenance_valid,
        zero_means,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untouched_roundtrip_preserves_absence_and_node_local_defaults() {
        let mut values = BTreeMap::new();
        assert!(legacy_capacity_patch(&values, Some(2), Some(15), 2, 15).is_empty());
        assert!(legacy_capacity_patch(&values, Some(2), Some(3), 2, 3).is_empty());
        values.insert(keys::MAX_HW_SESSIONS.into(), "0".into());
        values.insert(
            keys::MAX_HW_SESSIONS_PROVENANCE.into(),
            record("automatic", Some("0")),
        );
        assert!(legacy_capacity_patch(&values, Some(0), None, 2, 15).is_empty());
        assert_eq!(
            legacy_capacity_patch(&values, Some(1), None, 2, 15),
            vec![(keys::MAX_HW_SESSIONS, "1".into())]
        );
    }

    fn record(mode: &str, bound_value: Option<&str>) -> String {
        serde_json::json!({"schema":1,"mode":mode,"bound_value":bound_value}).to_string()
    }

    #[test]
    fn four_states_distinguish_storage_from_node_defaults_without_writing() {
        let mut settings = BTreeMap::new();
        let absent = ResourceSettings::from_snapshot(&settings);
        assert_eq!(absent.hardware_sessions.state, ReadState::Absent);
        assert_eq!(absent.software_pool_threads.stored_value, None);
        assert!(settings.is_empty());
        settings.insert(keys::MAX_HW_SESSIONS.into(), "2".into());
        assert_eq!(
            ResourceSettings::from_snapshot(&settings)
                .hardware_sessions
                .state,
            ReadState::LegacyAmbiguous
        );
        settings.insert(
            keys::MAX_HW_SESSIONS_PROVENANCE.into(),
            record("explicit", Some("2")),
        );
        assert_eq!(
            ResourceSettings::from_snapshot(&settings)
                .hardware_sessions
                .state,
            ReadState::Explicit
        );
        settings.insert(
            keys::MAX_HW_SESSIONS_PROVENANCE.into(),
            record("automatic", Some("2")),
        );
        assert_eq!(
            ResourceSettings::from_snapshot(&settings)
                .hardware_sessions
                .state,
            ReadState::Automatic
        );
        assert!(!settings.contains_key(keys::SW_POOL_THREADS));
    }

    #[test]
    fn changed_legacy_number_cannot_inherit_automatic_or_forged_provenance() {
        let automatic = record("automatic", Some("2"));
        let changed = read(Some("1"), Some(&automatic), ZeroMeaning::HardwareDisabled);
        assert_eq!(changed.state, ReadState::LegacyAmbiguous);
        assert_eq!(changed.stored_value, Some(1));
        assert!(!changed.provenance_valid);
        for malformed in [
            "{}",
            "not json",
            r#"{"schema":2,"mode":"automatic","bound_value":"1"}"#,
        ] {
            let observed = read(Some("1"), Some(malformed), ZeroMeaning::HardwareDisabled);
            assert_eq!(observed.state, ReadState::LegacyAmbiguous);
            assert!(!observed.provenance_valid);
        }
        assert!(!read(Some("-1"), None, ZeroMeaning::HardwareDisabled).provenance_valid);
    }

    #[test]
    fn zeros_keep_distinct_hardware_and_software_meanings() {
        let hardware = read(Some("0"), None, ZeroMeaning::HardwareDisabled);
        let software = read(Some("0"), None, ZeroMeaning::SerializedSoftware);
        assert_eq!(hardware.stored_value, Some(0));
        assert_eq!(software.stored_value, Some(0));
        assert_eq!(hardware.zero_means, ZeroMeaning::HardwareDisabled);
        assert_eq!(software.zero_means, ZeroMeaning::SerializedSoftware);
        assert_eq!(
            read(
                None,
                Some(&record("automatic", None)),
                ZeroMeaning::SerializedSoftware
            )
            .state,
            ReadState::Automatic
        );
        assert!(
            !read(
                None,
                Some(&record("explicit", None)),
                ZeroMeaning::SerializedSoftware
            )
            .provenance_valid
        );
    }
}
