//! Stored resource-setting provenance, separate from resolved node defaults.
//! This reader does not grant capacity or write/initialize settings. Numeric
//! and provenance records must be bound exactly; an old numeric writer cannot
//! inherit a stale Automatic decision after changing the stored value.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::store::keys;

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
