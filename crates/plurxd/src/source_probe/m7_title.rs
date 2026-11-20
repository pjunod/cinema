//! M7 experiment lifecycle owned by a maintenance title attempt.

use super::m7_comparison::{HeldTitle, TitleContext};
use crate::fragment_index_cluster::{open_source_fence, SourceFence};
use plurx_core::{
    process::{bounded, source_lifetime},
    transcode::dv_m7_receipt::M7SourceProbe,
};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};
use tokio::io::AsyncReadExt;

const MAX_HISTORICAL_JSON_BYTES: u64 = 4 * 1024 * 1024;
const HASH_CHUNK: usize = 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoricalResult {
    schema: u32,
    origin: HistoricalOrigin,
    source_sha256: String,
    protocol_sha256: String,
    report_sha256: String,
    request_sha256: String,
    evidence_sha256: String,
    disposition: HistoricalDisposition,
    frozen_margin: f64,
    scorer_sha256: String,
    helper_sha256: String,
    encoder_sha256: String,
    fit_table_sha256: String,
    authored_sha256: String,
    candidate_attempts: u32,
    model_gain: BTreeMap<String, f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum HistoricalOrigin {
    HistoricalOffline,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum HistoricalDisposition {
    Refused,
}

fn digest_text(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|ch| ch.is_ascii_digit() || (b'a'..=b'f').contains(&ch))
}

fn load_json(path: &Path, expected: Option<&str>) -> Result<Vec<u8>, String> {
    let file = plurx_core::fs_secure::open_read_nofollow_blocking(path)
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_HISTORICAL_JSON_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_HISTORICAL_JSON_BYTES
        || expected.is_some_and(|hash| hex::encode(Sha256::digest(&bytes)) != hash)
    {
        return Err("M7 experiment evidence length or digest changed".into());
    }
    Ok(bytes)
}

fn read_historical(path: &Path, source: &str, protocol: &str) -> Result<HistoricalResult, String> {
    let raw = load_json(&path.join("manifest.json"), None)?;
    let result: HistoricalResult = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    if result.schema != 1
        || result.source_sha256 != source
        || result.protocol_sha256 != protocol
        || !result.frozen_margin.is_finite()
        || result.frozen_margin < 0.0
        || result.candidate_attempts == 0
        || result.model_gain.len() != 4
        || !["A", "B", "C", "D"].into_iter().all(|name| {
            result
                .model_gain
                .get(name)
                .is_some_and(|gain| gain.is_finite())
        })
        || [
            &result.report_sha256,
            &result.request_sha256,
            &result.evidence_sha256,
            &result.scorer_sha256,
            &result.helper_sha256,
            &result.encoder_sha256,
            &result.fit_table_sha256,
            &result.authored_sha256,
        ]
        .into_iter()
        .any(|hash| !digest_text(hash))
    {
        return Err("invalid historical M7 experiment manifest".into());
    }
    let protocol = load_json(&path.join("frozen-protocol.json"), Some(protocol))?;
    let report = load_json(
        &path.join("comparison-result.json"),
        Some(&result.report_sha256),
    )?;
    load_json(
        &path.join("comparison-request.json"),
        Some(result.request_sha256.as_str()),
    )?;
    load_json(
        &path.join("verified-artifacts.json"),
        Some(&result.evidence_sha256),
    )?;
    let protocol: serde_json::Value =
        serde_json::from_slice(&protocol).map_err(|e| e.to_string())?;
    let report: serde_json::Value = serde_json::from_slice(&report).map_err(|e| e.to_string())?;
    if protocol["source"]["sha256"].as_str() != Some(source)
        || protocol["status"].as_str() != Some("frozen-before-candidate")
        || protocol["acceptance"]["minimum_mean_delta_itp_gain"].as_f64()
            != Some(result.frozen_margin)
        || report["status"].as_str() != Some("refused")
        || report["scorer_sha256"].as_str() != Some(result.scorer_sha256.as_str())
        || report["request_sha256"].as_str() != Some(result.request_sha256.as_str())
        || report["served_publication"]["curve_digest"].as_str()
            != Some(result.fit_table_sha256.as_str())
        || report["artifacts"]["authored"]["sha256"].as_str()
            != Some(result.authored_sha256.as_str())
        || result.model_gain.iter().any(|(name, gain)| {
            report["gates"][name]["window_and_pooled_mean_gain"].as_f64() != Some(*gain)
        })
    {
        return Err("historical M7 protocol and report disagree with their manifest".into());
    }
    Ok(result)
}

async fn historical(
    root: PathBuf,
    content_sha256: [u8; 32],
) -> Result<Option<HistoricalResult>, String> {
    let source = hex::encode(content_sha256);
    tokio::task::spawn_blocking(move || {
        let dir = root.join("experiments").join(&source);
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let mut results = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            if let Some(name) = name.to_str().filter(|name| digest_text(name)) {
                let path = entry.path();
                if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                    return Err("invalid M7 experiment registry entry".into());
                }
                if path.join("manifest.json").is_file() {
                    results.push(read_historical(&path, &source, name)?);
                }
            }
        }
        results.sort_unstable_by(|left, right| left.protocol_sha256.cmp(&right.protocol_sha256));
        Ok(results.into_iter().next())
    })
    .await
    .map_err(|error| error.to_string())?
}

