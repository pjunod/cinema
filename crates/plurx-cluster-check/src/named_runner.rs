//! P0c's pinned four-machine topology campaign.
//!
//! The ordinary topology mode intentionally stays loopback-only semantic CI.
//! This module is the explicit hardware boundary: four named Linux hosts run
//! fresh containerized voters, while the controller remains an external load
//! generator. Raw pair artifacts keep the topology-v1 contract; this module
//! adds the pre-registered 3--7-pair stopping rule and an aggregate campaign.

use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
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
    validate_topology_artifact, ClusterProcesses, ClusterTopologyArtifact, NodeLaunch, NodeProcess,
    NodeSpec, TopologyRun, TopologyWorkload, CONVERGENCE_TIMEOUT, TOPOLOGY_ARTIFACT_SCHEMA_VERSION,
};

pub const NAMED_CAMPAIGN_SCHEMA_VERSION: u32 = 1;
const NAMED_SCOPE: &str = "named_runner";
const SSH_CONNECT_TIMEOUT_SECS: &str = "10";
const REMOTE_ROOT_PREFIX: &str = "/var/tmp/plurx-cluster-named.";
const REMOTE_COMMAND_TIMEOUT: Duration = Duration::from_secs(45);
pub const CLEANUP_MANIFEST_FILENAME: &str = ".active-cleanup.json";
const OUTPUT_OWNER_FILENAME: &str = ".campaign-owner";
static ATOMIC_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
                run_remote_topology(
                    &config,
                    pair_index,
                    voter_count,
                    &workload,
                    &workload_sha256,
                    &cleanup_manifest,
                    &image_digest,
                    owner_nonce,
                )
                .await?,
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

fn campaign_metrics_ready(completed_pairs: usize) -> bool {
    completed_pairs >= 2
}

