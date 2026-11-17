//! Per-root source capacity and informational holder counts for Activity and
//! Settings. Claims enforce root limits from replicated settings atomically;
//! this module projects those settings beside the lease rows.
use plurx_core::domain::Library;
use plurx_core::store::background_jobs::SourceIoHolder;
use plurx_core::store::keys;
use std::collections::{BTreeMap, BTreeSet};

/// Default background source readers per library root.
pub(crate) const DEFAULT_READERS_PER_ROOT: i64 = 2;
/// Inclusive range every source reader limit accepts.
pub(crate) const READERS_RANGE: std::ops::RangeInclusive<i64> = 1..=16;
const ROOT_RESOURCE_PREFIX: &str = "source_io:root/";

/// One replicated library root and its stable settings/resource identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RootInventory {
    pub id: String,
    pub library_id: i64,
    pub library_name: String,
    pub path: String,
}

/// Canonical root identity: library id plus uppercase hex of its UTF-8 path.
/// Trailing slashes are removed; all-slash paths canonicalize to root.
pub(crate) fn root_id(library_id: i64, path: &str) -> Option<String> {
    if library_id <= 0 || path.is_empty() {
        return None;
    }
    let canonical = canonical_root_path(path);
    Some(format!("{library_id}/{}", hex_upper(canonical.as_bytes())))
}

fn canonical_root_path(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/"
    } else {
        trimmed
    }
}

pub(crate) fn root_inventory(libraries: &[Library]) -> Vec<RootInventory> {
    let mut roots = BTreeMap::new();
    for library in libraries {
        for path in &library.paths {
            let Some(path) = path.to_str() else {
                continue;
            };
            let Some(id) = root_id(library.id, path) else {
                continue;
            };
            let canonical_path = canonical_root_path(path).to_owned();
            roots.insert(
                id.clone(),
                RootInventory {
                    id,
                    library_id: library.id,
                    library_name: library.name.clone(),
                    path: canonical_path,
                },
            );
        }
    }
    roots.into_values().collect()
}

pub(crate) fn valid_root_id(id: &str) -> bool {
    let Some((library_text, hex)) = id.split_once('/') else {
        return false;
    };
    let Ok(library_id) = library_text.parse::<i64>() else {
        return false;
    };
    if library_id <= 0
        || library_id.to_string() != library_text
        || hex.is_empty()
        || hex.len() % 2 != 0
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
    {
        return false;
    }
    let bytes = hex
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16)? as u8;
            let low = (pair[1] as char).to_digit(16)? as u8;
            Some((high << 4) | low)
        })
        .collect::<Option<Vec<_>>>();
    let Some(bytes) = bytes else {
        return false;
    };
    let Ok(path) = std::str::from_utf8(&bytes) else {
        return false;
    };
    !path.is_empty()
        && canonical_root_path(path) == path
        && root_id(library_id, path).as_deref() == Some(id)
}

