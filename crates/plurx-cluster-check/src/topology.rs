//! Deterministic three-versus-four-voter topology evidence.
//!
//! This module deliberately separates semantic CI evidence from named-runner
//! performance evidence. GitHub-hosted CI proves that fresh independent
//! clusters execute the same quorum-acknowledged workload and emit a complete,
//! versioned artifact. It does not turn noisy hosted-runner timings or absent
//! host counters into a latency or resource claim.

use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    harness_executable, start_cluster_with_port_retry, ClusterProcesses, Request, Response,
    CONVERGENCE_TIMEOUT,
};
use plurx_core::cluster::migration::status::ReplicationHealth;

pub const TOPOLOGY_ARTIFACT_SCHEMA_VERSION: u32 = 1;
pub const TOPOLOGY_WRITE_OPERATIONS: u64 = 64;
const TOPOLOGY_VALUE_BYTES: usize = 64;
const SEMANTIC_EVIDENCE_SCOPE: &str = "semantic_ci";
const NAMED_RUNNER_EVIDENCE_SCOPE: &str = "named_runner";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClusterTopologyArtifact {
    pub schema_version: u32,
    pub evidence_scope: String,
    pub build_sha: String,
    pub workload: TopologyWorkload,
    pub workload_sha256: String,
    pub topology_order: Vec<u64>,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub runs: Vec<TopologyRun>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologyWorkload {
    pub id: String,
    pub operation: String,
    pub operations: u64,
    pub concurrency: u64,
    pub value_bytes: u64,
    pub latency_unit: String,
    pub index_unit: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TopologyRun {
    pub voter_count: u64,
    pub quorum: u64,
    pub leader: u64,
    pub request_target: String,
    pub workload_sha256: String,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub errors: u64,
    pub applied_index_before: u64,
    pub applied_index_after: u64,
    pub physical_commit_entries: u64,
    pub commit_latency_p50_us: f64,
    pub commit_latency_p95_us: f64,
    pub commit_latency_p99_us: f64,
    pub raw_commit_latency_us: Vec<u64>,
    pub applied_indexes: Vec<NodeAppliedIndex>,
    pub max_apply_lag_entries: u64,
    pub controller_host: String,
    pub load_generator_host: String,
    pub resources: Vec<ResourceSample>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeAppliedIndex {
    pub node_id: u64,
    pub applied_index: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResourceSample {
    pub node_id: u64,
    pub hardware: String,
    pub storage_device: String,
    pub network_path: String,
    pub cpu_seconds: Option<f64>,
    pub wall_seconds: Option<f64>,
    pub max_rss_bytes: Option<u64>,
    pub storage_read_bytes: Option<u64>,
    pub storage_write_bytes: Option<u64>,
    pub network_receive_bytes: Option<u64>,
    pub network_transmit_bytes: Option<u64>,
}

impl TopologyWorkload {
    fn semantic() -> Self {
        Self {
            id: "settings-put-v1".to_owned(),
            operation: "quorum_acknowledged_put_setting".to_owned(),
            operations: TOPOLOGY_WRITE_OPERATIONS,
            concurrency: 1,
            value_bytes: TOPOLOGY_VALUE_BYTES as u64,
            latency_unit: "microseconds".to_owned(),
            index_unit: "raft_entries".to_owned(),
        }
    }

    fn sha256(&self) -> Result<String> {
        Ok(hex::encode(Sha256::digest(serde_json::to_vec(self)?)))
    }
}

pub fn parse_topology_order(value: Option<&str>) -> Result<[u64; 2]> {
    match value.unwrap_or("3,4") {
        "3,4" => Ok([3, 4]),
        "4,3" => Ok([4, 3]),
        other => bail!("topology order must be exactly 3,4 or 4,3; got {other}"),
    }
}

pub async fn run_topology_comparison(output: &Path, order: [u64; 2]) -> Result<()> {
    let executable = harness_executable()?;
    let root = tempfile::tempdir().context("topology comparison data root")?;
    let workload = TopologyWorkload::semantic();
    let workload_sha256 = workload.sha256()?;
    let started_at_unix_ms = unix_ms()?;
    let mut runs = Vec::with_capacity(order.len());

    for voter_count in order {
        println!("cluster-check: fresh {voter_count}-voter topology workload");
        runs.push(
            run_one_topology(
                &executable,
                root.path(),
                voter_count,
                &workload,
                &workload_sha256,
            )
            .await?,
        );
    }

    let artifact = ClusterTopologyArtifact {
        schema_version: TOPOLOGY_ARTIFACT_SCHEMA_VERSION,
        evidence_scope: SEMANTIC_EVIDENCE_SCOPE.to_owned(),
        build_sha: resolve_build_sha()?,
        workload,
        workload_sha256,
        topology_order: order.to_vec(),
        started_at_unix_ms,
        finished_at_unix_ms: unix_ms()?,
        runs,
    };
    validate_topology_artifact(&artifact)?;

    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create topology artifact directory {parent:?}"))?;
    }
    let mut bytes = serde_json::to_vec_pretty(&artifact)?;
    bytes.push(b'\n');
    std::fs::write(output, bytes).with_context(|| format!("write topology artifact {output:?}"))?;
    println!("cluster-check: topology artifact {}", output.display());
    Ok(())
}

async fn run_one_topology(
    executable: &Path,
    root: &Path,
    voter_count: u64,
    workload: &TopologyWorkload,
    workload_sha256: &str,
) -> Result<TopologyRun> {
    let topology_root = root.join(format!("voters-{voter_count}"));
    let (mut cluster, _) =
        start_cluster_with_port_retry(executable, &topology_root, voter_count).await?;
    let result = exercise_topology(&mut cluster, voter_count, workload, workload_sha256).await;
    match result {
        Ok(run) => {
            cluster.shutdown_all().await?;
            Ok(run)
        }
        Err(error) => {
            cluster.kill_all().await;
            Err(error)
        }
    }
}

async fn exercise_topology(
    cluster: &mut ClusterProcesses,
    voter_count: u64,
    workload: &TopologyWorkload,
    workload_sha256: &str,
) -> Result<TopologyRun> {
    cluster.request(1, Request::Bootstrap).await?.require_ok()?;
    for node_id in 2..=voter_count {
        cluster
            .request(node_id, Request::Open)
            .await?
            .require_ok()?;
    }
    let voters = (1..=voter_count).collect::<Vec<_>>();
    cluster.wait_for_voters(&voters).await?;
    for node_id in &voters {
        cluster
            .wait_for_replication_health(*node_id, ReplicationHealth::InSync)
            .await?;
    }

    let leader = cluster.leader().await?;
    let applied_index_before = metric_index(cluster, leader).await?;
    let started_at_unix_ms = unix_ms()?;
    let value = "x".repeat(usize::try_from(workload.value_bytes)?);
    let mut raw_commit_latency_us = Vec::with_capacity(usize::try_from(workload.operations)?);
    for ordinal in 0..workload.operations {
        let started = Instant::now();
        cluster
            .request(
                leader,
                Request::TopologyWrite {
                    ordinal,
                    value: value.clone(),
                },
            )
            .await?
            .require_ok()?;
        raw_commit_latency_us.push(duration_us(started.elapsed()));
    }
    let applied_index_after = metric_index(cluster, leader).await?;
    let applied_indexes = wait_for_applied(cluster, &voters, applied_index_after).await?;
    let max_apply_lag_entries = applied_indexes
        .iter()
        .map(|sample| applied_index_after.saturating_sub(sample.applied_index))
        .max()
        .unwrap_or_default();
    let finished_at_unix_ms = unix_ms()?;

    Ok(TopologyRun {
        voter_count,
        quorum: voter_count / 2 + 1,
        leader,
        request_target: "leader".to_owned(),
        workload_sha256: workload_sha256.to_owned(),
        started_at_unix_ms,
        finished_at_unix_ms,
        errors: 0,
        applied_index_before,
        applied_index_after,
        physical_commit_entries: applied_index_after.saturating_sub(applied_index_before),
        commit_latency_p50_us: percentile_type7(&raw_commit_latency_us, 0.50)?,
        commit_latency_p95_us: percentile_type7(&raw_commit_latency_us, 0.95)?,
        commit_latency_p99_us: percentile_type7(&raw_commit_latency_us, 0.99)?,
        raw_commit_latency_us,
        applied_indexes,
        max_apply_lag_entries,
        controller_host: "github-hosted-ephemeral".to_owned(),
        load_generator_host: "controller-process".to_owned(),
        resources: voters
            .into_iter()
            .map(|node_id| ResourceSample {
                node_id,
                hardware: "not-collected-semantic-ci".to_owned(),
                storage_device: "not-collected-semantic-ci".to_owned(),
                network_path: "loopback-semantic-ci".to_owned(),
                cpu_seconds: None,
                wall_seconds: None,
                max_rss_bytes: None,
                storage_read_bytes: None,
                storage_write_bytes: None,
                network_receive_bytes: None,
                network_transmit_bytes: None,
            })
            .collect(),
    })
}

async fn metric_index(cluster: &mut ClusterProcesses, node_id: u64) -> Result<u64> {
    match cluster.request(node_id, Request::Metrics).await? {
        Response::Metrics {
            applied_index: Some(index),
            ..
        } => Ok(index),
        response => bail!("topology voter {node_id} omitted its applied index: {response:?}"),
    }
}

async fn wait_for_applied(
    cluster: &mut ClusterProcesses,
    voters: &[u64],
    target: u64,
) -> Result<Vec<NodeAppliedIndex>> {
    let deadline = Instant::now() + CONVERGENCE_TIMEOUT;
    loop {
        let mut samples = Vec::with_capacity(voters.len());
        for node_id in voters {
            samples.push(NodeAppliedIndex {
                node_id: *node_id,
                applied_index: metric_index(cluster, *node_id).await?,
            });
        }
        if samples.iter().all(|sample| sample.applied_index >= target) {
            return Ok(samples);
        }
        if Instant::now() >= deadline {
            bail!("topology voters did not apply through committed index {target}: {samples:?}");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

pub fn percentile_type7(samples: &[u64], quantile: f64) -> Result<f64> {
    if samples.is_empty() {
        bail!("cannot calculate a percentile without samples");
    }
    if !(0.0..=1.0).contains(&quantile) || !quantile.is_finite() {
        bail!("percentile quantile must be finite and inside 0..=1");
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let position = (sorted.len() - 1) as f64 * quantile;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    let fraction = position - lower as f64;
    Ok(sorted[lower] as f64 + (sorted[upper] as f64 - sorted[lower] as f64) * fraction)
}

pub fn validate_topology_artifact(artifact: &ClusterTopologyArtifact) -> Result<()> {
    if artifact.schema_version != TOPOLOGY_ARTIFACT_SCHEMA_VERSION {
        bail!(
            "unsupported topology artifact schema version {}",
            artifact.schema_version
        );
    }
    if artifact.evidence_scope != SEMANTIC_EVIDENCE_SCOPE
        && artifact.evidence_scope != NAMED_RUNNER_EVIDENCE_SCOPE
    {
        bail!(
            "unknown topology evidence scope {}",
            artifact.evidence_scope
        );
    }
    if artifact.build_sha.len() != 40
        || !artifact
            .build_sha
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("topology artifact build_sha must be a full 40-character Git SHA");
    }
    if artifact.topology_order.as_slice() != [3, 4] && artifact.topology_order.as_slice() != [4, 3]
    {
        bail!("topology artifact order must contain three and four voters exactly once");
    }
    if artifact.started_at_unix_ms > artifact.finished_at_unix_ms {
        bail!("topology artifact finished before it started");
    }
    if artifact.workload.operations == 0
        || artifact.workload.concurrency == 0
        || artifact.workload.value_bytes == 0
        || artifact.workload.latency_unit != "microseconds"
        || artifact.workload.index_unit != "raft_entries"
    {
        bail!("topology artifact workload is empty or uses undeclared units");
    }
    if artifact.workload.sha256()? != artifact.workload_sha256 {
        bail!("topology artifact workload hash does not match its declaration");
    }
    if artifact.runs.len() != 2 {
        bail!("topology artifact must contain exactly two topology runs");
    }

    for (position, run) in artifact.runs.iter().enumerate() {
        if run.voter_count != artifact.topology_order[position] {
            bail!("topology run order does not match the declared order");
        }
        if run.quorum != run.voter_count / 2 + 1 {
            bail!(
                "{} voters reported invalid quorum {}",
                run.voter_count,
                run.quorum
            );
        }
        if run.leader == 0 || run.leader > run.voter_count || run.request_target != "leader" {
            bail!("topology run did not target its elected leader");
        }
        if run.workload_sha256 != artifact.workload_sha256 {
            bail!("topology runs did not execute the identical workload");
        }
        if run.started_at_unix_ms > run.finished_at_unix_ms
            || run.started_at_unix_ms < artifact.started_at_unix_ms
            || run.finished_at_unix_ms > artifact.finished_at_unix_ms
        {
            bail!("topology run timestamps fall outside the artifact interval");
        }
        if run.errors != 0 {
            bail!("topology run recorded {} write errors", run.errors);
        }
        if u64::try_from(run.raw_commit_latency_us.len())? != artifact.workload.operations {
            bail!("topology run sample count does not match the declared workload");
        }
        if run.physical_commit_entries != artifact.workload.operations
            || run
                .applied_index_after
                .saturating_sub(run.applied_index_before)
                != run.physical_commit_entries
        {
            bail!("topology run did not observe exactly one Raft entry per acknowledged write");
        }
        for (quantile, actual) in [
            (0.50, run.commit_latency_p50_us),
            (0.95, run.commit_latency_p95_us),
            (0.99, run.commit_latency_p99_us),
        ] {
            let expected = percentile_type7(&run.raw_commit_latency_us, quantile)?;
            if (expected - actual).abs() > f64::EPSILON {
                bail!("topology run percentile does not match its raw samples");
            }
        }
        let expected_nodes = (1..=run.voter_count).collect::<Vec<_>>();
        let applied_nodes = run
            .applied_indexes
            .iter()
            .map(|sample| sample.node_id)
            .collect::<Vec<_>>();
        let resource_nodes = run
            .resources
            .iter()
            .map(|sample| sample.node_id)
            .collect::<Vec<_>>();
        if applied_nodes != expected_nodes || resource_nodes != expected_nodes {
            bail!("topology run must report every voter exactly once in ascending order");
        }
        let expected_lag = run
            .applied_indexes
            .iter()
            .map(|sample| run.applied_index_after.saturating_sub(sample.applied_index))
            .max()
            .unwrap_or_default();
        if expected_lag != run.max_apply_lag_entries {
            bail!("topology run apply-lag summary does not match its node samples");
        }
        for sample in &run.resources {
            if sample.hardware.is_empty()
                || sample.storage_device.is_empty()
                || sample.network_path.is_empty()
            {
                bail!("topology resource samples must name hardware, storage, and network path");
            }
            let any_resource = sample.cpu_seconds.is_some()
                || sample.wall_seconds.is_some()
                || sample.max_rss_bytes.is_some()
                || sample.storage_read_bytes.is_some()
                || sample.storage_write_bytes.is_some()
                || sample.network_receive_bytes.is_some()
                || sample.network_transmit_bytes.is_some();
            let every_resource = sample.cpu_seconds.is_some()
                && sample.wall_seconds.is_some()
                && sample.max_rss_bytes.is_some()
                && sample.storage_read_bytes.is_some()
                && sample.storage_write_bytes.is_some()
                && sample.network_receive_bytes.is_some()
                && sample.network_transmit_bytes.is_some();
            if artifact.evidence_scope == SEMANTIC_EVIDENCE_SCOPE && any_resource {
                bail!("semantic CI artifact must not claim host resource measurements");
            }
            if artifact.evidence_scope == NAMED_RUNNER_EVIDENCE_SCOPE && !every_resource {
                bail!("named-runner artifact must contain every resource measurement");
            }
            if [sample.cpu_seconds, sample.wall_seconds]
                .into_iter()
                .flatten()
                .any(|value| !value.is_finite() || value < 0.0)
            {
                bail!("topology resource measurements must be finite and non-negative");
            }
        }
    }
    Ok(())
}

fn duration_us(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn unix_ms() -> Result<i64> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("topology comparison clock precedes Unix epoch")?
            .as_millis(),
    )
    .context("topology comparison Unix millisecond clock overflowed")
}

fn resolve_build_sha() -> Result<String> {
    if let Ok(value) = std::env::var("GITHUB_SHA") {
        let value = value.trim().to_ascii_lowercase();
        if value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(value);
        }
    }
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .context("resolve topology artifact build SHA")?;
    if !output.status.success() {
        bail!("git rev-parse HEAD failed while resolving topology artifact build SHA");
    }
    let value = String::from_utf8(output.stdout)?
        .trim()
        .to_ascii_lowercase();
    if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("git rev-parse HEAD did not return a full build SHA");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_run(voter_count: u64, workload_sha256: &str) -> TopologyRun {
        let raw_commit_latency_us = vec![10, 20, 30, 40];
        TopologyRun {
            voter_count,
            quorum: voter_count / 2 + 1,
            leader: 1,
            request_target: "leader".to_owned(),
            workload_sha256: workload_sha256.to_owned(),
            started_at_unix_ms: 1_001,
            finished_at_unix_ms: 1_002,
            errors: 0,
            applied_index_before: 100,
            applied_index_after: 104,
            physical_commit_entries: 4,
            commit_latency_p50_us: percentile_type7(&raw_commit_latency_us, 0.50).unwrap(),
            commit_latency_p95_us: percentile_type7(&raw_commit_latency_us, 0.95).unwrap(),
            commit_latency_p99_us: percentile_type7(&raw_commit_latency_us, 0.99).unwrap(),
            raw_commit_latency_us,
            applied_indexes: (1..=voter_count)
                .map(|node_id| NodeAppliedIndex {
                    node_id,
                    applied_index: 104,
                })
                .collect(),
            max_apply_lag_entries: 0,
            controller_host: "semantic-controller".to_owned(),
            load_generator_host: "controller-process".to_owned(),
            resources: (1..=voter_count)
                .map(|node_id| ResourceSample {
                    node_id,
                    hardware: "not-collected-semantic-ci".to_owned(),
                    storage_device: "not-collected-semantic-ci".to_owned(),
                    network_path: "loopback-semantic-ci".to_owned(),
                    cpu_seconds: None,
                    wall_seconds: None,
                    max_rss_bytes: None,
                    storage_read_bytes: None,
                    storage_write_bytes: None,
                    network_receive_bytes: None,
                    network_transmit_bytes: None,
                })
                .collect(),
        }
    }

    fn fixture() -> ClusterTopologyArtifact {
        let workload = TopologyWorkload {
            operations: 4,
            ..TopologyWorkload::semantic()
        };
        let workload_sha256 = workload.sha256().unwrap();
        ClusterTopologyArtifact {
            schema_version: TOPOLOGY_ARTIFACT_SCHEMA_VERSION,
            evidence_scope: SEMANTIC_EVIDENCE_SCOPE.to_owned(),
            build_sha: "a".repeat(40),
            workload,
            workload_sha256: workload_sha256.clone(),
            topology_order: vec![3, 4],
            started_at_unix_ms: 1_000,
            finished_at_unix_ms: 1_003,
            runs: vec![
                fixture_run(3, &workload_sha256),
                fixture_run(4, &workload_sha256),
            ],
        }
    }

    #[test]
    fn topology_artifact_uses_linear_type7_percentiles() {
        let samples = [7, 15, 36, 39, 40, 41];
        assert_eq!(percentile_type7(&samples, 0.50).unwrap(), 37.5);
        assert_eq!(percentile_type7(&samples, 0.95).unwrap(), 40.75);
        assert!(percentile_type7(&[], 0.50).is_err());
        assert!(percentile_type7(&samples, 1.01).is_err());
    }

    #[test]
    fn topology_artifact_accepts_only_comparable_three_and_four_voter_runs() {
        validate_topology_artifact(&fixture()).expect("complete semantic artifact");

        let mut wrong_hash = fixture();
        wrong_hash.runs[1].workload_sha256 = "b".repeat(64);
        assert!(validate_topology_artifact(&wrong_hash)
            .unwrap_err()
            .to_string()
            .contains("identical workload"));

        let mut wrong_commits = fixture();
        wrong_commits.runs[0].physical_commit_entries = 3;
        assert!(validate_topology_artifact(&wrong_commits)
            .unwrap_err()
            .to_string()
            .contains("one Raft entry"));

        let mut invented_resources = fixture();
        invented_resources.runs[0].resources[0].cpu_seconds = Some(0.0);
        assert!(validate_topology_artifact(&invented_resources)
            .unwrap_err()
            .to_string()
            .contains("must not claim"));
    }

    #[test]
    fn topology_artifact_schema_is_versioned_and_closed() {
        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../../benchmarks/cluster-topology.schema.json"
        ))
        .expect("parse checked-in topology schema");
        assert_eq!(
            schema.get("$id").and_then(serde_json::Value::as_str),
            Some("https://plurx.tv/schemas/cluster-topology-v1.json")
        );
        assert_eq!(
            schema
                .get("additionalProperties")
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
    }

    #[test]
    fn topology_artifact_order_is_explicit_and_counterbalanced() {
        assert_eq!(parse_topology_order(None).unwrap(), [3, 4]);
        assert_eq!(parse_topology_order(Some("4,3")).unwrap(), [4, 3]);
        assert!(parse_topology_order(Some("3")).is_err());
        assert!(parse_topology_order(Some("3,3")).is_err());
    }
}