async fn run_remote_topology(
    config: &NamedRunnerConfig,
    pair_index: u64,
    voter_count: u64,
    workload: &TopologyWorkload,
    workload_sha256: &str,
    cleanup_manifest: &Path,
    image_digest: &str,
    owner_nonce: &str,
) -> Result<TopologyRun> {
    let selected = &config.voters[..usize::try_from(voter_count)?];
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
                "plurx-named-p{pair_index}-v{voter_count}-n{}",
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

    for cleanup in &cleanups {
        if let Err(error) =
            ssh_output(&cleanup.ssh_host, &["mkdir", "--", &cleanup.remote_root]).await
        {
            return Err(with_cleanup_error(
                error,
                finish_remote_cleanup(&cleanups, cleanup_manifest).await,
            ));
        }
    }

    let mut nodes = Vec::with_capacity(selected.len());
    for (voter, cleanup) in selected.iter().zip(&cleanups) {
        let launch = NodeLaunch {
            node_id: voter.node_id,
            root: PathBuf::from("/data"),
            nodes: specs.clone(),
            listen_addr: "0.0.0.0".to_owned(),
            emulate_old_watermark_handler: false,
        };
        match spawn_remote_node(
            voter,
            config,
            &cleanup.container_name,
            &cleanup.remote_root,
            image_digest,
            &launch,
        ) {
            Ok(process) => nodes.push(Some(process)),
            Err(error) => {
                drop(nodes);
                return Err(with_cleanup_error(
                    error,
                    finish_remote_cleanup(&cleanups, cleanup_manifest).await,
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
        let identities = selected
            .iter()
            .map(|voter| ResourceIdentity {
                node_id: voter.node_id,
                hardware: voter.hardware.clone(),
                storage_device: voter.storage_device.clone(),
                network_path: voter.network_path.clone(),
            })
            .collect::<Vec<_>>();
        exercise_topology(
            &mut cluster,
            voter_count,
            workload,
            workload_sha256,
            RunEvidence {
                controller_host: &config.controller_host,
                load_generator_host: &config.load_generator_host,
                resources: Some(&identities),
            },
        )
        .await
    }
    .await;

    let shutdown = if result.is_ok() {
        cluster.shutdown_all().await
    } else {
        cluster.kill_all().await;
        Ok(())
    };
    let cleanup = finish_remote_cleanup(&cleanups, cleanup_manifest).await;
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
    launch: &NodeLaunch,
) -> Result<NodeProcess> {
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteCleanup {
    ssh_host: String,
    container_name: String,
    remote_root: String,
    image_digest: String,
    node_id: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CleanupManifest {
    schema_version: u32,
    owner_nonce: String,
    cleanups: Vec<RemoteCleanup>,
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
    let owner_path = path.join(OUTPUT_OWNER_FILENAME);
    let owner_metadata =
        std::fs::symlink_metadata(&owner_path).context("inspect named-runner owner marker")?;
    if !owner_metadata.file_type().is_file()
        || owner_metadata.file_type().is_symlink()
        || owner_metadata.permissions().mode() & 0o077 != 0
        || std::fs::read_to_string(&owner_path)?.trim() != owner_nonce
    {
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
        || !cleanup.container_name.starts_with("plurx-named-p")
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
    let manifest: CleanupManifest = serde_json::from_slice(
        &std::fs::read(path)
            .with_context(|| format!("read named-runner cleanup manifest {}", path.display()))?,
    )?;
    if manifest.schema_version != 1 || manifest.cleanups.is_empty() || manifest.cleanups.len() > 4 {
        bail!("invalid named-runner cleanup manifest shape");
    }
    validate_owner_nonce(&manifest.owner_nonce)?;
    let output_dir = path
        .parent()
        .context("cleanup manifest has no output directory")?;
    let owner_path = output_dir.join(OUTPUT_OWNER_FILENAME);
    if std::fs::read_to_string(&owner_path)?.trim() != manifest.owner_nonce {
        bail!("cleanup manifest does not belong to the active output owner");
    }
    for cleanup in &manifest.cleanups {
        validate_cleanup(cleanup)?;
    }
    finish_remote_cleanup(&manifest.cleanups, path).await
}

async fn finish_remote_cleanup(cleanups: &[RemoteCleanup], manifest: &Path) -> Result<()> {
    cleanup_remote(cleanups).await?;
    std::fs::remove_file(manifest)
        .with_context(|| format!("remove cleanup manifest {}", manifest.display()))?;
    Ok(())
}

async fn cleanup_remote(cleanups: &[RemoteCleanup]) -> Result<()> {
    let mut errors = Vec::new();
    for cleanup in cleanups {
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
            continue;
        }
        // The exact container and nonce path were predeclared by this run. The
        // container removal is idempotent because a clean shutdown used --rm.
        if let Err(error) = ssh_output(
            &cleanup.ssh_host,
            &["docker", "rm", "-f", &cleanup.container_name],
        )
        .await
        {
            let text = format!("{error:#}");
            if !text.contains("No such container") {
                errors.push(text);
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
        }
        if let Err(error) =
            ssh_output(&cleanup.ssh_host, &["rmdir", "--", &cleanup.remote_root]).await
        {
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
    if config.schema_version != NAMED_CAMPAIGN_SCHEMA_VERSION {
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
    if (config.confidence_half_width_percent - 5.0).abs() > f64::EPSILON {
        bail!("named runner confidence half-width target must be 5 percent");
    }
    for value in [
        &config.runner_id,
        &config.image,
        &config.controller_host,
        &config.load_generator_host,
        &config.load_generator_isolation,
    ] {
        if value.trim().is_empty() {
            bail!("named-runner identity fields must not be empty");
        }
    }
    if config.controller_host != config.load_generator_host {
        bail!(
            "named workload runs in the external controller process, so controller and load-generator labels must match"
        );
    }
    for voter in &config.voters {
        for value in [
            &voter.ssh_host,
            &voter.advertised_host,
            &voter.label,
            &voter.hardware,
            &voter.storage_device,
            &voter.network_path,
        ] {
            if value.trim().is_empty() {
                bail!("named voter {} has an empty identity field", voter.node_id);
            }
        }
        for value in [&voter.ssh_host, &voter.advertised_host] {
            if !safe_host_token(value) {
                bail!("named voter {} has an unsafe host token", voter.node_id);
            }
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
    !repository.is_empty()
        && repository
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && repository
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._/:-".contains(&byte))
        && !repository.contains("..")
        && !repository.contains("//")
        && repository
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('-'))
        && tag.len() == 40
        && tag
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

async fn verify_deployed_image(
    config: &NamedRunnerConfig,
    build_sha: &str,
) -> Result<DeploymentProof> {
    let mut command = Command::new("docker");
    command
        .args([
            "image",
            "inspect",
            "--format",
            "{{.Id}} {{.Architecture}} {{.Os}}",
            &config.image,
        ])
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
    let local = String::from_utf8(local.stdout)?;
    let mut fields = local.split_whitespace();
    let digest = fields
        .next()
        .context("controller image omitted digest")?
        .to_owned();
    let architecture = fields
        .next()
        .context("controller image omitted architecture")?
        .to_owned();
    let image_os = fields.next().context("controller image omitted OS")?;
    if fields.next().is_some()
        || image_os != "linux"
        || !matches!(architecture.as_str(), "amd64" | "arm64")
    {
        bail!("named-runner image must be native Linux amd64 or arm64");
    }
    validate_image_digest(&digest)?;
    let controller_identity = local_machine_identity().await?;
    let mut remote_identities = Vec::with_capacity(config.voters.len());
    for voter in &config.voters {
        let remote = ssh_output(
            &voter.ssh_host,
            &[
                "docker",
                "image",
                "inspect",
                "--format",
                "{{.Id}} {{.Architecture}} {{.Os}}",
                &config.image,
            ],
        )
        .await?;
        if remote.trim() != format!("{digest} {architecture} linux") {
            bail!(
                "named voter {} does not have the controller image identity",
                voter.node_id
            );
        }
        let machine_architecture = ssh_output(&voter.ssh_host, &["uname", "-m"]).await?;
        if normalize_machine_architecture(machine_architecture.trim())? != architecture {
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
        image_digest: digest,
        native_architecture: architecture,
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

fn summarize_metrics(
    artifacts: &[ClusterTopologyArtifact],
    target_half_width_percent: f64,
) -> Result<Vec<PairedMetric>> {
    let extractors: [(&str, &str, fn(&TopologyRun) -> Result<f64>); 4] = [
        ("acknowledged_write_p99", "microseconds", |run| {
            Ok(run.acknowledged_write_round_trip_p99_us)
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
        _ => bail!("named campaign only supports 2--7 pairs"),
    }
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
    if campaign.build_sha.len() != 40
        || !campaign
            .build_sha
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("named campaign build SHA must be complete");
    }
    if !safe_image_reference(&campaign.image)
        || !image_is_pinned_to_sha(&campaign.image, &campaign.build_sha)
    {
        bail!("named campaign image is not pinned to its build SHA");
    }
    validate_image_digest(&campaign.image_digest)?;
    if !matches!(campaign.native_architecture.as_str(), "amd64" | "arm64")
        || campaign.controller_machine_fingerprint.len() != 64
        || campaign.voter_machine_fingerprints.len() != 4
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
    if [
        &campaign.runner_id,
        &campaign.controller_host,
        &campaign.load_generator_host,
        &campaign.load_generator_isolation,
    ]
    .into_iter()
    .any(|value| value.trim().is_empty())
    {
        bail!("named campaign identity fields must not be empty");
    }
    if campaign.voter_labels.len() != 4
        || campaign
            .voter_labels
            .iter()
            .any(|label| label.trim().is_empty())
        || campaign
            .voter_labels
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != 4
    {
        bail!("named campaign must identify four distinct runner nodes");
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
                run.controller_host != campaign.controller_host
                    || run.load_generator_host != campaign.load_generator_host
            }) {
                bail!("named raw pair controller placement does not match its campaign");
            }
            artifacts.push(artifact);
        }
    }
    let expected_metrics = [
        ("acknowledged_write_p99", "microseconds"),
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
        bail!("named campaign must report all four pre-registered estimands");
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
    }

    #[test]
    fn cleanup_scope_refuses_options_and_path_components() {
        use std::os::unix::fs::PermissionsExt;

        let valid = RemoteCleanup {
            ssh_host: "runner-1".to_owned(),
            container_name: "plurx-named-p1-v3-n1".to_owned(),
            remote_root: format!("{REMOTE_ROOT_PREFIX}{}-p1-v3-n1", "a".repeat(64)),
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
                container_name: "plurx-named-p1-v3-n1".to_owned(),
                remote_root: format!("{REMOTE_ROOT_PREFIX}{}-p1-v3-n1", "a".repeat(64)),
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
    fn campaign_schema_and_rust_validator_accept_the_same_summary_shape() {
        let samples3 = vec![100.0, 101.0, 99.0];
        let samples4 = vec![110.0, 111.1, 108.9];
        let metrics = [
            ("acknowledged_write_p99", "microseconds"),
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
            schema_version: 1,
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

        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../../benchmarks/cluster-topology-campaign.schema.json"
        ))
        .expect("campaign schema JSON");
        let validator = jsonschema::validator_for(&schema).expect("campaign schema validator");
        let json = serde_json::to_value(campaign).expect("campaign JSON");
        assert!(validator.is_valid(&json));
    }
}