async fn held_title(context: &TitleContext<'_>) -> Result<HeldTitle, String> {
    let source = open_source_fence(context.file, None).await?;
    let probe = M7SourceProbe::from_probe_json(&context.demand.probe_json)
        .map_err(|error| error.to_string())?;
    let hash = super::run_source_admitted_probe(
        &context.state.transcode,
        context.admission,
        context.cancel,
        context.shutdown,
        None,
        context.fence,
        || async {
            source.unchanged()
                && context.tools.is_current().await
                && source.reopen(context.file).await.is_ok()
        },
        raw_sha256(context, &source),
    )
    .await?;
    if context.cancel.is_cancelled()
        || !source.unchanged()
        || source.reopen(context.file).await.is_err()
    {
        return Err("M7 source changed while measuring its content".into());
    }
    Ok(HeldTitle {
        source,
        content_sha256: hash,
        probe_sha256: probe.sha256(),
    })
}

async fn raw_sha256(context: &TitleContext<'_>, source: &SourceFence) -> Result<[u8; 32], String> {
    // Historical experiment identities are raw complete-file SHA-256. The
    // fragment-index attestation intentionally uses a different hash domain.
    let reopened = source.reopen(context.file).await?;
    let _file = source_lifetime::register_daemon_file(&reopened.handle, &context.file.path)
        .map_err(|error| error.to_string())?;
    let mut reader = tokio::fs::File::from_std(reopened.handle);
    let budget = context.fence.source_budget();
    let mut remaining = u64::try_from(context.file.size).map_err(|e| e.to_string())?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0; HASH_CHUNK];
    while remaining > 0 {
        bounded::check_cancellation().map_err(|e| e.to_string())?;
        let wanted = remaining.min(HASH_CHUNK as u64) as usize;
        let allowed = budget.allowance(wanted).await?;
        let count = reader
            .read(&mut buffer[..allowed])
            .await
            .map_err(|e| e.to_string())?;
        if count == 0 {
            return Err("M7 source ended before its catalogue size".into());
        }
        digest.update(&buffer[..count]);
        remaining -= count as u64;
    }
    Ok(digest.finalize().into())
}

pub(super) async fn evaluate_history(context: &TitleContext<'_>) -> Result<(), String> {
    if context.demand.playback_requested
        || context.admission.is_probe_only()
        || !plurx_core::playback::file_can_convert_to_p81(context.file)
        || context.file.dolby_vision.profile != Some(7)
    {
        return Ok(());
    }
    let Some(root) = context.state.m7_experiment_dir.clone() else {
        return Ok(());
    };
    let title = held_title(context).await?;
    if let Some(result) = historical(root, title.content_sha256).await? {
        tracing::info!(
            file_id = context.file.id,
            origin = ?result.origin,
            disposition = ?result.disposition,
            source_sha256 = %result.source_sha256,
            probe_sha256 = %hex::encode(title.probe_sha256),
            protocol_sha256 = %result.protocol_sha256,
            report_sha256 = %result.report_sha256,
            scorer_sha256 = %result.scorer_sha256,
            helper_sha256 = %result.helper_sha256,
            encoder_sha256 = %result.encoder_sha256,
            table_sha256 = %result.fit_table_sha256,
            margin = result.frozen_margin,
            candidate_attempts = result.candidate_attempts,
            "retained historical M7 experiment"
        );
    }
    Ok(())
}
