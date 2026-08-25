//! P0c's pinned four-machine topology campaign.
//!
//! The ordinary topology mode intentionally stays loopback-only semantic CI.
//! This module is the explicit hardware boundary: four named Linux hosts run
//! fresh containerized voters, while the controller remains an external load
//! generator. Raw pair artifacts keep the topology-v2 contract; this module
//! adds the pre-registered 3--7-pair stopping rule and an aggregate campaign.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::BufReader;
use tokio::process::Command;

use super::topology::{exercise_topology, unix_ms, ResourceIdentity, RunEvidence};
use super::{
    default_read_pool_size, validate_topology_artifact, ClusterProcesses, ClusterTopologyArtifact,
    NodeLaunch, NodeProcess, NodeSpec, TopologyRun, TopologyWorkload, CONVERGENCE_TIMEOUT,
    TOPOLOGY_ARTIFACT_SCHEMA_VERSION,
};

const NAMED_RUNNER_CONFIG_SCHEMA_VERSION: u32 = 1;
pub const NAMED_CAMPAIGN_SCHEMA_VERSION: u32 = 2;
pub const INSTRUMENTATION_CAMPAIGN_SCHEMA_VERSION: u32 = 1;
const INSTRUMENTATION_ARM_SCHEMA_VERSION: u32 = 1;
const NAMED_SCOPE: &str = "named_runner";
const LOCAL_IMAGE_REPOSITORY: &str = "plurx-cluster-check";
const SSH_CONNECT_TIMEOUT_SECS: &str = "10";
const REMOTE_ROOT_PREFIX: &str = "/var/tmp/plurx-cluster-named.";
const REMOTE_OWNER_PREFIX: &str = ".plurx-owner-";
const REMOTE_OWNER_LABEL: &str = "tv.plurx.named-owner";
const REMOTE_COMMAND_TIMEOUT: Duration = Duration::from_secs(45);
pub const CLEANUP_MANIFEST_FILENAME: &str = ".active-cleanup.json";
const OUTPUT_OWNER_FILENAME: &str = ".campaign-owner";
static ATOMIC_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const EMBEDDED_BUILD_SHA: Option<&str> = option_env!("PLURX_BUILD_SHA");
const P2F_MIN_PAIRS: u64 = 8;
const P2F_MAX_PAIRS: u64 = 16;
const P2F_CONFIDENCE_HALF_WIDTH_PERCENT: f64 = 1.0;

fn p2f_state_order(pair_index: u64) -> [bool; 2] {
    if pair_index % 2 == 1 {
        [false, true]
    } else {
        [true, false]
    }
}

fn p2f_topology_order(pair_index: u64) -> [u64; 2] {
    if ((pair_index - 1) / 2).is_multiple_of(2) {
        [3, 4]
    } else {
        [4, 3]
    }
}

fn p2f_stopping_checkpoint(pair_index: u64) -> bool {
    pair_index >= P2F_MIN_PAIRS && pair_index.is_multiple_of(4)
}

