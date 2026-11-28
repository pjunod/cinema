//! Per-node readiness for the AI subtitles settings page.
//!
//! Each node decides for itself what the saved settings need and reports it
//! in its subtitle worker observation ([`crate::subtitle_models::self_install`]).
//! This module only folds those cluster-wide observations into one state per
//! node: `ready`, `downloading`, `failed` (with a reason), `unsupported`, or
//! `unknown` when the node's observation is unavailable or predates
//! self-install.
use crate::{http::error::ApiError, state::AppState};
use serde_json::{json, Value};

/// Readiness of every node the existing worker observations cover.
pub(crate) async fn cluster(state: &AppState) -> Result<Value, ApiError> {
    Ok(build(&crate::subtitle_models::workers_public(state).await?))
}

fn rank(state: &str) -> u8 {
    match state {
        "unsupported" => 3,
        "failed" => 2,
        "downloading" => 1,
        _ => 0,
    }
}

fn node(row: &Value) -> Value {
    let label = row["label"].as_str().unwrap_or("Unavailable worker");
    let base = |state: &str, reason: Option<&str>, tested: Option<i64>| {
        json!({"node_id":row["node_id"],"label":label,"cores":row["cores"].as_u64(),"gpu":null,
            "state":state,"reason":reason,"tested_at_ms":tested})
    };
    if row["observed_at_ms"].is_null() {
        return base("unknown", Some("worker_observation_unavailable"), None);
    }
    let Some(requirements) = row["requirements"].as_array() else {
        return base("unknown", Some("readiness_unreported"), None);
    };
    let tested = requirements
        .iter()
        .filter_map(|requirement| requirement["tested_at_ms"].as_i64())
        .max();
    let worst = requirements
        .iter()
        .max_by_key(|requirement| rank(requirement["state"].as_str().unwrap_or("")));
    match worst {
        None => base("ready", None, None),
        Some(requirement) => {
            let state = requirement["state"].as_str().unwrap_or("unknown");
            base(
                state,
                if state == "ready" {
                    None
                } else {
                    requirement["reason"].as_str()
                },
                tested,
            )
        }
    }
}

/// `{nodes:[{node_id,label,cores,gpu,state,reason,tested_at_ms}], summary}`
/// from worker observation rows ([`crate::subtitle_models::workers_public`]).
pub(crate) fn build(rows: &[Value]) -> Value {
    let nodes = rows.iter().map(node).collect::<Vec<_>>();
    let count = |state: &str| nodes.iter().filter(|node| node["state"] == state).count();
    json!({
        "nodes": nodes,
        "summary": {
            "nodes": nodes.len(),
            "ready": count("ready"),
            "downloading": count("downloading"),
            "failed": count("failed"),
            "unsupported": count("unsupported"),
            "unknown": count("unknown"),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_folds_each_nodes_requirements_into_one_state() {
        let rows = vec![
            json!({"node_id":"a","label":"Node A","observed_at_ms":1,"cores":8,
                "requirements":[
                    {"model_id":"small","state":"ready","reason":null,"tested_at_ms":100},
                    {"model_id":"opus-mt-mul-en","state":"ready","reason":null,"tested_at_ms":200}]}),
            json!({"node_id":"b","label":"Node B","observed_at_ms":1,"cores":4,
                "requirements":[
                    {"model_id":"small","state":"downloading","reason":"installing","tested_at_ms":null}]}),
            json!({"node_id":"c","label":"Node C","observed_at_ms":1,"cores":4,
                "requirements":[
                    {"model_id":"small","state":"failed","reason":"model_download_failed","retry_at_ms":9},
                    {"model_id":"opus-mt-mul-en","state":"ready","reason":null,"tested_at_ms":50}]}),
            json!({"node_id":"d","label":"Node D","observed_at_ms":1,"cores":2,
                "requirements":[
                    {"model_id":"small","state":"unsupported","reason":"insufficient_memory"}]}),
            json!({"node_id":"e","label":"Unavailable worker","observed_at_ms":null}),
            json!({"node_id":"f","label":"Old peer","observed_at_ms":1,"cores":2}),
            json!({"node_id":"g","label":"Nothing needed","observed_at_ms":1,"requirements":[]}),
        ];
        let readiness = build(&rows);
        let nodes = readiness["nodes"].as_array().expect("nodes");
        assert_eq!(nodes.len(), 7);
        for node in nodes {
            let mut keys = node
                .as_object()
                .expect("node object")
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            keys.sort();
            assert_eq!(
                keys,
                ["cores", "gpu", "label", "node_id", "reason", "state", "tested_at_ms"]
            );
            assert!(node["gpu"].is_null());
        }
        assert_eq!(
            nodes[0],
            json!({"node_id":"a","label":"Node A","cores":8,"gpu":null,"state":"ready","reason":null,"tested_at_ms":200})
        );
        assert_eq!(nodes[1]["state"], "downloading");
        assert_eq!(nodes[1]["reason"], "installing");
        assert_eq!(nodes[2]["state"], "failed");
        assert_eq!(nodes[2]["reason"], "model_download_failed");
        assert_eq!(nodes[2]["tested_at_ms"], 50);
        assert_eq!(nodes[3]["state"], "unsupported");
        assert_eq!(nodes[3]["reason"], "insufficient_memory");
        assert_eq!(nodes[4]["state"], "unknown");
        assert_eq!(nodes[4]["reason"], "worker_observation_unavailable");
        assert!(nodes[4]["cores"].is_null());
        assert_eq!(nodes[5]["state"], "unknown");
        assert_eq!(nodes[5]["reason"], "readiness_unreported");
        assert_eq!(nodes[6]["state"], "ready");
        assert_eq!(
            readiness["summary"],
            json!({"nodes":7,"ready":2,"downloading":1,"failed":1,"unsupported":1,"unknown":2})
        );
    }
}
