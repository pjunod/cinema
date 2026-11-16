//! Source capacity per node, for display: who holds which reader slots and
//! whether a lowered limit is still waiting on running readers. The claim
//! itself decides from the replicated settings inside its own statement; this
//! only reports the same settings beside the leases Activity already lists.
use plurx_core::store::background_jobs::SourceIoHolder;
use plurx_core::store::keys;
use std::collections::{BTreeMap, BTreeSet};

/// Default background source readers per node.
pub(crate) const DEFAULT_READERS_PER_NODE: i64 = 2;
/// Default background source readers per mapped storage domain.
pub(crate) const DEFAULT_READERS_PER_DOMAIN: i64 = 1;
/// Inclusive range every reader limit is saved in.
pub(crate) const READERS_RANGE: std::ops::RangeInclusive<i64> = 1..=16;

fn stored(settings: &BTreeMap<String, String>, key: &str) -> Option<i64> {
    settings
        .get(key)
        .and_then(|value| value.trim().parse::<i64>().ok())
        .filter(|value| READERS_RANGE.contains(value))
}

/// The cluster default, as the claim reads it.
pub(crate) fn readers_per_node(settings: &BTreeMap<String, String>) -> i64 {
    stored(settings, keys::JOB_SOURCE_READERS_PER_NODE).unwrap_or(DEFAULT_READERS_PER_NODE)
}

/// Per-node overrides, by node id.
pub(crate) fn node_overrides(settings: &BTreeMap<String, String>) -> BTreeMap<String, i64> {
    prefixed(settings, keys::JOB_SOURCE_READERS_NODE_PREFIX)
}

/// Per-domain `source_readers`, by domain id.
pub(crate) fn domain_readers(settings: &BTreeMap<String, String>) -> BTreeMap<String, i64> {
    prefixed(settings, keys::JOB_DOMAIN_SOURCE_READERS_PREFIX)
}

fn prefixed(settings: &BTreeMap<String, String>, prefix: &str) -> BTreeMap<String, i64> {
    settings
        .iter()
        .filter_map(|(key, _)| {
            let id = key.strip_prefix(prefix)?;
            Some((id.to_owned(), stored(settings, key)?))
        })
        .collect()
}

/// `{per_node, nodes:[{node_id, readers, overridden, maintenance, live,
/// playback, paused, waiting_to_apply}], domains:[{domain_id, readers,
/// maintenance, live, waiting_to_apply}]}`. `nodes` lists every node in
/// `known` plus any node holding a lease or carrying an override.
pub(crate) fn summary(
    settings: &BTreeMap<String, String>,
    holders: &[SourceIoHolder],
    known: impl IntoIterator<Item = String>,
) -> serde_json::Value {
    let per_node = readers_per_node(settings);
    let overrides = node_overrides(settings);
    let domains = domain_readers(settings);
    #[derive(Default)]
    struct Count {
        maintenance: BTreeSet<String>,
        live: BTreeSet<String>,
        playback: BTreeSet<String>,
        paused: BTreeSet<String>,
    }
    let mut nodes: BTreeMap<String, Count> = known
        .into_iter()
        .chain(overrides.keys().cloned())
        .map(|node| (node, Count::default()))
        .collect();
    let mut by_domain: BTreeMap<String, Count> = BTreeMap::new();
    for holder in holders {
        let Some(node) = holder.origin_node_id.clone() else {
            continue;
        };
        let lease = holder
            .lease_id
            .clone()
            .unwrap_or_else(|| format!("{}:{}", holder.kind, holder.work_id));
        let count = nodes.entry(node).or_default();
        let counted = holder
            .counts_against
            .as_deref()
            .is_some_and(|c| c != "none");
        let set = if holder.priority == "foreground" {
            &mut count.playback
        } else if holder.phase.as_deref() == Some("source_paused") {
            &mut count.paused
        } else if !counted {
            continue;
        } else if holder.live_demand {
            &mut count.live
        } else {
            &mut count.maintenance
        };
        set.insert(lease.clone());
        if counted {
            if let Some(domain) = holder.resource_key.strip_prefix("source_io:") {
                let entry = by_domain.entry(domain.to_owned()).or_default();
                if holder.live_demand {
                    entry.live.insert(lease);
                } else {
                    entry.maintenance.insert(lease);
                }
            }
        }
    }
    for domain in domains.keys() {
        by_domain.entry(domain.clone()).or_default();
    }
    let nodes = nodes
        .into_iter()
        .map(|(node_id, count)| {
            let readers = overrides.get(&node_id).copied().unwrap_or(per_node);
            serde_json::json!({
                "node_id": node_id,
                "readers": readers,
                "overridden": overrides.contains_key(&node_id),
                "maintenance": count.maintenance.len(),
                "live": count.live.len(),
                "playback": count.playback.len(),
                "paused": count.paused.len(),
                "waiting_to_apply": count.maintenance.len() as i64 > readers,
            })
        })
        .collect::<Vec<_>>();
    let domains = by_domain
        .into_iter()
        .map(|(domain_id, count)| {
            let readers = domains
                .get(&domain_id)
                .copied()
                .unwrap_or(DEFAULT_READERS_PER_DOMAIN);
            serde_json::json!({
                "domain_id": domain_id,
                "readers": readers,
                "maintenance": count.maintenance.len(),
                "live": count.live.len(),
                "waiting_to_apply": count.maintenance.len() as i64 > readers,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({"per_node": per_node, "nodes": nodes, "domains": domains})
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
            "origin_job_state": null, "counts_against": if key == "source_io" { "node" } else { "domain" },
            "live_demand": live, "lease_id": lease,
        }))
        .expect("holder")
    }

    #[test]
    fn a_lowered_limit_reports_the_nodes_still_above_it() {
        let mut settings = BTreeMap::new();
        settings.insert(keys::JOB_SOURCE_READERS_PER_NODE.to_owned(), "1".to_owned());
        settings.insert(
            format!("{}node-b", keys::JOB_SOURCE_READERS_NODE_PREFIX),
            "3".to_owned(),
        );
        let holders = [
            holder("job:a:1", "node-a", "source_io", false),
            holder("job:b:1", "node-a", "source_io", false),
            holder("job:c:1", "node-a", "source_io", true),
            holder("job:d:1", "node-b", "source_io", false),
        ];
        let summary = summary(&settings, &holders, ["node-c".to_owned()]);
        assert_eq!(summary["per_node"], 1);
        let nodes = summary["nodes"].as_array().expect("nodes");
        let a = nodes.iter().find(|n| n["node_id"] == "node-a").expect("a");
        assert_eq!(
            (a["maintenance"].clone(), a["live"].clone()),
            (2.into(), 1.into())
        );
        assert_eq!(
            a["waiting_to_apply"], true,
            "two maintenance readers of one"
        );
        let b = nodes.iter().find(|n| n["node_id"] == "node-b").expect("b");
        assert_eq!(
            (b["readers"].clone(), b["overridden"].clone()),
            (3.into(), true.into())
        );
        assert_eq!(b["waiting_to_apply"], false);
        let c = nodes
            .iter()
            .find(|n| n["node_id"] == "node-c")
            .expect("idle node listed");
        assert_eq!(c["maintenance"], 0);
    }
}