pub fn print_embedded_build_identity() -> Result<()> {
    let build_sha = EMBEDDED_BUILD_SHA.context("runner image omitted its embedded build SHA")?;
    if build_sha.len() != 40
        || !build_sha
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("runner image embedded an invalid build SHA");
    }
    println!("{build_sha}");
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedRunnerConfig {
    pub schema_version: u32,
    pub runner_id: String,
    pub image: String,
    pub controller_host: String,
    pub load_generator_host: String,
    pub load_generator_isolation: String,
    pub raft_port: u16,
    pub api_port: u16,
    pub min_pairs: u64,
    pub max_pairs: u64,
    pub confidence_half_width_percent: f64,
    /// Read-only pool each measured voter runs with. Defaulted so an existing
    /// runner config keeps working, and carried into every node launch so a
    /// 4/8/16 comparison measures three different pools rather than three runs
    /// of Hiqlite's default.
    #[serde(default = "default_read_pool_size")]
    pub read_pool_size: usize,
    pub voters: Vec<NamedVoter>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedVoter {
    pub node_id: u64,
    /// Stable evidence label. The SSH host and private address are runtime
    /// transport details and are never copied into committed artifacts.
    pub label: String,
    pub ssh_host: String,
    pub advertised_host: String,
    pub hardware: String,
    pub storage_device: String,
    pub network_path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedTopologyCampaign {
    pub schema_version: u32,
    pub evidence_scope: String,
    pub build_sha: String,
    pub runner_id: String,
    pub image: String,
    pub image_digest: String,
    pub native_architecture: String,
    pub controller_machine_fingerprint: String,
    pub voter_machine_fingerprints: Vec<String>,
    pub controller_host: String,
    pub load_generator_host: String,
    pub load_generator_isolation: String,
    pub voter_labels: Vec<String>,
    pub read_pool_size: usize,
    pub min_pairs: u64,
    pub max_pairs: u64,
    pub confidence_half_width_percent: f64,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub stop_reason: String,
    pub result: String,
    pub raw_pairs: Vec<RawPairReference>,
    pub metrics: Vec<PairedMetric>,
    pub budgets: CampaignBudgets,
}

/// P2f compares the same exact validation binary with only the validation-only
/// Store timer switch changed. Each arm is itself a complete, schema-validated
/// 3-voter/4-voter topology artifact, so the overhead report cannot replace
/// workload correctness with a synthetic timer microbenchmark.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedInstrumentationCampaign {
    pub schema_version: u32,
    pub evidence_scope: String,
    pub build_sha: String,
    pub runner_id: String,
    pub image: String,
    pub image_digest: String,
    pub native_architecture: String,
    pub controller_machine_fingerprint: String,
    pub voter_machine_fingerprints: Vec<String>,
    pub controller_host: String,
    pub load_generator_host: String,
    pub load_generator_isolation: String,
    pub voter_labels: Vec<String>,
    pub read_pool_size: usize,
    pub min_pairs: u64,
    pub max_pairs: u64,
    pub confidence_half_width_percent: f64,
    pub overhead_budget_percent: f64,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub stop_reason: String,
    pub result: String,
    pub raw_pairs: Vec<InstrumentationPairReference>,
    pub metrics: Vec<InstrumentationMetric>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstrumentationPairReference {
    pub pair_index: u64,
    pub state_order: Vec<String>,
    pub topology_order: Vec<u64>,
    pub control_artifact_path: String,
    pub control_artifact_sha256: String,
    pub instrumented_artifact_path: String,
    pub instrumented_artifact_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstrumentationMetric {
    pub metric: String,
    pub unit: String,
    pub control_values: Vec<f64>,
    pub instrumented_values: Vec<f64>,
    pub instrumented_over_control_ratios: Vec<f64>,
    pub median_control: f64,
    pub median_instrumented: f64,
    pub median_ratio: f64,
    pub geometric_mean_ratio: f64,
    pub ci95_lower_ratio: f64,
    pub ci95_upper_ratio: f64,
    pub ci95_half_width_percent: f64,
    pub precision_reached: bool,
    pub within_budget: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedInstrumentationArmArtifact {
    pub schema_version: u32,
    pub instrumentation_enabled: bool,
    pub topology_attestations: Vec<InstrumentationTopologyAttestation>,
    pub topology: ClusterTopologyArtifact,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstrumentationTopologyAttestation {
    pub voter_count: u64,
    pub nodes: Vec<InstrumentationNodeAttestation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstrumentationNodeAttestation {
    pub node_id: u64,
    pub instrumentation_enabled: bool,
    pub recorded_operations_total: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawPairReference {
    pub pair_index: u64,
    pub topology_order: Vec<u64>,
    pub artifact_path: String,
    pub artifact_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedMetric {
    pub metric: String,
    pub unit: String,
    pub three_voter_values: Vec<f64>,
    pub four_voter_values: Vec<f64>,
    pub four_over_three_ratios: Vec<f64>,
    pub median_three_voter: f64,
    pub median_four_voter: f64,
    pub median_ratio: f64,
    pub geometric_mean_ratio: f64,
    pub ci95_lower_ratio: f64,
    pub ci95_upper_ratio: f64,
    pub ci95_half_width_percent: f64,
    pub precision_reached: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignBudgets {
    pub additional_request_errors: u64,
    pub unaffected_store_p99_regression_percent: f64,
    pub instrumentation_cpu_and_wall_overhead_percent: f64,
    pub auth_activity_entry_reduction_percent: f64,
    pub authority_catalogue_read_reduction_percent: f64,
    pub browse_p99_regression_percent: f64,
    pub read_pool_write_p99_regression_percent: f64,
    pub read_pool_max_rss_regression_percent: f64,
}

impl Default for CampaignBudgets {
    fn default() -> Self {
        Self {
            additional_request_errors: 0,
            unaffected_store_p99_regression_percent: 10.0,
            instrumentation_cpu_and_wall_overhead_percent: 2.0,
            auth_activity_entry_reduction_percent: 95.0,
            authority_catalogue_read_reduction_percent: 70.0,
            browse_p99_regression_percent: 10.0,
            read_pool_write_p99_regression_percent: 10.0,
            read_pool_max_rss_regression_percent: 10.0,
        }
    }
}

pub async fn run_named_campaign(
    config_path: &Path,
    output_dir: &Path,
    source_root: &Path,
    owner_nonce: &str,
) -> Result<()> {
    let config: NamedRunnerConfig = serde_json::from_slice(
        &std::fs::read(config_path)
            .with_context(|| format!("read named-runner config {}", config_path.display()))?,
    )?;
    validate_config(&config)?;
    verify_output_claim(output_dir, owner_nonce)?;
    let cleanup_manifest = output_dir.join(CLEANUP_MANIFEST_FILENAME);
    let build_sha = verify_controller_source(source_root, None)?;
    if !image_is_pinned_to_sha(&config.image, &build_sha) {
        bail!(
            "named-runner image {} is not pinned to build {}",
            config.image,
            build_sha
        );
    }
    let deployment = verify_deployed_image(&config, &build_sha).await?;
    let image_digest = deployment.image_digest.clone();
    let workload = TopologyWorkload::semantic();
    let workload_sha256 = workload.sha256()?;
    let started_at_unix_ms = unix_ms()?;
    let mut raw_pairs = Vec::new();
    let mut artifacts = Vec::new();
    let mut metrics = Vec::new();
    let mut precision_reached = false;

    for pair_index in 1..=config.max_pairs {
        let order = if pair_index % 2 == 1 { [3, 4] } else { [4, 3] };
        let mut runs = Vec::with_capacity(2);
        for voter_count in order {
            println!("cluster-check: named pair {pair_index} fresh {voter_count}-voter topology");
            runs.push(
                run_remote_topology(RemoteTopologyRequest {
                    config: &config,
                    pair_index,
                    voter_count,
                    workload: &workload,
                    workload_sha256: &workload_sha256,
                    cleanup_manifest: &cleanup_manifest,
                    image_digest: &image_digest,
                    owner_nonce,
                    build_sha: &build_sha,
                    deployment: &deployment,
                    instrument_store_operations: true,
                })
                .await?
                .run,
            );
        }
        let artifact = ClusterTopologyArtifact {
            schema_version: TOPOLOGY_ARTIFACT_SCHEMA_VERSION,
            evidence_scope: NAMED_SCOPE.to_owned(),
            build_sha: build_sha.clone(),
            runner_image_digest: Some(image_digest.clone()),
            workload: workload.clone(),
            workload_sha256: workload_sha256.clone(),
            topology_order: order.to_vec(),
            started_at_unix_ms: runs
                .iter()
                .map(|run| run.started_at_unix_ms)
                .min()
                .context("named pair had no run start")?,
            finished_at_unix_ms: runs
                .iter()
                .map(|run| run.finished_at_unix_ms)
                .max()
                .context("named pair had no run finish")?,
            runs,
        };
        validate_topology_artifact(&artifact)?;
        let filename = format!("pair-{pair_index:02}.json");
        let path = output_dir.join(&filename);
        let bytes = pretty_json(&artifact)?;
        write_atomic(&path, &bytes)
            .with_context(|| format!("write named raw pair {}", path.display()))?;
        raw_pairs.push(RawPairReference {
            pair_index,
            topology_order: order.to_vec(),
            artifact_path: filename,
            artifact_sha256: hex::encode(Sha256::digest(&bytes)),
        });
        artifacts.push(artifact);

        if campaign_metrics_ready(artifacts.len()) {
            metrics = summarize_metrics(&artifacts, config.confidence_half_width_percent)?;
            precision_reached = pair_index >= config.min_pairs
                && metrics.iter().all(|metric| metric.precision_reached);
        }
        if precision_reached {
            break;
        }
    }

    let final_deployment = verify_deployed_image(&config, &build_sha).await?;
    if final_deployment != deployment {
        bail!("named-runner machine or image provenance changed during collection");
    }
    verify_controller_source(source_root, Some(&build_sha))?;

    let campaign = NamedTopologyCampaign {
        schema_version: NAMED_CAMPAIGN_SCHEMA_VERSION,
        evidence_scope: NAMED_SCOPE.to_owned(),
        build_sha,
        runner_id: config.runner_id.clone(),
        image: config.image.clone(),
        image_digest,
        native_architecture: deployment.native_architecture,
        controller_machine_fingerprint: deployment.controller_machine_fingerprint,
        voter_machine_fingerprints: deployment.voter_machine_fingerprints,
        controller_host: config.controller_host.clone(),
        load_generator_host: config.load_generator_host.clone(),
        load_generator_isolation: config.load_generator_isolation.clone(),
        voter_labels: config
            .voters
            .iter()
            .map(|voter| voter.label.clone())
            .collect(),
        read_pool_size: config.read_pool_size,
        min_pairs: config.min_pairs,
        max_pairs: config.max_pairs,
        confidence_half_width_percent: config.confidence_half_width_percent,
        started_at_unix_ms,
        finished_at_unix_ms: unix_ms()?,
        stop_reason: if precision_reached {
            "precision_reached".to_owned()
        } else {
            "max_pairs_reached".to_owned()
        },
        result: if precision_reached {
            "accepted".to_owned()
        } else {
            "inconclusive".to_owned()
        },
        raw_pairs,
        metrics,
        budgets: CampaignBudgets::default(),
    };
    validate_named_campaign(&campaign, Some(output_dir))?;
    let campaign_path = output_dir.join("campaign.json");
    write_atomic(&campaign_path, &pretty_json(&campaign)?)
        .with_context(|| format!("write named campaign {}", campaign_path.display()))?;
    release_output_claim(output_dir, owner_nonce)?;
    println!(
        "cluster-check: named campaign {} ({})",
        campaign_path.display(),
        campaign.result
    );
    Ok(())
}

/// Run P2f on the same four named machines and exact image as P0c. The state
/// order and the topology order are independently counterbalanced. A control
/// and instrumented arm each retain an ordinary topology-v2 artifact, which
/// keeps all workload, corpus, placement, and resource validation shared with
/// P0c while this wrapper evaluates only the Store instrumentation delta.
pub async fn run_named_instrumentation_campaign(
    config_path: &Path,
    output_dir: &Path,
    source_root: &Path,
    owner_nonce: &str,
) -> Result<()> {
    let config: NamedRunnerConfig = serde_json::from_slice(
        &std::fs::read(config_path)
            .with_context(|| format!("read named-runner config {}", config_path.display()))?,
    )?;
    validate_config(&config)?;
    verify_output_claim(output_dir, owner_nonce)?;
    let cleanup_manifest = output_dir.join(CLEANUP_MANIFEST_FILENAME);
    let build_sha = verify_controller_source(source_root, None)?;
    if !image_is_pinned_to_sha(&config.image, &build_sha) {
        bail!(
            "named-runner image {} is not pinned to build {}",
            config.image,
            build_sha
        );
    }
    let deployment = verify_deployed_image(&config, &build_sha).await?;
    let image_digest = deployment.image_digest.clone();
    let workload = TopologyWorkload::semantic();
    let workload_sha256 = workload.sha256()?;
    let started_at_unix_ms = unix_ms()?;
    let mut raw_pairs = Vec::new();
    let mut controls = Vec::new();
    let mut instrumented = Vec::new();
    let mut metrics = Vec::new();
    let mut precision_reached = false;

    for pair_index in 1..=P2F_MAX_PAIRS {
        let topology_order = p2f_topology_order(pair_index);
        let state_order = p2f_state_order(pair_index);
        let mut control = None;
        let mut measured = None;
        for instrumentation_enabled in state_order {
            let state = if instrumentation_enabled {
                "instrumented"
            } else {
                "control"
            };
            let mut runs = Vec::with_capacity(2);
            let mut topology_attestations = Vec::with_capacity(2);
            for voter_count in topology_order {
                println!(
                    "cluster-check: P2f pair {pair_index} {state} fresh {voter_count}-voter topology"
                );
                let outcome = run_remote_topology(RemoteTopologyRequest {
                    config: &config,
                    pair_index,
                    voter_count,
                    workload: &workload,
                    workload_sha256: &workload_sha256,
                    cleanup_manifest: &cleanup_manifest,
                    image_digest: &image_digest,
                    owner_nonce,
                    build_sha: &build_sha,
                    deployment: &deployment,
                    instrument_store_operations: instrumentation_enabled,
                })
                .await?;
                runs.push(outcome.run);
                topology_attestations.push(outcome.attestation);
            }
            let topology = ClusterTopologyArtifact {
                schema_version: TOPOLOGY_ARTIFACT_SCHEMA_VERSION,
                evidence_scope: NAMED_SCOPE.to_owned(),
                build_sha: build_sha.clone(),
                runner_image_digest: Some(image_digest.clone()),
                workload: workload.clone(),
                workload_sha256: workload_sha256.clone(),
                topology_order: topology_order.to_vec(),
                started_at_unix_ms: runs
                    .iter()
                    .map(|run| run.started_at_unix_ms)
                    .min()
                    .context("P2f arm had no run start")?,
                finished_at_unix_ms: runs
                    .iter()
                    .map(|run| run.finished_at_unix_ms)
                    .max()
                    .context("P2f arm had no run finish")?,
                runs,
            };
            validate_topology_artifact(&topology)?;
            let artifact = NamedInstrumentationArmArtifact {
                schema_version: INSTRUMENTATION_ARM_SCHEMA_VERSION,
                instrumentation_enabled,
                topology_attestations,
                topology,
            };
            validate_instrumentation_arm(&artifact, instrumentation_enabled)?;
            let filename = format!("pair-{pair_index:02}-{state}.json");
            let bytes = pretty_json(&artifact)?;
            write_atomic(&output_dir.join(&filename), &bytes)?;
            let hash = hex::encode(Sha256::digest(&bytes));
            if instrumentation_enabled {
                measured = Some((artifact, filename, hash));
            } else {
                control = Some((artifact, filename, hash));
            }
        }
        let (control_artifact, control_path, control_hash) =
            control.context("P2f pair omitted its control arm")?;
        let (instrumented_artifact, instrumented_path, instrumented_hash) =
            measured.context("P2f pair omitted its instrumented arm")?;
        controls.push(control_artifact.topology);
        instrumented.push(instrumented_artifact.topology);
        raw_pairs.push(InstrumentationPairReference {
            pair_index,
            state_order: state_order
                .into_iter()
                .map(|enabled| {
                    if enabled {
                        "instrumented".to_owned()
                    } else {
                        "control".to_owned()
                    }
                })
                .collect(),
            topology_order: topology_order.to_vec(),
            control_artifact_path: control_path,
            control_artifact_sha256: control_hash,
            instrumented_artifact_path: instrumented_path,
            instrumented_artifact_sha256: instrumented_hash,
        });
        if campaign_metrics_ready(controls.len()) {
            metrics = summarize_instrumentation_metrics(
                &controls,
                &instrumented,
                P2F_CONFIDENCE_HALF_WIDTH_PERCENT,
                CampaignBudgets::default().instrumentation_cpu_and_wall_overhead_percent,
            )?;
            precision_reached = p2f_stopping_checkpoint(pair_index)
                && metrics.iter().all(|metric| metric.precision_reached);
        }
        if precision_reached {
            break;
        }
    }

    let final_deployment = verify_deployed_image(&config, &build_sha).await?;
    if final_deployment != deployment {
        bail!("named-runner machine or image provenance changed during P2f collection");
    }
    verify_controller_source(source_root, Some(&build_sha))?;
    let within_budget = precision_reached && metrics.iter().all(|metric| metric.within_budget);
    let campaign = NamedInstrumentationCampaign {
        schema_version: INSTRUMENTATION_CAMPAIGN_SCHEMA_VERSION,
        evidence_scope: NAMED_SCOPE.to_owned(),
        build_sha,
        runner_id: config.runner_id.clone(),
        image: config.image.clone(),
        image_digest,
        native_architecture: deployment.native_architecture,
        controller_machine_fingerprint: deployment.controller_machine_fingerprint,
        voter_machine_fingerprints: deployment.voter_machine_fingerprints,
        controller_host: config.controller_host.clone(),
        load_generator_host: config.load_generator_host.clone(),
        load_generator_isolation: config.load_generator_isolation.clone(),
        voter_labels: config
            .voters
            .iter()
            .map(|voter| voter.label.clone())
            .collect(),
        read_pool_size: config.read_pool_size,
        min_pairs: P2F_MIN_PAIRS,
        max_pairs: P2F_MAX_PAIRS,
        confidence_half_width_percent: P2F_CONFIDENCE_HALF_WIDTH_PERCENT,
        overhead_budget_percent: CampaignBudgets::default()
            .instrumentation_cpu_and_wall_overhead_percent,
        started_at_unix_ms,
        finished_at_unix_ms: unix_ms()?,
        stop_reason: if precision_reached {
            "precision_reached".to_owned()
        } else {
            "max_pairs_reached".to_owned()
        },
        result: if within_budget {
            "accepted".to_owned()
        } else if precision_reached {
            "rejected".to_owned()
        } else {
            "inconclusive".to_owned()
        },
        raw_pairs,
        metrics,
    };
    validate_named_instrumentation_campaign(&campaign, Some(output_dir))?;
    let campaign_path = output_dir.join("instrumentation-campaign.json");
    write_atomic(&campaign_path, &pretty_json(&campaign)?)?;
    release_output_claim(output_dir, owner_nonce)?;
    println!(
        "cluster-check: named instrumentation campaign {} ({})",
        campaign_path.display(),
        campaign.result
    );
    Ok(())
}

fn campaign_metrics_ready(completed_pairs: usize) -> bool {
    completed_pairs >= 2
}

struct RemoteTopologyRequest<'a> {
    config: &'a NamedRunnerConfig,
    pair_index: u64,
    voter_count: u64,
    workload: &'a TopologyWorkload,
    workload_sha256: &'a str,
    cleanup_manifest: &'a Path,
    image_digest: &'a str,
    owner_nonce: &'a str,
    build_sha: &'a str,
    deployment: &'a DeploymentProof,
    instrument_store_operations: bool,
}

struct RemoteTopologyOutcome {
    run: TopologyRun,
    attestation: InstrumentationTopologyAttestation,
}

async fn run_remote_topology(request: RemoteTopologyRequest<'_>) -> Result<RemoteTopologyOutcome> {
    let RemoteTopologyRequest {
        config,
        pair_index,
        voter_count,
        workload,
        workload_sha256,
        cleanup_manifest,
        image_digest,
        owner_nonce,
        build_sha,
        deployment,
        instrument_store_operations,
    } = request;
    let selected = &config.voters[..usize::try_from(voter_count)?];
    let placement = verify_run_machine_placement(selected, build_sha, deployment).await?;
    let specs = selected
        .iter()
        .map(|voter| NodeSpec {
            id: voter.node_id,
            raft: format!("{}:{}", voter.advertised_host, config.raft_port),
            api: format!("{}:{}", voter.advertised_host, config.api_port),
        })
        .collect::<Vec<_>>();
    let nonce = remote_nonce()?;
    let cleanups = selected
        .iter()
        .map(|voter| RemoteCleanup {
            ssh_host: voter.ssh_host.clone(),
            container_name: format!(
                "plurx-named-{nonce}-p{pair_index}-v{voter_count}-n{}",
                voter.node_id
            ),
            remote_root: format!(
                "{REMOTE_ROOT_PREFIX}{nonce}-p{pair_index}-v{voter_count}-n{}",
                voter.node_id
            ),
            image_digest: image_digest.to_owned(),
            node_id: voter.node_id,
        })
        .collect::<Vec<_>>();
    write_cleanup_manifest(cleanup_manifest, owner_nonce, &cleanups)?;

    let mut claimed_cleanups = Vec::with_capacity(cleanups.len());
    for cleanup in &cleanups {
        if let Err(error) =
            ssh_output(&cleanup.ssh_host, &["mkdir", "--", &cleanup.remote_root]).await
        {
            return Err(with_cleanup_error(
                error,
                finish_remote_cleanup(&cleanups, &claimed_cleanups, cleanup_manifest, owner_nonce)
                    .await,
            ));
        }
        let owner_marker = remote_owner_marker(cleanup, owner_nonce)?;
        if let Err(error) = ssh_output(&cleanup.ssh_host, &["mkdir", "--", &owner_marker]).await {
            let release = ssh_output(&cleanup.ssh_host, &["rmdir", "--", &cleanup.remote_root])
                .await
                .map(|_| ());
            let cleanup =
                finish_remote_cleanup(&cleanups, &claimed_cleanups, cleanup_manifest, owner_nonce)
                    .await;
            let error = with_cleanup_error(error, release);
            return Err(with_cleanup_error(error, cleanup));
        }
        claimed_cleanups.push(cleanup.clone());
    }

    let mut nodes = Vec::with_capacity(selected.len());
    for (voter, cleanup) in selected.iter().zip(&cleanups) {
        let launch = NodeLaunch {
            listen_addr: "0.0.0.0".to_owned(),
            read_pool_size: config.read_pool_size,
            instrument_store_operations,
            ..NodeLaunch::voter(voter.node_id, PathBuf::from("/data"), specs.clone())
        };
        match spawn_remote_node(
            voter,
            config,
            &cleanup.container_name,
            &cleanup.remote_root,
            image_digest,
            owner_nonce,
            &launch,
        ) {
            Ok(process) => nodes.push(Some(process)),
            Err(error) => {
                drop(nodes);
                return Err(with_cleanup_error(
                    error,
                    finish_remote_cleanup(
                        &cleanups,
                        &claimed_cleanups,
                        cleanup_manifest,
                        owner_nonce,
                    )
                    .await,
                ));
            }
        }
    }

    let mut cluster = ClusterProcesses {
        nodes,
        root: PathBuf::from("/data"),
        convergence_timeout: CONVERGENCE_TIMEOUT,
    };
    let result = async {
        for node_id in 1..=voter_count {
            cluster.node_mut(node_id)?.wait_ready().await?;
        }
        for node_id in 1..=voter_count {
            match cluster
                .request(node_id, super::Request::StoreInstrumentationStatus)
                .await?
            {
                super::Response::StoreInstrumentationStatus {
                    enabled,
                    recorded_operations_total: 0,
                } if enabled == instrument_store_operations => {}
                response => bail!(
                    "named voter {node_id} did not attest a clean store instrumentation={instrument_store_operations} start: {response:?}"
                ),
            }
        }
        let identities = selected
            .iter()
            .map(|voter| ResourceIdentity {
                node_id: voter.node_id,
                hardware: voter.hardware.clone(),
                storage_device: voter.storage_device.clone(),
                network_path: voter.network_path.clone(),
            })
            .collect::<Vec<_>>();
        let run = exercise_topology(
            &mut cluster,
            voter_count,
            workload,
            workload_sha256,
            RunEvidence {
                read_pool_size: config.read_pool_size,
                controller_host: &config.controller_host,
                load_generator_host: &config.load_generator_host,
                load_generator_isolation: Some(&config.load_generator_isolation),
                controller_machine_fingerprint: Some(&placement.controller),
                voter_machine_fingerprints: Some(&placement.voters),
                resources: Some(&identities),
            },
        )
        .await?;
        let mut nodes = Vec::with_capacity(usize::try_from(voter_count)?);
        for node_id in 1..=voter_count {
            match cluster
                .request(node_id, super::Request::StoreInstrumentationStatus)
                .await?
            {
                super::Response::StoreInstrumentationStatus {
                    enabled,
                    recorded_operations_total,
                } if enabled == instrument_store_operations
                    && ((!enabled && recorded_operations_total == 0)
                        || (enabled && recorded_operations_total > 0)) =>
                {
                    nodes.push(InstrumentationNodeAttestation {
                        node_id,
                        instrumentation_enabled: enabled,
                        recorded_operations_total,
                    });
                }
                response => bail!(
                    "named voter {node_id} recorder behavior contradicted store instrumentation={instrument_store_operations}: {response:?}"
                ),
            }
        }
        Ok(RemoteTopologyOutcome {
            run,
            attestation: InstrumentationTopologyAttestation { voter_count, nodes },
        })
    }
    .await;

    let shutdown = if result.is_ok() {
        cluster.shutdown_all().await
    } else {
        cluster.kill_all().await;
        Ok(())
    };
    let cleanup =
        finish_remote_cleanup(&cleanups, &claimed_cleanups, cleanup_manifest, owner_nonce).await;
    let mut errors = Vec::new();
    let run = match result {
        Ok(run) => Some(run),
        Err(error) => {
            errors.push(format!("topology: {error:#}"));
            None
        }
    };
    if let Err(error) = shutdown {
        errors.push(format!("shutdown: {error:#}"));
    }
    if let Err(error) = cleanup {
        errors.push(format!("cleanup: {error:#}"));
    }
    if errors.is_empty() {
        run.context("named topology completed without a result")
    } else {
        bail!("named topology failed: {}", errors.join("; "))
    }
}

fn spawn_remote_node(
    voter: &NamedVoter,
    config: &NamedRunnerConfig,
    container_name: &str,
    remote_root: &str,
    image_digest: &str,
    owner_nonce: &str,
    launch: &NodeLaunch,
) -> Result<NodeProcess> {
    validate_owner_nonce(owner_nonce)?;
    let launch_hex = hex::encode(serde_json::to_vec(launch)?);
    let mut command = Command::new("ssh");
    command
        .args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "LogLevel=ERROR",
            "-o",
            &format!("ConnectTimeout={SSH_CONNECT_TIMEOUT_SECS}"),
            "--",
            &voter.ssh_host,
            "docker",
            "run",
            "--rm",
            "--pull=never",
            "-i",
            "--name",
            container_name,
            "--label",
            &format!("{REMOTE_OWNER_LABEL}={owner_nonce}"),
            "-v",
            &format!("{remote_root}:/data"),
            "-p",
            &format!(
                "{}:{}:{}",
                voter.advertised_host, config.raft_port, config.raft_port
            ),
            "-p",
            &format!(
                "{}:{}:{}",
                voter.advertised_host, config.api_port, config.api_port
            ),
            image_digest,
            "node-hex",
            &launch_hex,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .with_context(|| format!("spawn named voter {} on {}", voter.node_id, voter.ssh_host))?;
    let input = child.stdin.take().context("named voter stdin")?;
    let output = BufReader::new(child.stdout.take().context("named voter stdout")?);
    Ok(NodeProcess {
        id: voter.node_id,
        child,
        input,
        output,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteCleanup {
    ssh_host: String,
    container_name: String,
    remote_root: String,
    image_digest: String,
    node_id: u64,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CleanupManifest {
    schema_version: u32,
    owner_nonce: String,
    cleanups: Vec<RemoteCleanup>,
}

struct OpenedIdentity {
    file: std::fs::File,
    device: u64,
    inode: u64,
    mode: u32,
}

impl OpenedIdentity {
    fn open_private_file_at(
        directory: &std::fs::File,
        filename: &str,
        label: &str,
    ) -> Result<Self> {
        let filename = std::ffi::CString::new(filename)?;
        // SAFETY: the retained descriptor is an opened directory, the filename
        // is a fixed NUL-free basename, and a successful fd is immediately
        // transferred into one owned File.
        let descriptor = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                filename.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("open retained {label} without following links"));
        }
        // SAFETY: `openat` returned a new owned descriptor and this is its only
        // conversion into a File.
        let file = unsafe { std::fs::File::from_raw_fd(descriptor) };
        let metadata = file.metadata()?;
        if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o777 != 0o600 {
            bail!("{label} must be a regular mode-0600 file");
        }
        Ok(Self {
            file,
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.permissions().mode() & 0o777,
        })
    }

    fn open_directory(path: &Path) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_DIRECTORY)
            .open(path)
            .with_context(|| {
                format!(
                    "open cleanup manifest parent without following links {}",
                    path.display()
                )
            })?;
        let metadata = file.metadata()?;
        if !metadata.file_type().is_dir() {
            bail!("cleanup manifest parent is not a real directory");
        }
        Ok(Self {
            file,
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.permissions().mode() & 0o777,
        })
    }

    fn verify_private_entry_at(
        &self,
        directory: &std::fs::File,
        filename: &str,
        label: &str,
    ) -> Result<()> {
        let current = Self::open_private_file_at(directory, filename, label)?;
        if current.device != self.device || current.inode != self.inode || current.mode != self.mode
        {
            bail!("{label} identity changed while cleanup was in progress");
        }
        Ok(())
    }

    fn read_all(&mut self, label: &str) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.file
            .read_to_end(&mut bytes)
            .with_context(|| format!("read retained {label}"))?;
        Ok(bytes)
    }
}

struct CleanupAuthority {
    output_dir: PathBuf,
    directory: OpenedIdentity,
    manifest: OpenedIdentity,
    owner: OpenedIdentity,
    manifest_bytes: Vec<u8>,
    owner_nonce: String,
}

impl CleanupAuthority {
    fn open(path: &Path) -> Result<Self> {
        if path.file_name().and_then(|value| value.to_str()) != Some(CLEANUP_MANIFEST_FILENAME) {
            bail!("cleanup authority requires the exact active-manifest filename");
        }
        let supplied_output_dir = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .context("cleanup manifest has no claimed output directory")?
            .to_path_buf();
        if supplied_output_dir
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            bail!("cleanup manifest parent must not contain traversal");
        }
        let supplied_metadata = std::fs::symlink_metadata(&supplied_output_dir)?;
        if !supplied_metadata.file_type().is_dir() || supplied_metadata.file_type().is_symlink() {
            bail!("cleanup manifest parent must be a real directory");
        }
        let output_dir = std::fs::canonicalize(&supplied_output_dir).with_context(|| {
            format!(
                "resolve cleanup manifest parent {}",
                supplied_output_dir.display()
            )
        })?;
        let directory = OpenedIdentity::open_directory(&output_dir)?;
        if supplied_metadata.dev() != directory.device || supplied_metadata.ino() != directory.inode
        {
            bail!("cleanup manifest parent changed while authority was opened");
        }
        let mut manifest = OpenedIdentity::open_private_file_at(
            &directory.file,
            CLEANUP_MANIFEST_FILENAME,
            "cleanup manifest",
        )?;
        let mut owner = OpenedIdentity::open_private_file_at(
            &directory.file,
            OUTPUT_OWNER_FILENAME,
            "output owner marker",
        )?;
        let manifest_bytes = manifest.read_all("cleanup manifest")?;
        let owner_nonce = String::from_utf8(owner.read_all("output owner marker")?)?
            .trim()
            .to_owned();
        validate_owner_nonce(&owner_nonce)?;
        Ok(Self {
            output_dir,
            directory,
            manifest,
            owner,
            manifest_bytes,
            owner_nonce,
        })
    }

    fn remove_manifest(self) -> Result<()> {
        self.owner.verify_private_entry_at(
            &self.directory.file,
            OUTPUT_OWNER_FILENAME,
            "output owner marker",
        )?;
        self.manifest.verify_private_entry_at(
            &self.directory.file,
            CLEANUP_MANIFEST_FILENAME,
            "cleanup manifest",
        )?;
        let filename = std::ffi::CString::new(CLEANUP_MANIFEST_FILENAME)?;
        // SAFETY: the retained descriptor is the validated authority
        // directory and `filename` is the fixed manifest basename.
        if unsafe { libc::unlinkat(self.directory.file.as_raw_fd(), filename.as_ptr(), 0) } != 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!(
                    "remove retained cleanup manifest from {}",
                    self.output_dir.display()
                )
            });
        }
        self.directory.file.sync_all().with_context(|| {
            format!(
                "sync cleanup output directory {}",
                self.output_dir.display()
            )
        })?;
        Ok(())
    }
}

fn remote_nonce() -> Result<String> {
    let mut bytes = [0_u8; 32];
    std::fs::File::open("/dev/urandom")
        .context("open controller randomness for remote-root nonce")?
        .read_exact(&mut bytes)
        .context("read controller randomness for remote-root nonce")?;
    Ok(hex::encode(bytes))
}

pub fn claim_named_output(path: &Path, owner_nonce: &str) -> Result<()> {
    validate_owner_nonce(owner_nonce)?;
    std::fs::create_dir(path).with_context(|| {
        format!(
            "atomically claim absent named-runner output directory {}",
            path.display()
        )
    })?;
    if let Err(error) = write_atomic_with_mode(
        &path.join(OUTPUT_OWNER_FILENAME),
        format!("{owner_nonce}\n").as_bytes(),
        0o600,
    ) {
        let _ = std::fs::remove_dir(path);
        return Err(error).context("publish named-runner output ownership");
    }
    Ok(())
}

fn verify_output_claim(path: &Path, owner_nonce: &str) -> Result<()> {
    validate_owner_nonce(owner_nonce)?;
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect named-runner output {}", path.display()))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        bail!("named-runner output claim is not a real directory");
    }
    let canonical = std::fs::canonicalize(path)?;
    let directory = OpenedIdentity::open_directory(&canonical)?;
    if metadata.dev() != directory.device || metadata.ino() != directory.inode {
        bail!("named-runner output claim changed while it was opened");
    }
    let mut owner = OpenedIdentity::open_private_file_at(
        &directory.file,
        OUTPUT_OWNER_FILENAME,
        "output owner marker",
    )?;
    if String::from_utf8(owner.read_all("output owner marker")?)?.trim() != owner_nonce {
        bail!("named-runner output ownership does not match this invocation");
    }
    let entries = std::fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
    if entries.len() != 1 || entries[0].file_name().to_string_lossy() != OUTPUT_OWNER_FILENAME {
        bail!("claimed named-runner output must contain only its owner marker");
    }
    Ok(())
}

fn release_output_claim(path: &Path, owner_nonce: &str) -> Result<()> {
    let owner_path = path.join(OUTPUT_OWNER_FILENAME);
    if std::fs::read_to_string(&owner_path)?.trim() != owner_nonce {
        bail!("refuse to release a different named-runner output owner");
    }
    std::fs::remove_file(&owner_path).context("remove named-runner owner marker")?;
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

fn validate_owner_nonce(owner_nonce: &str) -> Result<()> {
    if owner_nonce.len() != 64
        || !owner_nonce
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("named-runner owner nonce must be 256-bit lowercase hexadecimal");
    }
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    write_atomic_with_mode(path, bytes, 0o644)
}

fn write_atomic_with_mode(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("named-runner artifact path has no UTF-8 filename")?;
    let sequence = ATOMIC_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary =
        path.with_file_name(format!(".{filename}.tmp-{}-{sequence}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temporary)
        .with_context(|| format!("create temporary artifact {}", temporary.display()))?;
    file.set_permissions(std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("set exact artifact mode on {}", temporary.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    if let Err(error) = std::fs::hard_link(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            bail!("refuse to replace named-runner artifact {}", path.display());
        }
        return Err(error).with_context(|| {
            format!(
                "atomically publish named-runner artifact {} without replacement",
                path.display()
            )
        });
    }
    std::fs::remove_file(&temporary)
        .with_context(|| format!("remove temporary artifact {}", temporary.display()))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::File::open(parent)
        .with_context(|| format!("open artifact directory {}", parent.display()))?
        .sync_all()
        .with_context(|| format!("sync artifact directory {}", parent.display()))?;
    Ok(())
}

fn validate_cleanup(cleanup: &RemoteCleanup) -> Result<()> {
    if cleanup.node_id == 0
        || cleanup.node_id > 4
        || !safe_host_token(&cleanup.ssh_host)
        || !cleanup.container_name.starts_with("plurx-named-")
        || !cleanup
            .container_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        bail!("cleanup manifest contains an unsafe remote identity");
    }
    validate_image_digest(&cleanup.image_digest)?;
    let suffix = cleanup
        .remote_root
        .strip_prefix(REMOTE_ROOT_PREFIX)
        .context("cleanup manifest contains an out-of-scope remote root")?;
    if suffix.len() < 72
        || suffix.contains('/')
        || suffix.contains("..")
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        || !suffix
            .bytes()
            .take(64)
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("cleanup manifest contains an unsafe remote-root basename");
    }
    if cleanup.container_name != format!("plurx-named-{suffix}") {
        bail!("cleanup container is not bound to its nonce-owned remote root");
    }
    Ok(())
}

fn write_cleanup_manifest(
    path: &Path,
    owner_nonce: &str,
    cleanups: &[RemoteCleanup],
) -> Result<()> {
    validate_owner_nonce(owner_nonce)?;
    for cleanup in cleanups {
        validate_cleanup(cleanup)?;
    }
    write_atomic_with_mode(
        path,
        &pretty_json(&CleanupManifest {
            schema_version: 1,
            owner_nonce: owner_nonce.to_owned(),
            cleanups: cleanups.to_vec(),
        })?,
        0o600,
    )
}

pub async fn cleanup_named_manifest(path: &Path) -> Result<()> {
    let authority = CleanupAuthority::open(path)?;
    let manifest: CleanupManifest = serde_json::from_slice(&authority.manifest_bytes)?;
    if manifest.schema_version != 1 || manifest.cleanups.is_empty() || manifest.cleanups.len() > 4 {
        bail!("invalid named-runner cleanup manifest shape");
    }
    validate_owner_nonce(&manifest.owner_nonce)?;
    if authority.owner_nonce != manifest.owner_nonce {
        bail!("cleanup manifest does not belong to the active output owner");
    }
    for cleanup in &manifest.cleanups {
        validate_cleanup(cleanup)?;
    }
    cleanup_remote(&manifest.cleanups, &manifest.owner_nonce).await?;
    authority.remove_manifest()
}

async fn finish_remote_cleanup(
    retained_cleanups: &[RemoteCleanup],
    active_cleanups: &[RemoteCleanup],
    manifest: &Path,
    owner_nonce: &str,
) -> Result<()> {
    let authority = CleanupAuthority::open(manifest)?;
    let retained: CleanupManifest = serde_json::from_slice(&authority.manifest_bytes)?;
    if retained.schema_version != 1
        || retained.owner_nonce != owner_nonce
        || authority.owner_nonce != owner_nonce
        || retained.cleanups != retained_cleanups
        || active_cleanups
            .iter()
            .any(|cleanup| !retained.cleanups.contains(cleanup))
    {
        bail!("cleanup manifest authority changed before remote cleanup");
    }
    cleanup_remote(active_cleanups, owner_nonce).await?;
    authority.remove_manifest()
}

fn remote_owner_marker(cleanup: &RemoteCleanup, owner_nonce: &str) -> Result<String> {
    validate_owner_nonce(owner_nonce)?;
    Ok(format!(
        "{}{}{}",
        cleanup.remote_root, REMOTE_OWNER_PREFIX, owner_nonce
    ))
}

async fn cleanup_remote(cleanups: &[RemoteCleanup], owner_nonce: &str) -> Result<()> {
    let mut errors = Vec::new();
    for cleanup in cleanups {
        let owner_marker = remote_owner_marker(cleanup, owner_nonce)?;
        let exists =
            match ssh_command(&cleanup.ssh_host, &["test", "-d", &cleanup.remote_root]).await {
                Ok(output) if output.status.success() => true,
                Ok(output) if output.status.code() == Some(1) => false,
                Ok(output) => {
                    errors.push(format!(
                        "inspect cleanup root on {} exited {}: {}",
                        cleanup.ssh_host,
                        output.status,
                        String::from_utf8_lossy(&output.stderr).trim()
                    ));
                    continue;
                }
                Err(error) => {
                    errors.push(format!("{error:#}"));
                    continue;
                }
            };
        if !exists {
            match ssh_command(&cleanup.ssh_host, &["test", "-d", &owner_marker]).await {
                Ok(output) if output.status.success() => {
                    if let Err(error) =
                        ssh_output(&cleanup.ssh_host, &["rmdir", "--", &owner_marker]).await
                    {
                        errors.push(format!("{error:#}"));
                    }
                }
                Ok(output) if output.status.code() == Some(1) => {}
                Ok(output) => errors.push(format!(
                    "inspect orphaned cleanup owner on {} exited {}: {}",
                    cleanup.ssh_host,
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                )),
                Err(error) => errors.push(format!("{error:#}")),
            }
            continue;
        }
        match ssh_command(&cleanup.ssh_host, &["test", "-d", &owner_marker]).await {
            Ok(output) if output.status.success() => {}
            Ok(output) if output.status.code() == Some(1) => {
                errors.push(format!(
                    "refuse cleanup of unowned remote root {} on {}",
                    cleanup.remote_root, cleanup.ssh_host
                ));
                continue;
            }
            Ok(output) => {
                errors.push(format!(
                    "inspect cleanup owner on {} exited {}: {}",
                    cleanup.ssh_host,
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
                continue;
            }
            Err(error) => {
                errors.push(format!("{error:#}"));
                continue;
            }
        }
        let container = ssh_command(
            &cleanup.ssh_host,
            &["docker", "container", "inspect", &cleanup.container_name],
        )
        .await;
        match container {
            Ok(output) if output.status.success() => {
                match container_owner_from_inspect(&output.stdout) {
                    Ok(owner) if owner == owner_nonce => {
                        if let Err(error) = ssh_output(
                            &cleanup.ssh_host,
                            &["docker", "rm", "-f", &cleanup.container_name],
                        )
                        .await
                        {
                            errors.push(format!("{error:#}"));
                            continue;
                        }
                    }
                    Ok(_) | Err(_) => {
                        errors.push(format!(
                            "refuse cleanup of unowned container {} on {}",
                            cleanup.container_name, cleanup.ssh_host
                        ));
                        continue;
                    }
                }
            }
            Ok(output)
                if String::from_utf8_lossy(&output.stderr)
                    .to_ascii_lowercase()
                    .contains("no such") => {}
            Ok(output) => {
                errors.push(format!(
                    "inspect cleanup container on {} exited {}: {}",
                    cleanup.ssh_host,
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
                continue;
            }
            Err(error) => {
                errors.push(format!("{error:#}"));
                continue;
            }
        }
        if let Err(error) = ssh_output(
            &cleanup.ssh_host,
            &[
                "docker",
                "run",
                "--rm",
                "--pull=never",
                "-v",
                &format!("{}:/data", cleanup.remote_root),
                "--entrypoint",
                "/bin/rm",
                &cleanup.image_digest,
                "-rf",
                "--",
                &format!("/data/node-{}", cleanup.node_id),
            ],
        )
        .await
        {
            errors.push(format!("{error:#}"));
            continue;
        }
        if let Err(error) =
            ssh_output(&cleanup.ssh_host, &["rmdir", "--", &cleanup.remote_root]).await
        {
            let text = format!("{error:#}");
            if !text.contains("No such file or directory") {
                errors.push(text);
                continue;
            }
        }
        if let Err(error) = ssh_output(&cleanup.ssh_host, &["rmdir", "--", &owner_marker]).await {
            let text = format!("{error:#}");
            if !text.contains("No such file or directory") {
                errors.push(text);
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        bail!("named-runner cleanup failed: {}", errors.join("; "))
    }
}

fn container_owner_from_inspect(output: &[u8]) -> Result<String> {
    let containers: serde_json::Value = serde_json::from_slice(output)?;
    containers
        .as_array()
        .filter(|containers| containers.len() == 1)
        .and_then(|containers| containers.first())
        .and_then(|container| container.pointer(&format!("/Config/Labels/{REMOTE_OWNER_LABEL}")))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .context("container inspection omitted the named-runner owner label")
}

fn with_cleanup_error(primary: anyhow::Error, cleanup: Result<()>) -> anyhow::Error {
    match cleanup {
        Ok(()) => primary,
        Err(cleanup) => anyhow!("{primary:#}; cleanup: {cleanup:#}"),
    }
}

async fn ssh_output(host: &str, remote: &[&str]) -> Result<String> {
    let output = ssh_command(host, remote).await?;
    if !output.status.success() {
        bail!(
            "named-runner command on {host} exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("named-runner command output was not UTF-8")
}

async fn ssh_command(host: &str, remote: &[&str]) -> Result<std::process::Output> {
    let mut command = Command::new("ssh");
    command
        .args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "LogLevel=ERROR",
            "-o",
            &format!("ConnectTimeout={SSH_CONNECT_TIMEOUT_SECS}"),
            "--",
            host,
        ])
        .args(remote)
        .kill_on_drop(true);
    let output = tokio::time::timeout(REMOTE_COMMAND_TIMEOUT, command.output())
        .await
        .with_context(|| format!("named-runner command on {host} timed out"))?
        .with_context(|| format!("run named-runner command on {host}"))?;
    Ok(output)
}

fn validate_config(config: &NamedRunnerConfig) -> Result<()> {
    if config.schema_version != NAMED_RUNNER_CONFIG_SCHEMA_VERSION {
        bail!("unsupported named-runner config schema version");
    }
    if config.voters.len() != 4
        || config
            .voters
            .iter()
            .map(|voter| voter.node_id)
            .collect::<Vec<_>>()
            != [1, 2, 3, 4]
    {
        bail!("named runner must declare exactly voters 1,2,3,4 in order");
    }
    if config
        .voters
        .iter()
        .map(|voter| &voter.label)
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != 4
    {
        bail!("named runner voter labels must be distinct");
    }
    for (field, values) in [
        (
            "SSH hosts",
            config
                .voters
                .iter()
                .map(|voter| &voter.ssh_host)
                .collect::<std::collections::BTreeSet<_>>(),
        ),
        (
            "advertised hosts",
            config
                .voters
                .iter()
                .map(|voter| &voter.advertised_host)
                .collect::<std::collections::BTreeSet<_>>(),
        ),
    ] {
        if values.len() != 4 {
            bail!("named runner {field} must identify four distinct machines");
        }
    }
    if config.raft_port < 49_152 || config.api_port < 49_152 || config.raft_port == config.api_port
    {
        bail!("named runner requires two distinct dynamic/private ports (49152--65535)");
    }
    if config.min_pairs != 3 || config.max_pairs != 7 {
        bail!("named runner must use the pre-registered 3--7 pair rule");
    }
    // The same range plurxd enforces for `cluster.read_pool_size`: a campaign
    // must not measure a pool the daemon would refuse to start with.
    if !(1..=16).contains(&config.read_pool_size) {
        bail!("named runner read_pool_size must be between 1 and 16");
    }
    if (config.confidence_half_width_percent - 5.0).abs() > f64::EPSILON {
        bail!("named runner confidence half-width target must be 5 percent");
    }
    let transport_values = config
        .voters
        .iter()
        .flat_map(|configured| {
            [
                configured.ssh_host.as_str(),
                configured.advertised_host.as_str(),
            ]
        })
        .collect::<Vec<_>>();
    for (field, value) in [
        ("runner_id", config.runner_id.as_str()),
        ("controller_host", config.controller_host.as_str()),
        ("load_generator_host", config.load_generator_host.as_str()),
        (
            "load_generator_isolation",
            config.load_generator_isolation.as_str(),
        ),
    ] {
        validate_public_evidence(field, value, &transport_values)?;
    }
    if config.controller_host != config.load_generator_host {
        bail!(
            "named workload runs in the external controller process, so controller and load-generator labels must match"
        );
    }
    for voter in &config.voters {
        for value in [&voter.ssh_host, &voter.advertised_host] {
            if !safe_host_token(value) {
                bail!("named voter {} has an unsafe host token", voter.node_id);
            }
        }
        for (field, value) in [
            ("voter label", voter.label.as_str()),
            ("hardware class", voter.hardware.as_str()),
            ("storage class", voter.storage_device.as_str()),
            ("network class", voter.network_path.as_str()),
        ] {
            validate_public_evidence(field, value, &transport_values)?;
        }
    }
    if !safe_image_reference(&config.image) {
        bail!("named-runner image must be a safe OCI name with a full-SHA tag");
    }
    Ok(())
}

fn safe_host_token(value: &str) -> bool {
    value.len() <= 253
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && value
            .bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        && !value.contains("..")
}

fn validate_public_evidence(field: &str, value: &str, transport_values: &[&str]) -> Result<()> {
    let trimmed = value.trim();
    if value != trimmed
        || value.is_empty()
        || value.len() > 256
        || !value.is_ascii()
        || value.bytes().any(|byte| byte.is_ascii_control())
        || value.contains("://")
        || value
            .chars()
            .any(|character| matches!(character, '/' | '\\' | '@' | ':'))
        || value.parse::<std::net::IpAddr>().is_ok()
        || value
            .split(|character: char| !(character.is_ascii_digit() || character == '.'))
            .any(|token| token.parse::<std::net::Ipv4Addr>().is_ok())
    {
        bail!("named-runner {field} is not a bounded commit-safe label");
    }
    let lower = value.to_ascii_lowercase();
    if [
        "serial",
        "machine-id",
        "machine_id",
        "token",
        "secret",
        "private key",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        bail!("named-runner {field} contains identity or credential-shaped material");
    }
    if transport_values.iter().any(|transport| {
        let transport = transport.trim().to_ascii_lowercase();
        !transport.is_empty()
            && (lower == transport || (transport.len() >= 4 && lower.contains(&transport)))
    }) {
        bail!("named-runner {field} exposes a runtime transport identity");
    }
    if value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|token| {
            token.len() >= 24
                && (token.bytes().all(|byte| byte.is_ascii_hexdigit())
                    || (token.len() >= 32
                        && token
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))))
        })
    {
        bail!("named-runner {field} contains token-shaped material");
    }
    Ok(())
}

fn verify_controller_source(source_root: &Path, expected_sha: Option<&str>) -> Result<String> {
    let source_root = std::fs::canonicalize(source_root)
        .with_context(|| format!("resolve controller source root {}", source_root.display()))?;
    let top_level = git_output(&source_root, &["rev-parse", "--show-toplevel"])?;
    let top_level = std::fs::canonicalize(top_level.trim())
        .context("resolve controller Git top-level directory")?;
    if top_level != source_root {
        bail!("named-runner source root must be the Git top-level directory");
    }
    let build_sha = git_output(&source_root, &["rev-parse", "HEAD"])?
        .trim()
        .to_owned();
    if build_sha.len() != 40
        || !build_sha
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("named-runner controller HEAD is not a full lowercase Git SHA");
    }
    if expected_sha.is_some_and(|expected| expected != build_sha) {
        bail!("named-runner controller HEAD changed during collection");
    }
    let status = git_output(
        &source_root,
        &["status", "--porcelain=v1", "--untracked-files=normal"],
    )?;
    if !status.is_empty() {
        bail!("named-runner controller source must remain clean during collection");
    }
    Ok(build_sha)
}

fn git_output(source_root: &Path, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(source_root)
        .args(args)
        .output()
        .with_context(|| format!("run git {} for named runner", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed for named runner: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("named-runner Git output was not UTF-8")
}

fn safe_image_reference(value: &str) -> bool {
    let Some((repository, tag)) = value.rsplit_once(':') else {
        return false;
    };
    repository == LOCAL_IMAGE_REPOSITORY
        && tag.len() == 40
        && tag
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn image_is_pinned_to_sha(image: &str, build_sha: &str) -> bool {
    image
        .rsplit_once(':')
        .is_some_and(|(_, tag)| tag == build_sha)
}

#[derive(Debug, PartialEq, Eq)]
struct DeploymentProof {
    image_digest: String,
    native_architecture: String,
    controller_machine_fingerprint: String,
    voter_machine_fingerprints: Vec<String>,
}

struct RunMachinePlacement {
    controller: String,
    voters: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct InspectedImage {
    digest: String,
    architecture: String,
    operating_system: String,
    revision: String,
}

fn parse_inspected_image(output: &str) -> Result<InspectedImage> {
    let images: serde_json::Value =
        serde_json::from_str(output).context("decode Docker image inspection")?;
    let image = images
        .as_array()
        .filter(|images| images.len() == 1)
        .and_then(|images| images.first())
        .context("Docker image inspection must contain exactly one image")?;
    let string_field = |field: &str| {
        image
            .get(field)
            .and_then(serde_json::Value::as_str)
            .with_context(|| format!("image inspection omitted {field}"))
    };
    let revision = image
        .pointer("/Config/Labels/org.opencontainers.image.revision")
        .and_then(serde_json::Value::as_str)
        .context("image inspection omitted its OCI revision label")?;
    Ok(InspectedImage {
        digest: string_field("Id")?.to_owned(),
        architecture: string_field("Architecture")?.to_owned(),
        operating_system: string_field("Os")?.to_owned(),
        revision: revision.to_owned(),
    })
}

async fn verify_run_machine_placement(
    selected: &[NamedVoter],
    build_sha: &str,
    deployment: &DeploymentProof,
) -> Result<RunMachinePlacement> {
    let controller = machine_fingerprint(build_sha, &local_machine_identity().await?);
    if controller != deployment.controller_machine_fingerprint {
        bail!("named-runner controller machine changed before a raw topology run");
    }
    let mut voters = Vec::with_capacity(selected.len());
    for voter in selected {
        let machine_id = ssh_output(&voter.ssh_host, &["cat", "/etc/machine-id"]).await?;
        let fingerprint =
            machine_fingerprint(build_sha, validate_machine_identity(machine_id.trim())?);
        let expected = deployment
            .voter_machine_fingerprints
            .get(usize::try_from(voter.node_id - 1)?)
            .context("deployment proof omitted a named voter fingerprint")?;
        if &fingerprint != expected {
            bail!(
                "named voter {} machine identity changed before a raw topology run",
                voter.node_id
            );
        }
        voters.push(fingerprint);
    }
    if voters
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != voters.len()
        || voters.iter().any(|fingerprint| fingerprint == &controller)
    {
        bail!("named raw topology did not retain its registered machine placement");
    }
    Ok(RunMachinePlacement { controller, voters })
}

async fn verify_deployed_image(
    config: &NamedRunnerConfig,
    build_sha: &str,
) -> Result<DeploymentProof> {
    let mut command = Command::new("docker");
    command
        .args(["image", "inspect", &config.image])
        .kill_on_drop(true);
    let local = tokio::time::timeout(REMOTE_COMMAND_TIMEOUT, command.output())
        .await
        .context("inspect named-runner image on controller timed out")?
        .context("inspect named-runner image on the external controller")?;
    if !local.status.success() {
        bail!(
            "inspect named-runner image on controller: {}",
            String::from_utf8_lossy(&local.stderr).trim()
        );
    }
    let local = parse_inspected_image(&String::from_utf8(local.stdout)?)?;
    if local.operating_system != "linux"
        || local.revision != build_sha
        || !matches!(local.architecture.as_str(), "amd64" | "arm64")
    {
        bail!("named-runner image must be native Linux amd64 or arm64");
    }
    validate_image_digest(&local.digest)?;
    let mut identity_command = Command::new("docker");
    identity_command
        .args([
            "run",
            "--rm",
            "--pull=never",
            "--entrypoint",
            "/usr/local/bin/plurx-cluster-check",
            &local.digest,
            "build-identity",
        ])
        .kill_on_drop(true);
    let identity = tokio::time::timeout(REMOTE_COMMAND_TIMEOUT, identity_command.output())
        .await
        .context("run named-runner image identity on controller timed out")?
        .context("run named-runner image identity on controller")?;
    if !identity.status.success() || String::from_utf8(identity.stdout)?.trim() != build_sha {
        bail!("named-runner binary is not bound to the controller source SHA");
    }
    let controller_identity = local_machine_identity().await?;
    let mut remote_identities = Vec::with_capacity(config.voters.len());
    for voter in &config.voters {
        let remote = ssh_output(
            &voter.ssh_host,
            &["docker", "image", "inspect", &config.image],
        )
        .await?;
        let remote = parse_inspected_image(&remote)?;
        if remote != local {
            bail!(
                "named voter {} does not have the controller image identity",
                voter.node_id
            );
        }
        let remote_build_sha = ssh_output(
            &voter.ssh_host,
            &[
                "docker",
                "run",
                "--rm",
                "--pull=never",
                "--entrypoint",
                "/usr/local/bin/plurx-cluster-check",
                &local.digest,
                "build-identity",
            ],
        )
        .await?;
        if remote_build_sha.trim() != build_sha {
            bail!(
                "named voter {} binary is not bound to the controller source SHA",
                voter.node_id
            );
        }
        let machine_architecture = ssh_output(&voter.ssh_host, &["uname", "-m"]).await?;
        if normalize_machine_architecture(machine_architecture.trim())? != local.architecture {
            bail!(
                "named voter {} would emulate the runner image",
                voter.node_id
            );
        }
        if ssh_output(&voter.ssh_host, &["uname", "-s"]).await?.trim() != "Linux" {
            bail!("named voter {} is not Linux", voter.node_id);
        }
        let machine_id = ssh_output(&voter.ssh_host, &["cat", "/etc/machine-id"]).await?;
        remote_identities.push(validate_machine_identity(machine_id.trim())?.to_owned());
    }
    if remote_identities
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != 4
        || remote_identities
            .iter()
            .any(|identity| identity == &controller_identity)
    {
        bail!("named runner did not resolve four machines external to the controller");
    }
    Ok(DeploymentProof {
        image_digest: local.digest,
        native_architecture: local.architecture,
        controller_machine_fingerprint: machine_fingerprint(build_sha, &controller_identity),
        voter_machine_fingerprints: remote_identities
            .iter()
            .map(|identity| machine_fingerprint(build_sha, identity))
            .collect(),
    })
}

fn normalize_machine_architecture(architecture: &str) -> Result<&'static str> {
    match architecture {
        "x86_64" | "amd64" => Ok("amd64"),
        "aarch64" | "arm64" => Ok("arm64"),
        other => bail!("unsupported named-runner machine architecture {other}"),
    }
}

async fn local_machine_identity() -> Result<String> {
    if let Ok(identity) = std::fs::read_to_string("/etc/machine-id") {
        return Ok(validate_machine_identity(identity.trim())?.to_owned());
    }
    let mut command = Command::new("ioreg");
    command
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .kill_on_drop(true);
    let output = tokio::time::timeout(REMOTE_COMMAND_TIMEOUT, command.output())
        .await
        .context("inspect controller machine identity timed out")?
        .context("inspect controller machine identity")?;
    if !output.status.success() {
        bail!("controller has no supported stable machine identity");
    }
    let output = String::from_utf8(output.stdout)?;
    let identity = output
        .lines()
        .find(|line| line.contains("\"IOPlatformUUID\""))
        .and_then(|line| line.split('"').nth(3))
        .context("controller ioreg omitted IOPlatformUUID")?
        .replace('-', "")
        .to_ascii_lowercase();
    Ok(validate_machine_identity(&identity)?.to_owned())
}

fn validate_machine_identity(identity: &str) -> Result<&str> {
    if identity.len() != 32
        || !identity
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("machine identity is not 128-bit lowercase hexadecimal");
    }
    Ok(identity)
}

fn machine_fingerprint(build_sha: &str, identity: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"plurx-named-machine-v1\0");
    hash.update(build_sha.as_bytes());
    hash.update(b"\0");
    hash.update(identity.as_bytes());
    hex::encode(hash.finalize())
}

fn validate_image_digest(digest: &str) -> Result<()> {
    let hex = digest
        .strip_prefix("sha256:")
        .context("named-runner image digest is not SHA-256")?;
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("named-runner image digest is not lowercase SHA-256");
    }
    Ok(())
}

type MetricExtractor = fn(&TopologyRun) -> Result<f64>;
type MetricSpec<'a> = (&'a str, &'a str, MetricExtractor);

fn summarize_metrics(
    artifacts: &[ClusterTopologyArtifact],
    target_half_width_percent: f64,
) -> Result<Vec<PairedMetric>> {
    let extractors: [MetricSpec<'_>; 6] = [
        ("acknowledged_write_p99", "microseconds", |run| {
            Ok(run.acknowledged_write_round_trip_p99_us)
        }),
        ("local_catalogue_read_p95", "microseconds", |run| {
            Ok(run.local_catalogue_read_p95_us)
        }),
        ("max_rss", "bytes", |run| {
            sum_resource(run, |sample| sample.max_rss_bytes.map(|value| value as f64))
        }),
        ("cpu", "seconds", |run| {
            sum_resource(run, |sample| sample.cpu_seconds)
        }),
        ("storage_write", "bytes", |run| {
            sum_resource(run, |sample| {
                sample.storage_write_bytes.map(|value| value as f64)
            })
        }),
        ("network_transmit", "bytes", |run| {
            sum_resource(run, |sample| {
                sample.network_transmit_bytes.map(|value| value as f64)
            })
        }),
    ];
    extractors
        .into_iter()
        .map(|(metric, unit, extract)| {
            let mut three = Vec::with_capacity(artifacts.len());
            let mut four = Vec::with_capacity(artifacts.len());
            for artifact in artifacts {
                let run3 = artifact
                    .runs
                    .iter()
                    .find(|run| run.voter_count == 3)
                    .context("named pair omitted three-voter run")?;
                let run4 = artifact
                    .runs
                    .iter()
                    .find(|run| run.voter_count == 4)
                    .context("named pair omitted four-voter run")?;
                three.push(extract(run3)?);
                four.push(extract(run4)?);
            }
            paired_metric(metric, unit, three, four, target_half_width_percent)
        })
        .collect()
}

fn sum_resource(
    run: &TopologyRun,
    value: impl Fn(&super::ResourceSample) -> Option<f64>,
) -> Result<f64> {
    run.resources.iter().try_fold(0.0, |total, sample| {
        Ok(total
            + value(sample).with_context(|| {
                format!("voter {} omitted named resource value", sample.node_id)
            })?)
    })
}

fn summarize_instrumentation_metrics(
    control: &[ClusterTopologyArtifact],
    instrumented: &[ClusterTopologyArtifact],
    target_half_width_percent: f64,
    overhead_budget_percent: f64,
) -> Result<Vec<InstrumentationMetric>> {
    if control.len() != instrumented.len() {
        bail!("P2f control and instrumented campaign lengths differ");
    }
    let specs: [(u64, &str, &str, MetricExtractor); 4] = [
        (3, "three_voter_cpu", "seconds", |run| {
            sum_resource(run, |sample| sample.cpu_seconds)
        }),
        (3, "three_voter_wall", "seconds", |run| {
            sum_resource(run, |sample| sample.wall_seconds)
        }),
        (4, "four_voter_cpu", "seconds", |run| {
            sum_resource(run, |sample| sample.cpu_seconds)
        }),
        (4, "four_voter_wall", "seconds", |run| {
            sum_resource(run, |sample| sample.wall_seconds)
        }),
    ];
    specs
        .into_iter()
        .map(|(voter_count, metric, unit, extract)| {
            let mut baseline = Vec::with_capacity(control.len());
            let mut measured = Vec::with_capacity(instrumented.len());
            for (control_artifact, instrumented_artifact) in control.iter().zip(instrumented) {
                let control_run = control_artifact
                    .runs
                    .iter()
                    .find(|run| run.voter_count == voter_count)
                    .with_context(|| format!("P2f control omitted {voter_count}-voter run"))?;
                let instrumented_run = instrumented_artifact
                    .runs
                    .iter()
                    .find(|run| run.voter_count == voter_count)
                    .with_context(|| {
                        format!("P2f instrumented arm omitted {voter_count}-voter run")
                    })?;
                baseline.push(extract(control_run)?);
                measured.push(extract(instrumented_run)?);
            }
            instrumentation_metric(
                metric,
                unit,
                baseline,
                measured,
                target_half_width_percent,
                overhead_budget_percent,
            )
        })
        .collect()
}

fn instrumentation_metric(
    metric: &str,
    unit: &str,
    control: Vec<f64>,
    instrumented: Vec<f64>,
    target_half_width_percent: f64,
    overhead_budget_percent: f64,
) -> Result<InstrumentationMetric> {
    if control.len() != instrumented.len()
        || control.len() < 2
        || control.len() > usize::try_from(P2F_MAX_PAIRS)?
    {
        bail!("P2f metric requires 2--{P2F_MAX_PAIRS} complete pairs");
    }
    if control
        .iter()
        .chain(&instrumented)
        .any(|value| !value.is_finite() || *value <= 0.0)
    {
        bail!("P2f metric values must be finite and positive");
    }
    let ratios = instrumented
        .iter()
        .zip(&control)
        .map(|(instrumented, control)| instrumented / control)
        .collect::<Vec<_>>();
    let logs = ratios.iter().map(|ratio| ratio.ln()).collect::<Vec<_>>();
    let n = logs.len() as f64;
    let mean = logs.iter().sum::<f64>() / n;
    let variance = logs.iter().map(|value| (value - mean).powi(2)).sum::<f64>() / (n - 1.0);
    let critical = student_t_975(logs.len() - 1)?;
    let half_width_log = critical * (variance / n).sqrt();
    let half_width_percent = (half_width_log.exp() - 1.0) * 100.0;
    let geometric_mean_ratio = mean.exp();
    Ok(InstrumentationMetric {
        metric: metric.to_owned(),
        unit: unit.to_owned(),
        control_values: control.clone(),
        instrumented_values: instrumented.clone(),
        instrumented_over_control_ratios: ratios.clone(),
        median_control: percentile_f64_type7(&control, 0.5)?,
        median_instrumented: percentile_f64_type7(&instrumented, 0.5)?,
        median_ratio: percentile_f64_type7(&ratios, 0.5)?,
        geometric_mean_ratio,
        ci95_lower_ratio: (mean - half_width_log).exp(),
        ci95_upper_ratio: (mean + half_width_log).exp(),
        ci95_half_width_percent: half_width_percent,
        precision_reached: half_width_percent <= target_half_width_percent,
        within_budget: (mean + half_width_log).exp() <= 1.0 + overhead_budget_percent / 100.0,
    })
}

fn paired_metric(
    metric: &str,
    unit: &str,
    three: Vec<f64>,
    four: Vec<f64>,
    target_half_width_percent: f64,
) -> Result<PairedMetric> {
    if three.len() != four.len() || three.len() < 2 || three.len() > 7 {
        bail!("paired metric requires 2--7 complete pairs");
    }
    if three
        .iter()
        .chain(&four)
        .any(|value| !value.is_finite() || *value <= 0.0)
    {
        bail!("paired metric values must be finite and positive");
    }
    let ratios = four
        .iter()
        .zip(&three)
        .map(|(four, three)| four / three)
        .collect::<Vec<_>>();
    let logs = ratios.iter().map(|ratio| ratio.ln()).collect::<Vec<_>>();
    let n = logs.len() as f64;
    let mean = logs.iter().sum::<f64>() / n;
    let variance = logs.iter().map(|value| (value - mean).powi(2)).sum::<f64>() / (n - 1.0);
    let critical = student_t_975(logs.len() - 1)?;
    let half_width_log = critical * (variance / n).sqrt();
    let half_width_percent = (half_width_log.exp() - 1.0) * 100.0;
    Ok(PairedMetric {
        metric: metric.to_owned(),
        unit: unit.to_owned(),
        three_voter_values: three.clone(),
        four_voter_values: four.clone(),
        four_over_three_ratios: ratios.clone(),
        median_three_voter: percentile_f64_type7(&three, 0.5)?,
        median_four_voter: percentile_f64_type7(&four, 0.5)?,
        median_ratio: percentile_f64_type7(&ratios, 0.5)?,
        geometric_mean_ratio: mean.exp(),
        ci95_lower_ratio: (mean - half_width_log).exp(),
        ci95_upper_ratio: (mean + half_width_log).exp(),
        ci95_half_width_percent: half_width_percent,
        precision_reached: half_width_percent <= target_half_width_percent,
    })
}

fn percentile_f64_type7(values: &[f64], quantile: f64) -> Result<f64> {
    if values.is_empty() || !(0.0..=1.0).contains(&quantile) {
        bail!("invalid percentile input");
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    if sorted.len() == 1 {
        return Ok(sorted[0]);
    }
    let rank = (sorted.len() - 1) as f64 * quantile;
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    Ok(sorted[lower] + (sorted[upper] - sorted[lower]) * rank.fract())
}

fn student_t_975(degrees_of_freedom: usize) -> Result<f64> {
    match degrees_of_freedom {
        1 => Ok(12.706_204_736),
        2 => Ok(4.302_652_73),
        3 => Ok(3.182_446_305),
        4 => Ok(2.776_445_105),
        5 => Ok(2.570_581_836),
        6 => Ok(2.446_911_851),
        7 => Ok(2.364_624_252),
        8 => Ok(2.306_004_135),
        9 => Ok(2.262_157_163),
        10 => Ok(2.228_138_852),
        11 => Ok(2.200_985_16),
        12 => Ok(2.178_812_83),
        13 => Ok(2.160_368_656),
        14 => Ok(2.144_786_688),
        15 => Ok(2.131_449_546),
        _ => bail!("named campaign only supports 2--16 pairs"),
    }
}

fn validate_instrumentation_arm(
    artifact: &NamedInstrumentationArmArtifact,
    expected_enabled: bool,
) -> Result<()> {
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../benchmarks/cluster-instrumentation-arm.schema.json"
    ))?;
    let validator = jsonschema::validator_for(&schema)?;
    if let Err(error) = validator.validate(&serde_json::to_value(artifact)?) {
        bail!("P2f arm violates its JSON Schema: {error}");
    }
    if artifact.schema_version != INSTRUMENTATION_ARM_SCHEMA_VERSION
        || artifact.instrumentation_enabled != expected_enabled
    {
        bail!("P2f arm did not attest its declared instrumentation state");
    }
    validate_topology_artifact(&artifact.topology)?;
    validate_instrumentation_state(
        &artifact.topology_attestations,
        &artifact.topology.topology_order,
        expected_enabled,
    )?;
    for (attestation, run) in artifact
        .topology_attestations
        .iter()
        .zip(&artifact.topology.runs)
    {
        if attestation.voter_count != run.voter_count {
            bail!("P2f topology attestation does not match its measured run");
        }
    }
    Ok(())
}

fn validate_instrumentation_state(
    attestations: &[InstrumentationTopologyAttestation],
    topology_order: &[u64],
    expected_enabled: bool,
) -> Result<()> {
    if attestations.len() != topology_order.len() {
        bail!("P2f arm omitted a topology instrumentation attestation");
    }
    for (attestation, expected_voter_count) in attestations.iter().zip(topology_order) {
        if attestation.voter_count != *expected_voter_count
            || attestation.nodes.len() != usize::try_from(*expected_voter_count)?
        {
            bail!("P2f topology attestation does not match its measured run");
        }
        for (index, node) in attestation.nodes.iter().enumerate() {
            let expected_node_id = u64::try_from(index)? + 1;
            if node.node_id != expected_node_id
                || node.instrumentation_enabled != expected_enabled
                || (!expected_enabled && node.recorded_operations_total != 0)
                || (expected_enabled && node.recorded_operations_total == 0)
            {
                bail!("P2f node recorder behavior contradicts its attested state");
            }
        }
    }
    Ok(())
}

pub fn validate_named_instrumentation_campaign(
    campaign: &NamedInstrumentationCampaign,
    artifact_root: Option<&Path>,
) -> Result<()> {
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../benchmarks/cluster-instrumentation-campaign.schema.json"
    ))?;
    let validator = jsonschema::validator_for(&schema)?;
    if let Err(error) = validator.validate(&serde_json::to_value(campaign)?) {
        bail!("P2f campaign violates its JSON Schema: {error}");
    }
    if campaign.schema_version != INSTRUMENTATION_CAMPAIGN_SCHEMA_VERSION
        || campaign.evidence_scope != NAMED_SCOPE
    {
        bail!("unsupported P2f campaign contract");
    }
    if !is_lower_hex(&campaign.build_sha, 40)
        || !safe_image_reference(&campaign.image)
        || !image_is_pinned_to_sha(&campaign.image, &campaign.build_sha)
    {
        bail!("P2f campaign image is not pinned to its complete build SHA");
    }
    validate_image_digest(&campaign.image_digest)?;
    if !matches!(campaign.native_architecture.as_str(), "amd64" | "arm64")
        || !is_lower_hex(&campaign.controller_machine_fingerprint, 64)
        || campaign.voter_machine_fingerprints.len() != 4
        || campaign
            .voter_machine_fingerprints
            .iter()
            .any(|fingerprint| !is_lower_hex(fingerprint, 64))
        || campaign
            .voter_machine_fingerprints
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != 4
        || campaign
            .voter_machine_fingerprints
            .contains(&campaign.controller_machine_fingerprint)
    {
        bail!("P2f machine-placement proof is invalid");
    }
    if campaign.started_at_unix_ms > campaign.finished_at_unix_ms
        || campaign.controller_host != campaign.load_generator_host
    {
        bail!("P2f campaign time or external-controller placement is invalid");
    }
    for (field, value) in [
        ("runner_id", campaign.runner_id.as_str()),
        ("controller_host", campaign.controller_host.as_str()),
        ("load_generator_host", campaign.load_generator_host.as_str()),
        (
            "load_generator_isolation",
            campaign.load_generator_isolation.as_str(),
        ),
    ] {
        validate_public_evidence(field, value, &[])?;
    }
    if campaign.voter_labels.len() != 4
        || campaign
            .voter_labels
            .iter()
            .any(|label| validate_public_evidence("voter label", label, &[]).is_err())
        || campaign
            .voter_labels
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != 4
        || !(1..=16).contains(&campaign.read_pool_size)
    {
        bail!("P2f campaign public runner identity or read pool is invalid");
    }
    let budgets = CampaignBudgets::default();
    if campaign.min_pairs != P2F_MIN_PAIRS
        || campaign.max_pairs != P2F_MAX_PAIRS
        || (campaign.confidence_half_width_percent - P2F_CONFIDENCE_HALF_WIDTH_PERCENT).abs()
            > f64::EPSILON
        || (campaign.overhead_budget_percent
            - budgets.instrumentation_cpu_and_wall_overhead_percent)
            .abs()
            > f64::EPSILON
        || campaign.raw_pairs.len() < usize::try_from(P2F_MIN_PAIRS)?
        || campaign.raw_pairs.len() > usize::try_from(P2F_MAX_PAIRS)?
        || !u64::try_from(campaign.raw_pairs.len())?.is_multiple_of(4)
    {
        bail!("P2f campaign changed its pre-registered pair, precision, or budget contract");
    }

    let mut controls = Vec::new();
    let mut instrumented = Vec::new();
    for (index, pair) in campaign.raw_pairs.iter().enumerate() {
        let expected = u64::try_from(index)? + 1;
        let expected_states =
            p2f_state_order(expected).map(
                |enabled| {
                    if enabled {
                        "instrumented"
                    } else {
                        "control"
                    }
                },
            );
        let expected_topologies = p2f_topology_order(expected);
        if pair.pair_index != expected
            || pair
                .state_order
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                != expected_states
            || pair.topology_order != expected_topologies
            || pair.control_artifact_path != format!("pair-{expected:02}-control.json")
            || pair.instrumented_artifact_path != format!("pair-{expected:02}-instrumented.json")
        {
            bail!("P2f raw pairs are not independently counterbalanced");
        }
        if pair.control_artifact_sha256 == pair.instrumented_artifact_sha256 {
            bail!("P2f control and instrumented arms cannot retain identical bytes");
        }
        if let Some(root) = artifact_root {
            let mut pair_artifacts = Vec::new();
            for (path, expected_hash, expected_enabled) in [
                (
                    &pair.control_artifact_path,
                    &pair.control_artifact_sha256,
                    false,
                ),
                (
                    &pair.instrumented_artifact_path,
                    &pair.instrumented_artifact_sha256,
                    true,
                ),
            ] {
                let bytes = std::fs::read(root.join(path))?;
                if hex::encode(Sha256::digest(&bytes)) != *expected_hash {
                    bail!("P2f raw artifact hash does not match retained bytes");
                }
                let arm: NamedInstrumentationArmArtifact = serde_json::from_slice(&bytes)?;
                validate_instrumentation_arm(&arm, expected_enabled)?;
                let artifact = &arm.topology;
                if artifact.evidence_scope != NAMED_SCOPE
                    || artifact.build_sha != campaign.build_sha
                    || artifact.runner_image_digest.as_deref()
                        != Some(campaign.image_digest.as_str())
                    || artifact.topology_order != pair.topology_order
                    || artifact.started_at_unix_ms < campaign.started_at_unix_ms
                    || artifact.finished_at_unix_ms > campaign.finished_at_unix_ms
                    || artifact.runs.iter().any(|run| {
                        let voter_count = usize::try_from(run.voter_count).unwrap_or(usize::MAX);
                        run.read_pool_size != campaign.read_pool_size
                            || run.controller_host != campaign.controller_host
                            || run.load_generator_host != campaign.load_generator_host
                            || run.load_generator_isolation.as_deref()
                                != Some(campaign.load_generator_isolation.as_str())
                            || run.controller_machine_fingerprint.as_deref()
                                != Some(campaign.controller_machine_fingerprint.as_str())
                            || campaign
                                .voter_machine_fingerprints
                                .get(..voter_count)
                                .is_none_or(|expected| {
                                    run.voter_machine_fingerprints.as_deref() != Some(expected)
                                })
                    })
                {
                    bail!("P2f raw artifact provenance contradicts its campaign");
                }
                pair_artifacts.push(arm.topology);
            }
            if pair_artifacts[0].workload != pair_artifacts[1].workload
                || pair_artifacts[0].workload_sha256 != pair_artifacts[1].workload_sha256
            {
                bail!("P2f changed the workload between control and instrumented arms");
            }
            controls.push(pair_artifacts.remove(0));
            instrumented.push(pair_artifacts.remove(0));
        }
    }

    let expected_metrics = [
        ("three_voter_cpu", "seconds"),
        ("three_voter_wall", "seconds"),
        ("four_voter_cpu", "seconds"),
        ("four_voter_wall", "seconds"),
    ];
    if campaign.metrics.len() != expected_metrics.len()
        || campaign
            .metrics
            .iter()
            .zip(expected_metrics)
            .any(|(metric, expected)| (metric.metric.as_str(), metric.unit.as_str()) != expected)
    {
        bail!("P2f campaign must report CPU and wall overhead for both topologies");
    }
    if campaign.metrics.iter().any(|metric| {
        metric.control_values.len() != campaign.raw_pairs.len()
            || metric.instrumented_values.len() != campaign.raw_pairs.len()
            || metric.instrumented_over_control_ratios.len() != campaign.raw_pairs.len()
    }) {
        bail!("P2f metric sample shapes do not match its raw pairs");
    }
    let recomputed = campaign
        .metrics
        .iter()
        .map(|metric| {
            instrumentation_metric(
                &metric.metric,
                &metric.unit,
                metric.control_values.clone(),
                metric.instrumented_values.clone(),
                campaign.confidence_half_width_percent,
                campaign.overhead_budget_percent,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    if recomputed != campaign.metrics {
        bail!("P2f metric calculations do not match retained summary samples");
    }
    if artifact_root.is_some()
        && summarize_instrumentation_metrics(
            &controls,
            &instrumented,
            campaign.confidence_half_width_percent,
            campaign.overhead_budget_percent,
        )? != campaign.metrics
    {
        bail!("P2f metric summary does not match hash-bound raw topology artifacts");
    }
    let minimum = usize::try_from(campaign.min_pairs)?;
    for prefix in minimum..campaign.raw_pairs.len() {
        if !u64::try_from(prefix)?.is_multiple_of(4) {
            continue;
        }
        let earlier_precise = campaign.metrics.iter().try_fold(true, |precise, metric| {
            Ok::<_, anyhow::Error>(
                precise
                    && instrumentation_metric(
                        &metric.metric,
                        &metric.unit,
                        metric.control_values[..prefix].to_vec(),
                        metric.instrumented_values[..prefix].to_vec(),
                        campaign.confidence_half_width_percent,
                        campaign.overhead_budget_percent,
                    )?
                    .precision_reached,
            )
        })?;
        if earlier_precise {
            bail!("P2f campaign continued after an earlier precise prefix");
        }
    }
    let every_precise = campaign
        .metrics
        .iter()
        .all(|metric| metric.precision_reached);
    let every_within_budget = campaign.metrics.iter().all(|metric| metric.within_budget);
    match (
        campaign.result.as_str(),
        campaign.stop_reason.as_str(),
        every_precise,
        every_within_budget,
        campaign.raw_pairs.len(),
    ) {
        ("accepted", "precision_reached", true, true, count)
            if count >= usize::try_from(P2F_MIN_PAIRS)? && count.is_multiple_of(4) => {}
        ("rejected", "precision_reached", true, false, count)
            if count >= usize::try_from(P2F_MIN_PAIRS)? && count.is_multiple_of(4) => {}
        ("inconclusive", "max_pairs_reached", false, _, count)
            if count == usize::try_from(P2F_MAX_PAIRS)? => {}
        _ => bail!("P2f result contradicts its precision, budget, or stopping rule"),
    }
    Ok(())
}

pub fn validate_named_campaign(
    campaign: &NamedTopologyCampaign,
    artifact_root: Option<&Path>,
) -> Result<()> {
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../benchmarks/cluster-topology-campaign.schema.json"
    ))?;
    let validator = jsonschema::validator_for(&schema)?;
    let value = serde_json::to_value(campaign)?;
    if let Err(error) = validator.validate(&value) {
        bail!("named campaign violates its JSON Schema: {error}");
    }
    if campaign.schema_version != NAMED_CAMPAIGN_SCHEMA_VERSION
        || campaign.evidence_scope != NAMED_SCOPE
    {
        bail!("unsupported named campaign contract");
    }
    if !is_lower_hex(&campaign.build_sha, 40) {
        bail!("named campaign build SHA must be complete");
    }
    if !safe_image_reference(&campaign.image)
        || !image_is_pinned_to_sha(&campaign.image, &campaign.build_sha)
    {
        bail!("named campaign image is not pinned to its build SHA");
    }
    validate_image_digest(&campaign.image_digest)?;
    if !matches!(campaign.native_architecture.as_str(), "amd64" | "arm64")
        || !is_lower_hex(&campaign.controller_machine_fingerprint, 64)
        || campaign.voter_machine_fingerprints.len() != 4
        || campaign
            .voter_machine_fingerprints
            .iter()
            .any(|fingerprint| !is_lower_hex(fingerprint, 64))
        || campaign
            .voter_machine_fingerprints
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != 4
        || campaign
            .voter_machine_fingerprints
            .contains(&campaign.controller_machine_fingerprint)
    {
        bail!("named campaign machine-placement proof is invalid");
    }
    if campaign.started_at_unix_ms > campaign.finished_at_unix_ms {
        bail!("named campaign finished before it started");
    }
    if campaign.controller_host != campaign.load_generator_host {
        bail!("named campaign load generator was not the external controller process");
    }
    for (field, value) in [
        ("runner_id", campaign.runner_id.as_str()),
        ("controller_host", campaign.controller_host.as_str()),
        ("load_generator_host", campaign.load_generator_host.as_str()),
        (
            "load_generator_isolation",
            campaign.load_generator_isolation.as_str(),
        ),
    ] {
        validate_public_evidence(field, value, &[])?;
    }
    if campaign.voter_labels.len() != 4
        || campaign
            .voter_labels
            .iter()
            .any(|label| validate_public_evidence("voter label", label, &[]).is_err())
        || campaign
            .voter_labels
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != 4
    {
        bail!("named campaign must identify four distinct runner nodes");
    }
    if !(1..=16).contains(&campaign.read_pool_size) {
        bail!("named campaign read_pool_size must be between 1 and 16");
    }
    if campaign.min_pairs != 3
        || campaign.max_pairs != 7
        || (campaign.confidence_half_width_percent - 5.0).abs() > f64::EPSILON
    {
        bail!("named campaign changed the pre-registered stopping rule");
    }
    if campaign.raw_pairs.len() < 3 || campaign.raw_pairs.len() > 7 {
        bail!("named campaign must retain 3--7 raw pairs");
    }
    let mut artifacts = Vec::new();
    for (index, pair) in campaign.raw_pairs.iter().enumerate() {
        let expected = u64::try_from(index)? + 1;
        let expected_order = if expected % 2 == 1 {
            [3, 4].as_slice()
        } else {
            [4, 3].as_slice()
        };
        if pair.pair_index != expected
            || pair.topology_order != expected_order
            || pair.artifact_path != format!("pair-{expected:02}.json")
        {
            bail!("named campaign raw pairs are not counterbalanced in order");
        }
        if let Some(root) = artifact_root {
            let bytes = std::fs::read(root.join(&pair.artifact_path))?;
            if hex::encode(Sha256::digest(&bytes)) != pair.artifact_sha256 {
                bail!("named campaign raw pair hash does not match retained bytes");
            }
            let artifact: ClusterTopologyArtifact = serde_json::from_slice(&bytes)?;
            validate_topology_artifact(&artifact)?;
            if artifact.evidence_scope != NAMED_SCOPE
                || artifact.build_sha != campaign.build_sha
                || artifact.runner_image_digest.as_deref() != Some(campaign.image_digest.as_str())
                || artifact.topology_order != pair.topology_order
                || artifact.started_at_unix_ms < campaign.started_at_unix_ms
                || artifact.finished_at_unix_ms > campaign.finished_at_unix_ms
            {
                bail!("named raw pair provenance does not match its campaign");
            }
            if artifact.runs.iter().any(|run| {
                let voter_count = usize::try_from(run.voter_count).unwrap_or(usize::MAX);
                run.read_pool_size != campaign.read_pool_size
                    || run.controller_host != campaign.controller_host
                    || run.load_generator_host != campaign.load_generator_host
                    || run.load_generator_isolation.as_deref()
                        != Some(campaign.load_generator_isolation.as_str())
                    || run.controller_machine_fingerprint.as_deref()
                        != Some(campaign.controller_machine_fingerprint.as_str())
                    || campaign
                        .voter_machine_fingerprints
                        .get(..voter_count)
                        .is_none_or(|expected| {
                            run.voter_machine_fingerprints.as_deref() != Some(expected)
                        })
                    || run.resources.iter().any(|resource| {
                        [
                            ("hardware class", resource.hardware.as_str()),
                            ("storage class", resource.storage_device.as_str()),
                            ("network class", resource.network_path.as_str()),
                        ]
                        .into_iter()
                        .any(|(field, value)| validate_public_evidence(field, value, &[]).is_err())
                    })
            }) {
                bail!("named raw pair controller placement does not match its campaign");
            }
            artifacts.push(artifact);
        }
    }
    let expected_metrics = [
        ("acknowledged_write_p99", "microseconds"),
        ("local_catalogue_read_p95", "microseconds"),
        ("max_rss", "bytes"),
        ("cpu", "seconds"),
        ("storage_write", "bytes"),
        ("network_transmit", "bytes"),
    ];
    if campaign.metrics.len() != expected_metrics.len()
        || campaign
            .metrics
            .iter()
            .zip(expected_metrics)
            .any(|(metric, expected)| (metric.metric.as_str(), metric.unit.as_str()) != expected)
    {
        bail!("named campaign must report all six pre-registered estimands");
    }
    let metrics_well_formed = campaign.metrics.iter().all(|metric| {
        metric.three_voter_values.len() == campaign.raw_pairs.len()
            && metric.four_voter_values.len() == campaign.raw_pairs.len()
            && metric.four_over_three_ratios.len() == campaign.raw_pairs.len()
            && metric.ci95_half_width_percent.is_finite()
            && metric.precision_reached
                == (metric.ci95_half_width_percent <= campaign.confidence_half_width_percent)
    });
    if !metrics_well_formed {
        bail!("named campaign metric shapes or precision flags are invalid");
    }
    let recomputed_from_summary = campaign
        .metrics
        .iter()
        .map(|metric| {
            paired_metric(
                &metric.metric,
                &metric.unit,
                metric.three_voter_values.clone(),
                metric.four_voter_values.clone(),
                campaign.confidence_half_width_percent,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    if recomputed_from_summary != campaign.metrics {
        bail!("named campaign metric calculations do not match their retained samples");
    }
    if artifact_root.is_some()
        && summarize_metrics(&artifacts, campaign.confidence_half_width_percent)?
            != campaign.metrics
    {
        bail!("named campaign metric summary does not match its raw pairs");
    }
    let minimum = usize::try_from(campaign.min_pairs)?;
    for prefix in minimum..campaign.raw_pairs.len() {
        let precise = campaign.metrics.iter().try_fold(true, |precise, metric| {
            Ok::<_, anyhow::Error>(
                precise
                    && paired_metric(
                        &metric.metric,
                        &metric.unit,
                        metric.three_voter_values[..prefix].to_vec(),
                        metric.four_voter_values[..prefix].to_vec(),
                        campaign.confidence_half_width_percent,
                    )?
                    .precision_reached,
            )
        })?;
        if precise {
            bail!("named campaign continued after an earlier precise prefix");
        }
    }
    if campaign.budgets != CampaignBudgets::default() {
        bail!("named campaign changed the pre-registered performance budgets");
    }
    let every_precise = campaign
        .metrics
        .iter()
        .all(|metric| metric.precision_reached);
    match (
        campaign.result.as_str(),
        campaign.stop_reason.as_str(),
        every_precise,
        campaign.raw_pairs.len(),
    ) {
        ("accepted", "precision_reached", true, count) if count >= 3 => {}
        ("inconclusive", "max_pairs_reached", false, 7) => {}
        _ => bail!("named campaign result contradicts its precision and stop rule"),
    }
    Ok(())
}

fn pretty_json(value: &impl Serialize) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_scale_student_t_precision_uses_the_registered_pair_limits() {
        let metric = paired_metric(
            "cpu",
            "seconds",
            vec![100.0, 101.0, 99.0],
            vec![110.0, 111.1, 108.9],
            5.0,
        )
        .expect("paired metric");
        assert!((metric.geometric_mean_ratio - 1.1).abs() < 1e-12);
        assert!(metric.precision_reached);

        let noisy = paired_metric(
            "cpu",
            "seconds",
            vec![100.0, 100.0, 100.0],
            vec![90.0, 110.0, 130.0],
            5.0,
        )
        .expect("noisy metric");
        assert!(!noisy.precision_reached);
    }

    #[test]
    fn first_pair_is_retained_without_attempting_an_undefined_interval() {
        assert!(!campaign_metrics_ready(1));
        assert!(campaign_metrics_ready(2));
    }

    #[test]
    fn output_claim_and_publication_have_one_winner() {
        let parent = tempfile::tempdir().expect("named output parent");
        let output = parent.path().join("campaign");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let claimers = ["a".repeat(64), "b".repeat(64)]
            .into_iter()
            .map(|owner| {
                let output = output.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    claim_named_output(&output, &owner)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let claim_results = claimers
            .into_iter()
            .map(|claimer| claimer.join().expect("competing claimer did not panic"))
            .collect::<Vec<_>>();
        assert_eq!(
            claim_results.iter().filter(|result| result.is_ok()).count(),
            1
        );
        let retained_owner = std::fs::read_to_string(output.join(OUTPUT_OWNER_FILENAME))
            .expect("winning owner retained");
        assert!(retained_owner.trim() == "a".repeat(64) || retained_owner.trim() == "b".repeat(64));

        let destination = output.join("pair-01.json");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let writers = [b"first\n".to_vec(), b"second\n".to_vec()]
            .into_iter()
            .map(|bytes| {
                let destination = destination.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    write_atomic(&destination, &bytes)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let results = writers
            .into_iter()
            .map(|writer| writer.join().expect("competing writer did not panic"))
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let retained = std::fs::read(destination).expect("winning bytes retained");
        assert!(retained == b"first\n" || retained == b"second\n");
    }

    #[test]
    fn remote_tokens_refuse_options_and_shell_syntax() {
        let sha = "a".repeat(40);
        assert!(safe_image_reference(&format!("plurx-cluster-check:{sha}")));
        assert!(safe_host_token("192.168.4.8"));
        assert!(!safe_host_token("-oProxyCommand=bad"));
        assert!(!safe_host_token("nuc4;reboot"));
        assert!(!safe_image_reference("--privileged"));
        assert!(!safe_image_reference("$(touch /tmp/no)"));
        assert!(
            validate_public_evidence("runner_id", "private-runner-1", &["private-runner-1"])
                .is_err()
        );
        assert!(validate_public_evidence("runner_id", "lab 10.20.30.40", &[]).is_err());
        assert!(validate_public_evidence("runner_id", " padded-label ", &[]).is_err());
    }

    #[test]
    fn raw_image_inspection_needs_no_shell_sensitive_remote_format() {
        let output = serde_json::json!([{
            "Id": format!("sha256:{}", "b".repeat(64)),
            "Architecture": "arm64",
            "Os": "linux",
            "Config": {
                "Labels": {
                    "org.opencontainers.image.revision": "a".repeat(40)
                }
            }
        }])
        .to_string();
        assert_eq!(
            parse_inspected_image(&output).expect("pinned image inspection"),
            InspectedImage {
                digest: format!("sha256:{}", "b".repeat(64)),
                architecture: "arm64".to_owned(),
                operating_system: "linux".to_owned(),
                revision: "a".repeat(40),
            }
        );
        assert!(parse_inspected_image("[]").is_err());
    }

    #[test]
    fn container_cleanup_requires_the_separate_owner_capability() {
        let owner = "a".repeat(64);
        let inspection = serde_json::json!([{
            "Config": { "Labels": { (REMOTE_OWNER_LABEL): owner } }
        }]);
        assert_eq!(
            container_owner_from_inspect(&serde_json::to_vec(&inspection).expect("inspect JSON"))
                .expect("owner label"),
            "a".repeat(64)
        );
        let unowned = serde_json::json!([{"Config": {"Labels": {}}}]);
        assert!(container_owner_from_inspect(
            &serde_json::to_vec(&unowned).expect("unowned inspect JSON")
        )
        .is_err());
    }

    #[test]
    fn cleanup_scope_refuses_options_and_path_components() {
        use std::os::unix::fs::PermissionsExt;

        let nonce = "a".repeat(64);
        let valid = RemoteCleanup {
            ssh_host: "runner-1".to_owned(),
            container_name: format!("plurx-named-{nonce}-p1-v3-n1"),
            remote_root: format!("{REMOTE_ROOT_PREFIX}{nonce}-p1-v3-n1"),
            image_digest: format!("sha256:{}", "b".repeat(64)),
            node_id: 1,
        };
        validate_cleanup(&valid).expect("runner-owned cleanup target");
        let mut traversal = valid.clone();
        traversal.remote_root = format!("{REMOTE_ROOT_PREFIX}../other");
        assert!(validate_cleanup(&traversal).is_err());
        let mut option = valid;
        option.ssh_host = "-oProxyCommand=bad".to_owned();
        assert!(validate_cleanup(&option).is_err());

        let directory = tempfile::tempdir().expect("cleanup manifest directory");
        let manifest = directory.path().join(CLEANUP_MANIFEST_FILENAME);
        let owner = "c".repeat(64);
        write_cleanup_manifest(&manifest, &owner, &[traversal])
            .expect_err("unsafe cleanup cannot be published");
        write_cleanup_manifest(
            &manifest,
            &owner,
            &[RemoteCleanup {
                ssh_host: "runner-1".to_owned(),
                container_name: format!("plurx-named-{nonce}-p1-v3-n1"),
                remote_root: format!("{REMOTE_ROOT_PREFIX}{nonce}-p1-v3-n1"),
                image_digest: format!("sha256:{}", "b".repeat(64)),
                node_id: 1,
            }],
        )
        .expect("publish private cleanup manifest");
        let mode = std::fs::metadata(manifest)
            .expect("cleanup manifest metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn cleanup_targets_are_bound_to_each_campaign_nonce() {
        let cleanup = |nonce: &str| RemoteCleanup {
            ssh_host: "runner-1".to_owned(),
            container_name: format!("plurx-named-{nonce}-p1-v3-n1"),
            remote_root: format!("{REMOTE_ROOT_PREFIX}{nonce}-p1-v3-n1"),
            image_digest: format!("sha256:{}", "b".repeat(64)),
            node_id: 1,
        };
        let first = cleanup(&"a".repeat(64));
        let second = cleanup(&"c".repeat(64));
        validate_cleanup(&first).expect("first campaign target");
        validate_cleanup(&second).expect("second campaign target");
        assert_ne!(first.container_name, second.container_name);

        let mut crossed = first;
        crossed.container_name = second.container_name;
        validate_cleanup(&crossed).expect_err("cross-campaign target must be rejected");

        let first_owner = remote_owner_marker(&crossed, &"d".repeat(64)).expect("first owner");
        let second_owner = remote_owner_marker(&crossed, &"e".repeat(64)).expect("second owner");
        assert_ne!(first_owner, second_owner);
    }

    #[test]
    fn cleanup_authority_rejects_links_and_non_private_files() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let parent = tempfile::tempdir().expect("cleanup authority parent");
        let output = parent.path().join("campaign");
        std::fs::create_dir(&output).expect("campaign output");
        let owner = "a".repeat(64);
        write_atomic_with_mode(
            &output.join(OUTPUT_OWNER_FILENAME),
            format!("{owner}\n").as_bytes(),
            0o600,
        )
        .expect("owner marker");
        let cleanup = RemoteCleanup {
            ssh_host: "runner-1".to_owned(),
            container_name: format!("plurx-named-{owner}-p1-v3-n1"),
            remote_root: format!("{REMOTE_ROOT_PREFIX}{owner}-p1-v3-n1"),
            image_digest: format!("sha256:{}", "b".repeat(64)),
            node_id: 1,
        };
        let manifest = output.join(CLEANUP_MANIFEST_FILENAME);
        write_cleanup_manifest(&manifest, &owner, &[cleanup]).expect("cleanup manifest");
        CleanupAuthority::open(&manifest).expect("private real cleanup authority");

        std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o644))
            .expect("make manifest public");
        assert!(CleanupAuthority::open(&manifest).is_err());
        std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o600))
            .expect("restore manifest mode");

        let linked_output = parent.path().join("linked-campaign");
        symlink(&output, &linked_output).expect("linked output");
        assert!(CleanupAuthority::open(&linked_output.join(CLEANUP_MANIFEST_FILENAME)).is_err());
    }

    #[test]
    fn campaign_schema_and_rust_validator_accept_the_same_summary_shape() {
        let samples3 = vec![100.0, 101.0, 99.0];
        let samples4 = vec![110.0, 111.1, 108.9];
        let metrics = [
            ("acknowledged_write_p99", "microseconds"),
            ("local_catalogue_read_p95", "microseconds"),
            ("max_rss", "bytes"),
            ("cpu", "seconds"),
            ("storage_write", "bytes"),
            ("network_transmit", "bytes"),
        ]
        .into_iter()
        .map(|(metric, unit)| {
            paired_metric(metric, unit, samples3.clone(), samples4.clone(), 5.0)
                .expect("paired fixture")
        })
        .collect();
        let campaign = NamedTopologyCampaign {
            schema_version: NAMED_CAMPAIGN_SCHEMA_VERSION,
            evidence_scope: NAMED_SCOPE.to_owned(),
            build_sha: "a".repeat(40),
            runner_id: "runner-v1".to_owned(),
            image: format!("plurx-cluster-check:{}", "a".repeat(40)),
            image_digest: format!("sha256:{}", "c".repeat(64)),
            native_architecture: "amd64".to_owned(),
            controller_machine_fingerprint: "d".repeat(64),
            voter_machine_fingerprints: (1..=4).map(|id| format!("{id:064x}")).collect(),
            controller_host: "controller-1".to_owned(),
            load_generator_host: "controller-1".to_owned(),
            load_generator_isolation: "external and idle".to_owned(),
            voter_labels: (1..=4).map(|id| format!("runner-node-{id}")).collect(),
            read_pool_size: 4,
            min_pairs: 3,
            max_pairs: 7,
            confidence_half_width_percent: 5.0,
            started_at_unix_ms: 1,
            finished_at_unix_ms: 2,
            stop_reason: "precision_reached".to_owned(),
            result: "accepted".to_owned(),
            raw_pairs: (1..=3)
                .map(|pair_index| RawPairReference {
                    pair_index,
                    topology_order: if pair_index % 2 == 1 {
                        vec![3, 4]
                    } else {
                        vec![4, 3]
                    },
                    artifact_path: format!("pair-{pair_index:02}.json"),
                    artifact_sha256: "b".repeat(64),
                })
                .collect(),
            metrics,
            budgets: CampaignBudgets::default(),
        };
        validate_named_campaign(&campaign, None).expect("Rust campaign validator");

        let mut malformed = campaign.clone();
        malformed.raw_pairs.push(RawPairReference {
            pair_index: 4,
            topology_order: vec![4, 3],
            artifact_path: "pair-04.json".to_owned(),
            artifact_sha256: "b".repeat(64),
        });
        assert!(validate_named_campaign(&malformed, None).is_err());

        let mut unsupported_pool = campaign.clone();
        unsupported_pool.read_pool_size = 17;
        assert!(validate_named_campaign(&unsupported_pool, None).is_err());

        let mut changed_guardrail = campaign.clone();
        changed_guardrail
            .budgets
            .read_pool_max_rss_regression_percent = 11.0;
        assert!(validate_named_campaign(&changed_guardrail, None).is_err());

        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../../benchmarks/cluster-topology-campaign.schema.json"
        ))
        .expect("campaign schema JSON");
        let validator = jsonschema::validator_for(&schema).expect("campaign schema validator");
        let json = serde_json::to_value(campaign).expect("campaign JSON");
        assert!(validator.is_valid(&json));

        let mut missing_pool = json.clone();
        missing_pool
            .as_object_mut()
            .expect("campaign object")
            .remove("read_pool_size");
        assert!(!validator.is_valid(&missing_pool));
    }

    #[test]
    fn instrumentation_campaign_schema_and_rust_validator_pin_the_control_contract() {
        let control = vec![100.0, 100.1, 99.9, 100.2, 99.8, 100.3, 99.7, 100.4];
        let measured = control
            .iter()
            .map(|value| value * 1.005)
            .collect::<Vec<_>>();
        let metrics = [
            "three_voter_cpu",
            "three_voter_wall",
            "four_voter_cpu",
            "four_voter_wall",
        ]
        .into_iter()
        .map(|metric| {
            instrumentation_metric(
                metric,
                "seconds",
                control.clone(),
                measured.clone(),
                P2F_CONFIDENCE_HALF_WIDTH_PERCENT,
                2.0,
            )
            .expect("P2f metric")
        })
        .collect();
        let campaign = NamedInstrumentationCampaign {
            schema_version: INSTRUMENTATION_CAMPAIGN_SCHEMA_VERSION,
            evidence_scope: NAMED_SCOPE.to_owned(),
            build_sha: "a".repeat(40),
            runner_id: "runner-v1".to_owned(),
            image: format!("plurx-cluster-check:{}", "a".repeat(40)),
            image_digest: format!("sha256:{}", "c".repeat(64)),
            native_architecture: "amd64".to_owned(),
            controller_machine_fingerprint: "d".repeat(64),
            voter_machine_fingerprints: (1..=4).map(|id| format!("{id:064x}")).collect(),
            controller_host: "controller-1".to_owned(),
            load_generator_host: "controller-1".to_owned(),
            load_generator_isolation: "external and idle".to_owned(),
            voter_labels: (1..=4).map(|id| format!("runner-node-{id}")).collect(),
            read_pool_size: 4,
            min_pairs: P2F_MIN_PAIRS,
            max_pairs: P2F_MAX_PAIRS,
            confidence_half_width_percent: P2F_CONFIDENCE_HALF_WIDTH_PERCENT,
            overhead_budget_percent: 2.0,
            started_at_unix_ms: 1,
            finished_at_unix_ms: 2,
            stop_reason: "precision_reached".to_owned(),
            result: "accepted".to_owned(),
            raw_pairs: (1..=P2F_MIN_PAIRS)
                .map(|pair_index| InstrumentationPairReference {
                    pair_index,
                    state_order: p2f_state_order(pair_index)
                        .into_iter()
                        .map(|enabled| {
                            if enabled {
                                "instrumented".to_owned()
                            } else {
                                "control".to_owned()
                            }
                        })
                        .collect(),
                    topology_order: p2f_topology_order(pair_index).to_vec(),
                    control_artifact_path: format!("pair-{pair_index:02}-control.json"),
                    control_artifact_sha256: "b".repeat(64),
                    instrumented_artifact_path: format!("pair-{pair_index:02}-instrumented.json"),
                    instrumented_artifact_sha256: "e".repeat(64),
                })
                .collect(),
            metrics,
        };
        validate_named_instrumentation_campaign(&campaign, None)
            .expect("Rust P2f campaign validator");

        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../../benchmarks/cluster-instrumentation-campaign.schema.json"
        ))
        .expect("P2f schema JSON");
        let validator = jsonschema::validator_for(&schema).expect("P2f schema validator");
        assert!(validator.is_valid(&serde_json::to_value(&campaign).expect("P2f JSON")));

        let mut changed_budget = campaign.clone();
        changed_budget.overhead_budget_percent = 3.0;
        assert!(validate_named_instrumentation_campaign(&changed_budget, None).is_err());

        let mut swapped_state = campaign.clone();
        swapped_state.raw_pairs[0].state_order.reverse();
        assert!(validate_named_instrumentation_campaign(&swapped_state, None).is_err());

        let mut identical_arms = campaign.clone();
        identical_arms.raw_pairs[0].instrumented_artifact_sha256 =
            identical_arms.raw_pairs[0].control_artifact_sha256.clone();
        assert!(validate_named_instrumentation_campaign(&identical_arms, None).is_err());

        let mut invented_result = campaign.clone();
        invented_result.result = "rejected".to_owned();
        assert!(validate_named_instrumentation_campaign(&invented_result, None).is_err());

        let mut changed_sample = campaign.clone();
        changed_sample.metrics[0].instrumented_values[0] += 1.0;
        assert!(validate_named_instrumentation_campaign(&changed_sample, None).is_err());
    }

    #[test]
    fn instrumentation_attestations_bind_switch_state_to_recorder_activity() {
        let fixture = |enabled: bool| {
            [3_u64, 4]
                .into_iter()
                .map(|voter_count| InstrumentationTopologyAttestation {
                    voter_count,
                    nodes: (1..=voter_count)
                        .map(|node_id| InstrumentationNodeAttestation {
                            node_id,
                            instrumentation_enabled: enabled,
                            recorded_operations_total: if enabled { 1 } else { 0 },
                        })
                        .collect(),
                })
                .collect::<Vec<_>>()
        };
        validate_instrumentation_state(&fixture(false), &[3, 4], false)
            .expect("control recorder attestation");
        validate_instrumentation_state(&fixture(true), &[3, 4], true)
            .expect("instrumented recorder attestation");

        let mut control_recorded = fixture(false);
        control_recorded[0].nodes[0].recorded_operations_total = 1;
        assert!(validate_instrumentation_state(&control_recorded, &[3, 4], false).is_err());

        let mut measured_silent = fixture(true);
        measured_silent[1].nodes[3].recorded_operations_total = 0;
        assert!(validate_instrumentation_state(&measured_silent, &[3, 4], true).is_err());

        let mut swapped = fixture(false);
        swapped[0].nodes[0].instrumentation_enabled = true;
        assert!(validate_instrumentation_state(&swapped, &[3, 4], false).is_err());
    }

    #[test]
    fn instrumentation_budget_uses_the_upper_confidence_bound() {
        let control = vec![100.0; 8];
        let measured = vec![99.0, 104.8, 99.0, 104.8, 99.0, 104.8, 99.0, 104.8];
        let metric = instrumentation_metric(
            "four_voter_cpu",
            "seconds",
            control,
            measured,
            P2F_CONFIDENCE_HALF_WIDTH_PERCENT,
            2.0,
        )
        .expect("P2f confidence fixture");
        assert!(metric.geometric_mean_ratio < 1.02);
        assert!(metric.ci95_upper_ratio > 1.02);
        assert!(!metric.within_budget);
    }
}