fn hex_upper(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn stored(settings: &BTreeMap<String, String>, key: &str) -> Option<i64> {
    settings
        .get(key)
        .map(|value| sql_integer(value))
        .filter(|value| READERS_RANGE.contains(value))
}

/// `CAST(value AS INTEGER)` as the claim reads it: leading spaces, then the
/// longest signed-integer prefix (`"3 readers"` is 3), else 0.
fn sql_integer(value: &str) -> i64 {
    let text = value.trim_start_matches([' ', '\t', '\n', '\r', '\u{000b}', '\u{000c}']);
    let (sign, digits) = match text.as_bytes().first() {
        Some(b'-') => (-1, &text[1..]),
        Some(b'+') => (1, &text[1..]),
        _ => (1, text),
    };
    let end = digits
        .bytes()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(digits.len());
    digits[..end]
        .parse::<i64>()
        .map_or(0, |magnitude| sign * magnitude)
}

/// The cluster root default, as the claim reads it.
pub(crate) fn readers_per_root(settings: &BTreeMap<String, String>) -> i64 {
    stored(settings, keys::JOB_SOURCE_READERS_PER_ROOT).unwrap_or(DEFAULT_READERS_PER_ROOT)
}

/// Root overrides, by canonical `<library_id>/<uppercase-hex-path>` id.
pub(crate) fn root_overrides(settings: &BTreeMap<String, String>) -> BTreeMap<String, i64> {
    prefixed(settings, keys::JOB_ROOT_SOURCE_READERS_PREFIX)
        .into_iter()
        .filter(|(id, _)| valid_root_id(id))
        .collect()
}

/// Explicit per-domain aggregate caps. An absent setting means unlimited.
pub(crate) fn domain_readers(settings: &BTreeMap<String, String>) -> BTreeMap<String, i64> {
    prefixed(settings, keys::JOB_DOMAIN_SOURCE_READERS_PREFIX)
}

fn prefixed(settings: &BTreeMap<String, String>, prefix: &str) -> BTreeMap<String, i64> {
    settings
        .keys()
        .filter_map(|key| {
            let id = key.strip_prefix(prefix)?;
            Some((id.to_owned(), stored(settings, key)?))
        })
        .collect()
}

#[derive(Default)]
struct Count {
    maintenance: BTreeSet<String>,
    live: BTreeSet<String>,
    playback: BTreeSet<String>,
    paused: BTreeSet<String>,
}

fn add_holder(count: &mut Count, holder: &SourceIoHolder, lease: &str, counted: bool) {
    if holder.priority == "foreground" {
        count.playback.insert(lease.to_owned());
    } else if holder.phase.as_deref() == Some("source_paused") {
        count.paused.insert(lease.to_owned());
    } else if counted && holder.live_demand {
        count.live.insert(lease.to_owned());
    } else if counted {
        count.maintenance.insert(lease.to_owned());
    }
}

/// `{per_root, roots, nodes, domains}`. Node rows are occupancy only: there
/// is no implicit node cap. Root rows include every current root and stale
/// configured overrides so removed roots can still be reset.
pub(crate) fn summary(
    settings: &BTreeMap<String, String>,
    holders: &[SourceIoHolder],
    known: impl IntoIterator<Item = String>,
    roots: &[RootInventory],
) -> serde_json::Value {
    let per_root = readers_per_root(settings);
    let overrides = root_overrides(settings);
    let domains = domain_readers(settings);
    let mut nodes: BTreeMap<String, Count> = known
        .into_iter()
        .map(|node| (node, Count::default()))
        .collect();
    let mut by_root: BTreeMap<String, Count> = BTreeMap::new();
    let mut by_domain: BTreeMap<String, Count> = BTreeMap::new();

    for holder in holders {
        let Some(node) = holder.origin_node_id.clone() else {
            continue;
        };
        let lease = holder
            .lease_id
            .clone()
            .unwrap_or_else(|| format!("{}:{}", holder.kind, holder.work_id));
        let counted = holder
            .counts_against
            .as_deref()
            .is_some_and(|value| value != "none");
        add_holder(nodes.entry(node).or_default(), holder, &lease, counted);
        if !counted {
            continue;
        }
        if let Some(id) = holder.resource_key.strip_prefix(ROOT_RESOURCE_PREFIX) {
            add_holder(
                by_root.entry(id.to_owned()).or_default(),
                holder,
                &lease,
                true,
            );
        } else if let Some(domain) = holder
            .resource_key
            .strip_prefix("source_io:")
            .filter(|domain| !domain.is_empty())
        {
            add_holder(
                by_domain.entry(domain.to_owned()).or_default(),
                holder,
                &lease,
                true,
            );
        }
    }

    let mut inventory: BTreeMap<String, RootInventory> = roots
        .iter()
        .cloned()
        .map(|root| (root.id.clone(), root))
        .collect();
    for id in overrides.keys() {
        inventory
            .entry(id.clone())
            .or_insert_with(|| RootInventory {
                id: id.clone(),
                library_id: id
                    .split_once('/')
                    .and_then(|(value, _)| value.parse().ok())
                    .unwrap_or_default(),
                library_name: "Removed library root".to_owned(),
                path: String::new(),
            });
    }
    let roots = inventory
        .into_values()
        .map(|root| {
            let count = by_root.remove(&root.id).unwrap_or_default();
            let readers = overrides.get(&root.id).copied().unwrap_or(per_root);
            serde_json::json!({
                "root_id": root.id,
                "library_id": root.library_id,
                "library_name": root.library_name,
                "root_path": root.path,
                "readers": readers,
                "overridden": overrides.contains_key(&root.id),
                "maintenance": count.maintenance.len(),
                "live": count.live.len(),
                "playback": count.playback.len(),
                "paused": count.paused.len(),
                "waiting_to_apply": count.maintenance.len() as i64 > readers,
            })
        })
        .collect::<Vec<_>>();

    for domain in domains.keys() {
        by_domain.entry(domain.clone()).or_default();
    }
    let nodes = nodes
        .into_iter()
        .map(|(node_id, count)| {
            serde_json::json!({
                "node_id": node_id,
                "maintenance": count.maintenance.len(),
                "live": count.live.len(),
                "playback": count.playback.len(),
                "paused": count.paused.len(),
            })
        })
        .collect::<Vec<_>>();
    let domains = by_domain
        .into_iter()
        .map(|(domain_id, count)| {
            let readers = domains.get(&domain_id).copied();
            serde_json::json!({
                "domain_id": domain_id,
                "readers": readers,
                "overridden": readers.is_some(),
                "maintenance": count.maintenance.len(),
                "live": count.live.len(),
                "waiting_to_apply": readers.is_some_and(|limit| count.maintenance.len() as i64 > limit),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({"per_root": per_root, "roots": roots, "nodes": nodes, "domains": domains})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holder(lease: &str, node: &str, key: &str, live: bool) -> SourceIoHolder {
        serde_json::from_value(serde_json::json!({
            "resource_key": key, "work_id": lease, "kind": "library_scan",
            "priority": if live { "viewer" } else { "maintenance" },
            "started_at_ms": 1, "lease_expires_at_ms": 2, "phase": null,
            "last_phase_at_ms": null, "origin_node_id": node, "origin_job_kind": null,
            "origin_job_state": null, "counts_against": if key == "source_io" { "unrooted" } else { "root" },
            "live_demand": live, "lease_id": lease,
        }))
        .expect("holder")
    }

    /// The display must agree with the replicated claim's SQLite CAST,
    /// including its ASCII-only leading whitespace.
    #[test]
    fn settings_read_as_the_claim_casts_them() {
        for (value, expected) in [
            (" 4", 4),
            ("3 readers", 3),
            ("5\n", 5),
            ("x", 2),
            ("-3", 2),
            ("\u{00a0}3", 2),
            ("\u{000b}3", 3),
        ] {
            let settings = BTreeMap::from([(
                keys::JOB_SOURCE_READERS_PER_ROOT.to_owned(),
                value.to_owned(),
            )]);
            assert_eq!(readers_per_root(&settings), expected, "{value:?}");
        }
    }

    #[test]
    fn root_ids_use_canonical_uppercase_hex_and_root_override_ids_validate() {
        let id = root_id(12, "/mnt/Médía///").expect("canonical root");
        assert_eq!(id, "12/2F6D6E742F4DC3A964C3AD612F");
        assert!(valid_root_id(&id));
        assert!(!valid_root_id("012/2F"));
        assert!(!valid_root_id("12/2f"));
        assert!(!valid_root_id("12/2F2F"));
        assert_eq!(root_id(12, "/"), Some("12/2F".to_owned()));
        assert_eq!(root_id(12, "////"), Some("12/2F".to_owned()));
        assert_eq!(root_id(12, ""), None);
        let relative = root_id(12, "media/movies/").expect("configured relative root");
        assert_eq!(relative, "12/6D656469612F6D6F76696573");
        assert!(valid_root_id(&relative));
    }

    #[test]
    fn root_summary_counts_independent_roots_and_reports_nodes_without_a_node_cap() {
        let first = root_id(1, "/nas/a").expect("root");
        let second = root_id(1, "/nas/b").expect("root");
        let first_key = format!("{ROOT_RESOURCE_PREFIX}{first}");
        let second_key = format!("{ROOT_RESOURCE_PREFIX}{second}");
        let mut settings =
            BTreeMap::from([(keys::JOB_SOURCE_READERS_PER_ROOT.to_owned(), "1".to_owned())]);
        settings.insert(
            format!("{}{}", keys::JOB_ROOT_SOURCE_READERS_PREFIX, first),
            "1".to_owned(),
        );
        settings.insert(
            format!("{}nas", keys::JOB_DOMAIN_SOURCE_READERS_PREFIX),
            "3".into(),
        );
        let roots = [
            RootInventory {
                id: first.clone(),
                library_id: 1,
                library_name: "Movies".to_owned(),
                path: "/nas/a".to_owned(),
            },
            RootInventory {
                id: second.clone(),
                library_id: 1,
                library_name: "Movies".to_owned(),
                path: "/nas/b".to_owned(),
            },
        ];
        let holders = [
            holder("a1", "node-a", &first_key, false),
            holder("a2", "node-b", &first_key, false),
            holder("a3", "node-a", &first_key, true),
            holder("b1", "node-a", &second_key, false),
        ];
        let result = summary(&settings, &holders, ["node-c".to_owned()], &roots);
        assert_eq!(result["per_root"], 1);
        let rows = result["roots"].as_array().expect("root rows");
        let first_row = rows
            .iter()
            .find(|row| row["root_id"] == first)
            .expect("first");
        assert_eq!(first_row["maintenance"], 2);
        assert_eq!(first_row["live"], 1);
        assert_eq!(first_row["readers"], 1);
        assert_eq!(first_row["waiting_to_apply"], true);
        let second_row = rows
            .iter()
            .find(|row| row["root_id"] == second)
            .expect("second");
        assert_eq!(second_row["maintenance"], 1);
        assert_eq!(second_row["readers"], 1);
        let node = result["nodes"]
            .as_array()
            .and_then(|nodes| nodes.iter().find(|row| row["node_id"] == "node-a"))
            .expect("node");
        assert_eq!(node["maintenance"], 2);
        assert!(
            node.get("readers").is_none(),
            "node counts are informational"
        );
        let domain = result["domains"]
            .as_array()
            .and_then(|domains| domains.iter().find(|row| row["domain_id"] == "nas"))
            .expect("explicit domain cap");
        assert_eq!(domain["readers"], 3);
    }
}
