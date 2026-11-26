//! M7 experiment lifecycle owned by a maintenance title attempt.

use super::m7_comparison::{HeldTitle, TitleContext};
use crate::fragment_index_cluster::{open_source_fence, SourceFence};
use plurx_core::{
    process::bounded,
    store::dv_m7_attempts::{M7CandidateAnchor, M7CandidatePlan, M7CandidateScope},
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

#[derive(Debug, thiserror::Error)]
pub(super) enum EvaluationError {
    #[error("unsupported M7 source: {0}")]
    Unsupported(String),
    #[error("M7 candidate inconclusive: {0}")]
    Inconclusive(String),
    #[error("M7 deferred: {0}")]
    Deferred(String),
}

impl From<String> for EvaluationError {
    fn from(error: String) -> Self {
        Self::Deferred(error)
    }
}

/// A scanner refusal of the unchanged source under current tools ends the
/// title as unsupported; any other scan failure is retried.
impl From<dv_title::CensusScanError> for EvaluationError {
    fn from(error: dv_title::CensusScanError) -> Self {
        match error {
            dv_title::CensusScanError::Refused(message) => Self::Unsupported(message),
            dv_title::CensusScanError::Transient(message) => Self::Deferred(message),
        }
    }
}

impl From<&str> for EvaluationError {
    fn from(error: &str) -> Self {
        Self::Deferred(error.into())
    }
}

/// One step of an evaluation; `Ok` reaches the title's M7 result.
pub(super) type EvaluationResult = Result<(), EvaluationError>;

/// How an M7 evaluation that neither deferred nor was refused ended.
/// [`EvaluationError::Unsupported`] and [`EvaluationError::Inconclusive`] are
/// the refused endings, [`EvaluationError::Deferred`] the retry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Evaluated {
    /// M7 did not run on this attempt: a viewer's or probe-only admission,
    /// or no experiment directory. Nothing is recorded.
    Skipped,
    /// Not an M7 title: no P8.1 conversion, not Profile 7, or not a
    /// consistent FEL title. Recorded and never reopened.
    NotApplicable,
    /// A historical or repetition-alias result, or a measured, reused or
    /// recovered one. Recorded and never reopened.
    Result,
}

pub(super) type Evaluation = Result<Evaluated, EvaluationError>;

/// The M7 identity a title's outcome is recorded under: the M7 sources this
/// daemon was built from (`PLURX_M7_SOURCE_SHA256`, build.rs) and every
/// installed artifact M7 runs. A change to either reopens the titles an
/// older identity refused.
pub(super) fn identity(tools: &crate::dv_tools::ToolSet) -> String {
    identity_of(env!("PLURX_M7_SOURCE_SHA256"), &tools.m7_tools_digest())
}

fn identity_of(source_sha256: &str, tools_digest: &str) -> String {
    let mut hash = Sha256::new();
    for part in [
        b"plurx-m7-identity-v1".as_slice(),
        source_sha256.as_bytes(),
        tools_digest.as_bytes(),
    ] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part);
    }
    hex::encode(hash.finalize())
}

/// The record an evaluation's ending publishes with the title, if any.
pub(super) fn outcome_record(
    evaluation: &Evaluation,
    identity: &str,
) -> Option<plurx_core::store::dv_m7_outcomes::M7TitleOutcome> {
    use plurx_core::store::dv_m7_outcomes::{M7OutcomeKind, M7TitleOutcome};
    let (outcome, gate) = match evaluation {
        Ok(Evaluated::Skipped) | Err(EvaluationError::Deferred(_)) => return None,
        Ok(Evaluated::NotApplicable) => (M7OutcomeKind::NotApplicable, None),
        Ok(Evaluated::Result) => (M7OutcomeKind::Result, None),
        Err(EvaluationError::Unsupported(gate)) => (M7OutcomeKind::Unsupported, Some(gate)),
        Err(EvaluationError::Inconclusive(gate)) => (M7OutcomeKind::Inconclusive, Some(gate)),
    };
    Some(M7TitleOutcome {
        identity: identity.to_owned(),
        fit_version: plurx_core::transcode::dv_refit::FIT_VERSION.to_owned(),
        outcome,
        // A gate is never empty; the store refuses one.
        gate: gate.map(|gate| match M7TitleOutcome::bounded_gate(gate) {
            bounded if bounded.is_empty() => "unspecified".to_owned(),
            bounded => bounded,
        }),
    })
}

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
        whole_source_census: None,
    })
}

async fn raw_sha256(context: &TitleContext<'_>, source: &SourceFence) -> Result<[u8; 32], String> {
    // Historical experiment identities are raw complete-file SHA-256. The
    // fragment-index attestation intentionally uses a different hash domain.
    // An in-process read of library media under the source fence, which
    // already proved the reopened handle is the catalogued file. No child
    // receives it, so it is not a daemon-owned asset: registering it as one
    // is refused whenever reader roots are configured (library media never
    // sits under the daemon's owned roots) and deferred every M7 title.
    let reopened = source.reopen(context.file).await?;
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

pub(super) async fn evaluate(
    context: &TitleContext<'_>,
    stats: &plurx_core::scan::dv_title::DvTitleStats,
) -> Evaluation {
    if context.demand.playback_requested || context.admission.is_probe_only() {
        return Ok(Evaluated::Skipped);
    }
    let Some(root) = context.state.m7_experiment_dir.clone() else {
        return Ok(Evaluated::Skipped);
    };
    if !plurx_core::playback::file_can_convert_to_p81(context.file)
        || context.file.dolby_vision.profile != Some(7)
    {
        return Ok(Evaluated::NotApplicable);
    }
    let title = held_title(context).await?;
    if let Some(result) = historical(root.clone(), title.content_sha256).await? {
        anchor_historical(context, &title, &result.protocol_sha256).await?;
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
        return Ok(Evaluated::Result);
    }
    if let Some(alias) = repetition(root.clone(), title.content_sha256).await? {
        anchor_historical(context, &title, &alias.protocol_sha256).await?;
        tracing::info!(
            file_id=context.file.id,
            fixture_sha256=%alias.fixture_sha256, source_sha256=%alias.source_sha256,
            protocol_sha256=%alias.protocol_sha256, repeat_count=alias.repeat_count,
            "physical repeat fixture shares the sealed M7 source experiment"
        );
        return Ok(Evaluated::Result);
    }
    use plurx_core::scan::dv_title::{TitleConstant, TitleElKind};
    if stats.el_kind != TitleConstant::Consistent(TitleElKind::Fel) {
        return Ok(Evaluated::NotApplicable);
    }
    // The new-title measurement is a ~110 KB future in debug builds; nested by
    // value under title work it overflowed a 2 MiB runtime worker stack.
    Box::pin(measure_new_title(context, title, &root, stats))
        .await
        .map(|()| Evaluated::Result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Repetition {
    schema: u32,
    fixture_sha256: String,
    source_sha256: String,
    protocol_sha256: String,
    repeat_count: u32,
    proof_sha256: String,
}

async fn repetition(root: PathBuf, content: [u8; 32]) -> Result<Option<Repetition>, String> {
    tokio::task::spawn_blocking(move || {
        let found = hex::encode(content);
        let path = root.join("repetitions").join(&found);
        if !path.exists() {
            return Ok(None);
        }
        let alias: Repetition =
            serde_json::from_slice(&load_json(&path.join("manifest.json"), None)?)
                .map_err(string)?;
        if alias.schema != 1
            || alias.fixture_sha256 != found
            || alias.source_sha256 == found
            || alias.repeat_count < 2
            || [
                &alias.source_sha256,
                &alias.protocol_sha256,
                &alias.proof_sha256,
            ]
            .into_iter()
            .any(|hash| !digest_text(hash))
        {
            return Err("invalid physical repetition experiment receipt".into());
        }
        let proof_raw = load_json(
            &path.join("fixture-provenance.json"),
            Some(&alias.proof_sha256),
        )?;
        let proof: serde_json::Value = serde_json::from_slice(&proof_raw).map_err(string)?;
        if proof["fixture_sha256"].as_str() != Some(alias.fixture_sha256.as_str())
            || proof["source_sha256"].as_str() != Some(alias.source_sha256.as_str())
            || proof["packet_bytes_exact"].as_bool() != Some(true)
            || proof["repeat_count"].as_u64() != Some(u64::from(alias.repeat_count))
        {
            return Err("physical repetition proof differs from its experiment receipt".into());
        }
        read_historical(
            &root
                .join("experiments")
                .join(&alias.source_sha256)
                .join(&alias.protocol_sha256),
            &alias.source_sha256,
            &alias.protocol_sha256,
        )?;
        Ok(Some(alias))
    })
    .await
    .map_err(string)?
}

mod publication;
mod recovery;

use super::{
    dv_title::{self, M7WindowRange},
    m7_candidate::{prepare_authored, FrozenRecipe, PrepareError, RatePlan, StaticHdr},
    m7_comparison::{self, ComparisonInput, EvaluationWindow, FrozenProtocol, PublicationReadback},
};
use plurx_core::transcode::{
    dv_m7_receipt::{CalibrationWindow, MODEL_VERSION},
    dv_m7_scoring::{
        active_area,
        aggregation::FrozenMargin,
        capture::{
            score_baseline_frame, supported_raster, CaptureLayout, CapturePlan, CaptureRole,
            FrameEvidence,
        },
    },
    Rational,
};
use publication::FittedPublication;
use serde::Serialize;
use std::{io::Write, sync::Arc};
use tempfile::{NamedTempFile, TempDir};

const AUTOMATIC_PROTOCOL: &str = "plurx-m7-early-rap-window-calibration-v1";
const FRAMES: usize = 48;
const BASELINE_WINDOWS: usize = 2;
const SOURCE_RATE_BAND_PERCENT: u32 = 1;
const MIN_AUTOMATIC_MARGIN: f64 = 0.05;

async fn measure_new_title(
    context: &TitleContext<'_>,
    mut title: HeldTitle,
    root: &Path,
    stats: &plurx_core::scan::dv_title::DvTitleStats,
) -> EvaluationResult {
    // A sealed source experiment remains terminal even if its current
    // catalogue receipt was interrupted or its original helper is retired.
    if let Some(completion) = recovery::find(root, title.content_sha256).await? {
        return completion.reconcile(context, &title).await;
    }
    let source_probe =
        M7SourceProbe::from_probe_json(&context.demand.probe_json).map_err(string)?;
    let scope = candidate_scope(context, &title);
    // F+ demand expects complete title facts. Publishing only the aggregate
    // lets the existing fitter work while this title claim remains retryable.
    if plurx_core::scan::dv_title::DvTitleStats::from_probe_json(&context.demand.probe_json)
        != Some(*stats)
        && !context
            .fence
            .record_m7_title_facts(&source_probe, &scope, stats)
            .await
            .map_err(string)?
    {
        return Err(EvaluationError::Deferred(
            "M7 prerequisite title facts lost their publication authority".into(),
        ));
    }
    let scratch = tempfile::Builder::new()
        .prefix("m7-title-")
        .tempdir_in(&context.state.runtime_cache_dir)
        .map_err(string)?;
    let expected_version = title.source.object_version().to_owned();
    let prepared = Box::pin(super::run_source_admitted_probe(
        &context.state.transcode,
        context.admission,
        context.cancel,
        context.shutdown,
        None,
        context.fence,
        || async {
            context.tools.is_current().await
                && crate::fragment_index_cluster::inspect_source(context.file)
                    .await.is_ok_and(|version| version == expected_version)
        },
        async {
            let fitted = FittedPublication::load(context).await?;
            // Inside the admitted, version-checked source run: the frame
            // observation is of the same held object the census reads.
            let st2086_frames = undeclared_st2086_frames(context, &title).await?;
            let mut source = source_plan(context, &title, &fitted, st2086_frames.as_deref())?;
            let reserve_bytes = m7_comparison::title_scratch_bytes(source.layout).map_err(string)?;
            let reservation = crate::playback_cache::reserve(scratch.path(), reserve_bytes)
                .await.map_err(string)?;
            let (_, census) = dv_title::scan_with_m7_census(
                context.file, &context.demand.probe_json, context.tools,
                &context.state.runtime_cache_dir, &source.windows,
            ).await?;
            title.whole_source_census = Some(census);
            let verified = title.whole_source_census.as_ref().ok_or("M7 title census disappeared")?;
            let (coded_bytes, presentation_ticks, rate_clock) = verified.coded_video_extent();
            if rate_clock != source.layout.time_base {
                return Err(EvaluationError::Deferred(
                    "M7 whole-source rate and window census clocks differ".into(),
                ));
            }
            let rate = measured_source_rate_plan(coded_bytes, presentation_ticks, rate_clock)
                .map_err(EvaluationError::Unsupported)?;
            let recipe = FrozenRecipe::new(source.cadence, source.static_hdr, rate)
                .map_err(|error| EvaluationError::Unsupported(error.to_string()))?;
            let windows = source.windows.iter().map(|range| {
                verified.window(&title.source, context.tools, source.layout, range.first_pts)
                .filter(|frames| frames.len() == FRAMES)
                .map(<[FrameEvidence]>::to_vec)
                .ok_or("M7 window does not contain the frozen 48-frame exposure")
            }).collect::<Result<Vec<_>, _>>()?;
            let publications = fitted.readbacks(context, &title, &windows, scratch.path()).await?;
            source.layout.roi = publication_active_area(&publications, source.layout.raster)?;
            source.layout.sites_per_frame().map_err(string)?;
            let mut calibration = Vec::new();
            let mut evaluation = Vec::new();
            for (index, (frames, (whole, seek))) in windows.into_iter().zip(publications).enumerate() {
                if index < BASELINE_WINDOWS {
                    calibration.push(calibrate(
                        context, &title, source.layout, &frames, &whole, &seek,
                        &fitted, &scratch, reserve_bytes,
                    ).await?);
                } else {
                    evaluation.push((frames, whole, seek));
                }
            }
            let margin = frozen_margin(&calibration)?;
            let table_sha: [u8; 32] = hex::decode(fitted.table().digest())
                .map_err(string)?.try_into().map_err(|_| "M7 fitted digest length changed")?;
            let recipe_sha = recipe.recipe_sha256();
            let freeze = serde_json::to_vec(&serde_json::json!({
                "schema": AUTOMATIC_PROTOCOL,
                "source_sha256": hex::encode(title.content_sha256),
                "probe_sha256": hex::encode(title.probe_sha256),
                "model": MODEL_VERSION,
                "fitted_table_sha256": hex::encode(table_sha),
                "margin": margin.value(),
                "calibration": calibration,
                "evaluation": evaluation.iter().map(|(frames, whole, seek)| serde_json::json!({
                    "frames": frames.iter().map(|frame| serde_json::json!({
                        "ordinal": frame.ordinal, "pts": frame.pts, "duration": frame.duration,
                        "au": hex::encode(frame.access_unit_sha256),
                        "rpu": hex::encode(frame.rpu_sha256), "vcl": hex::encode(frame.bl_vcl_sha256),
                    })).collect::<Vec<_>>(),
                    "whole": hex::encode(whole.readback_sha256),
                    "seek": hex::encode(seek.readback_sha256),
                })).collect::<Vec<_>>(),
                "layout": {"raster": source.layout.raster, "roi": source.layout.roi,
                  "time_base": source.layout.time_base, "video_index": source.layout.video_index},
                "source_video_rate": {
                    "coded_packet_bytes": coded_bytes,
                    "presentation_ticks": presentation_ticks,
                    "time_base": rate_clock,
                },
                "recipe_sha256": hex::encode(recipe_sha),
                "recipe": &recipe,
            })).map_err(string)?;
            let protocol_sha: [u8; 32] = Sha256::digest(&freeze).into();
            let directory = root.join("experiments")
                .join(hex::encode(title.content_sha256)).join(hex::encode(protocol_sha));
            let plan = M7CandidatePlan {
                protocol_sha256: protocol_sha,
                recipe_sha256: recipe_sha,
                maximum_attempts: rate.maximum_attempts,
            };
            match context.fence.anchor_m7_candidate(&source_probe, &scope, plan).await.map_err(string)? {
                M7CandidateAnchor::Ready => {}
                M7CandidateAnchor::DifferentProtocol => return Err(EvaluationError::Inconclusive(
                    "this source already has a different frozen experiment".into(),
                )),
                M7CandidateAnchor::Fenced => return Err(EvaluationError::Deferred(
                    "M7 protocol anchor lost its title authority".into(),
                )),
            }
            immutable_write_owned(directory.clone(), "native-protocol.json", freeze).await?;
            let frozen = FrozenProtocol {
                source_sha256: title.content_sha256, probe_sha256: title.probe_sha256,
                protocol_sha256: protocol_sha, recipe_sha256: recipe_sha,
                fit_table_sha256: table_sha, margin, encoder_rate: source.cadence,
                calibration: calibration.into_iter().map(|item| item.window).collect(),
            };
            let mut prepared_windows = Vec::with_capacity(evaluation.len());
            for (frames, whole, seek) in evaluation {
                let capture = CapturePlan { layout: source.layout, frames: &frames };
                let planes = m7_comparison::prepare_planes(
                    context, &title, capture, &scratch, reserve_bytes,
                ).await.map_err(string)?;
                for frame in &planes.frames {
                    if active_area(&frame.source_rpu, source.layout.raster).map_err(string)?
                        != source.layout.roi
                    {
                        return Err(EvaluationError::Unsupported(
                            "M7 original evaluation RPU active area disagrees with fitted readback".into(),
                        ));
                    }
                }
                let authored = prepare_authored(
                    context, &title, &frozen, &recipe, planes, source.dv_level,
                    &scratch, &directory, context.cancel,
                ).await.map_err(candidate_error)?;
                prepared_windows.push(EvaluationWindow {
                    layout: source.layout, source_frames: frames, authored, whole, seek,
                });
            }
            Ok::<_, EvaluationError>((directory, ComparisonInput {
                title, frozen, windows: prepared_windows, scratch_bytes: reserve_bytes,
            }, reservation))
        },
    )).await?;
    let (directory, input, _reservation) = prepared;
    let (result, held) = Box::pin(m7_comparison::execute(context, input))
        .await
        .map_err(comparison_error)?;
    match result {
        m7_comparison::ComparisonRun::Reused(receipt) => {
            tracing::info!(file_id=context.file.id, disposition=?receipt.disposition(),
                binding_key=%receipt.binding_key(), "reused exact M7 measurement");
            Ok(())
        }
        m7_comparison::ComparisonRun::Measured(measurement) => {
            let completion = recovery::seal(directory, measurement).await?;
            completion.reconcile(context, &held).await
        }
    }
}

fn candidate_error(error: PrepareError) -> EvaluationError {
    match error {
        PrepareError::RateInconclusive(_)
        | PrepareError::ActualRate
        | PrepareError::IncompleteAttempt { .. } => {
            EvaluationError::Inconclusive(error.to_string())
        }
        _ => EvaluationError::Deferred(error.to_string()),
    }
}

fn comparison_error(error: m7_comparison::Error) -> EvaluationError {
    match error {
        m7_comparison::Error::Content(_) => EvaluationError::Unsupported(error.to_string()),
        _ => EvaluationError::Deferred(error.to_string()),
    }
}

pub(super) fn candidate_scope(context: &TitleContext<'_>, title: &HeldTitle) -> M7CandidateScope {
    M7CandidateScope {
        file_id: context.file.id,
        source_size: context.file.size,
        source_mtime: context.file.mtime,
        probe_revision: context.demand.probe_revision,
        scanner_digest: context.tools.title_scanner_digest(),
        source_sha256: title.content_sha256,
    }
}

async fn anchor_historical(
    context: &TitleContext<'_>,
    title: &HeldTitle,
    protocol: &str,
) -> EvaluationResult {
    let mut digest = [0; 32];
    hex::decode_to_slice(protocol, &mut digest).map_err(string)?;
    let plan = M7CandidatePlan::sealed_historical(digest);
    let probe = M7SourceProbe::from_probe_json(&context.demand.probe_json).map_err(string)?;
    match context
        .fence
        .anchor_m7_candidate(&probe, &candidate_scope(context, title), plan)
        .await
        .map_err(string)?
    {
        M7CandidateAnchor::Ready => Ok(()),
        M7CandidateAnchor::DifferentProtocol => Err(EvaluationError::Deferred(
            "verified historical M7 result conflicts with a different replicated source experiment"
                .into(),
        )),
        M7CandidateAnchor::Fenced => Err(EvaluationError::Deferred(
            "historical M7 source anchor lost its title authority".into(),
        )),
    }
}

struct SourcePlan {
    layout: CaptureLayout,
    cadence: Rational,
    dv_level: u8,
    windows: Vec<M7WindowRange>,
    static_hdr: StaticHdr,
}

/// Domain of the static-HDR observation digest when the ST 2086 values come
/// from the held source's frames rather than the probe document's stream.
/// v2 binds the validated values, not FFprobe's output bytes.
const ST2086_FRAMES_DOMAIN: &[u8] = b"plurx-m7-st2086-frames-v2";
const ST2086_FRAMES_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

fn selected_video_stream(
    document: &serde_json::Value,
) -> Result<(u32, &serde_json::Value), EvaluationError> {
    let video_index =
        crate::decode_facts::first_playable_video_index(document).ok_or_else(|| {
            EvaluationError::Unsupported("M7 source has no selected video stream".into())
        })?;
    let stream = document["streams"]
        .as_array()
        .and_then(|streams| {
            streams
                .iter()
                .find(|stream| stream["index"].as_u64() == Some(u64::from(video_index)))
        })
        .ok_or_else(|| EvaluationError::Unsupported("M7 selected stream is absent".into()))?;
    Ok((video_index, stream))
}

/// The frozen protocol takes its static-HDR authority from the first original
/// source access units. A remux often drops the container MDCV while every
/// HEVC frame still carries the exact SEI, so a stream that declares none is
/// resolved through the bounded, selected-stream frame projection Linux HDR10
/// uses (`held_source_mastering`). A declared or malformed stream needs no
/// frame observation: `static_hdr_authority` takes or refuses it.
async fn undeclared_st2086_frames(
    context: &TitleContext<'_>,
    title: &HeldTitle,
) -> Result<Option<Vec<u8>>, EvaluationError> {
    let document: serde_json::Value =
        serde_json::from_str(&context.demand.probe_json).map_err(string)?;
    let (video_index, stream) = selected_video_stream(&document)?;
    if !matches!(
        crate::decode_facts::mastering::source_st2086(stream),
        Ok(None)
    ) {
        return Ok(None);
    }
    held_source_st2086_frames(context, title, video_index)
        .await
        .map(Some)
        .map_err(FrameProjectionError::into_evaluation)
}

/// Why the frame projection produced no observation.
#[derive(Debug, PartialEq, Eq)]
enum FrameProjectionError {
    /// The projection did not run to completion over the unchanged held
    /// source: launch, deadline, the source read or the source itself.
    Transient(String),
    /// FFprobe ran over the unchanged held source and refused it, or wrote
    /// more than the projection's bound. The same source does it again.
    Deterministic(String),
}

impl FrameProjectionError {
    fn into_evaluation(self) -> EvaluationError {
        match self {
            Self::Transient(error) => {
                EvaluationError::Deferred(format!("M7 source frame MDCV probe: {error}"))
            }
            Self::Deterministic(error) => {
                EvaluationError::Unsupported(format!("M7 source frame MDCV probe: {error}"))
            }
        }
    }
}

/// Classify a completed projection run: a signal is not FFprobe's verdict,
/// an exit status and an oversized output are.
fn frame_projection_verdict(
    status: std::process::ExitStatus,
    stderr: &[u8],
    stdout_len: usize,
    limit: usize,
) -> Result<(), FrameProjectionError> {
    if !status.success() {
        let message = format!(
            "ffprobe exited {status}: {}",
            String::from_utf8_lossy(stderr).trim()
        );
        return Err(if status.code().is_some() {
            FrameProjectionError::Deterministic(message)
        } else {
            FrameProjectionError::Transient(message)
        });
    }
    if stdout_len > limit {
        return Err(FrameProjectionError::Deterministic(
            "frame projection exceeded its output bound".into(),
        ));
    }
    Ok(())
}

/// Run the exact `SourceFrames` FFprobe projection over the title's held
/// source through the job's budgeted background input, as the M7 renderer
/// observations read it. Stdout is bounded to the projection's limit. A
/// launch failure, the deadline, a failed source read or a changed source is
/// transient; FFprobe's own refusal (an exit status) or an oversized output
/// over the unchanged source is deterministic.
async fn held_source_st2086_frames(
    context: &TitleContext<'_>,
    title: &HeldTitle,
    video_index: u32,
) -> Result<Vec<u8>, FrameProjectionError> {
    use plurx_core::process::{source_lifetime, ChildWork};
    use FrameProjectionError::Transient;
    let (arguments, limit) = crate::decode_facts::source_frames_projection(video_index);
    let held = SourceFence::clone_held(&title.source.handle, title.source.object_version())
        .map_err(Transient)?;
    let input = crate::source_input::SourceInput::open_background_held(
        held,
        "application/octet-stream",
        context.fence.source_budget(),
    )
    .await
    .map_err(Transient)?;
    let mut command = tokio::process::Command::new(crate::ffmpeg::ffprobe_bin());
    command.args(&arguments).arg(input.url());
    let output = source_lifetime::mediated(bounded::output_command_captured(
        &mut command,
        ST2086_FRAMES_DEADLINE,
        limit.saturating_add(1),
        ChildWork::background("Dolby Vision M7 source mastering frames"),
    ))
    .await;
    let source_read_failed = input.source_read_failed();
    input
        .close_and_wait()
        .await
        .map_err(|error| Transient(format!("joining source input: {error}")))?;
    let output = output.map_err(|error| Transient(error.to_string()))?;
    if source_read_failed {
        return Err(Transient("held source read failed".into()));
    }
    if !title.source.unchanged() {
        return Err(Transient(
            "held source changed during the frame projection".into(),
        ));
    }
    frame_projection_verdict(output.status, &output.stderr, output.stdout.len(), limit)?;
    Ok(output.stdout)
}

/// The recipe's static HDR and the observation it binds.
///
/// A stream-declared MDCV keeps `source_observation_sha256 = probe_sha256`,
/// so historical and stream-declared sources hash exactly as before. Values
/// resolved from frames bind SHA-256 over a domain-separated, length-prefixed
/// encoding of `ST2086_FRAMES_DOMAIN`, the 32-byte probe digest, and the
/// validated ST 2086 values in a canonical encoding: the recipe names both
/// the document that declared no MDCV and the values the frames supplied, so
/// an FFprobe update that prints the same frames differently is the same
/// observation. Output that is not JSON, or frames carrying no or conflicting
/// MDCV, is the source's own and refuses it.
fn static_hdr_authority(
    stream: &serde_json::Value,
    video_index: u32,
    probe_sha256: [u8; 32],
    st2086_frames: Option<&[u8]>,
) -> Result<StaticHdr, EvaluationError> {
    let absent = || {
        EvaluationError::Unsupported("M7 selected source has no exact ST 2086 declarations".into())
    };
    let ((primaries_rgbw, max_1e4_nits, min_1e4_nits), source_observation_sha256) =
        match crate::decode_facts::mastering::source_st2086(stream)
            .map_err(EvaluationError::Unsupported)?
        {
            Some(declared) => (declared, probe_sha256),
            None => {
                let bytes = st2086_frames.ok_or_else(absent)?;
                let frames: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| {
                    EvaluationError::Unsupported(format!(
                        "M7 source frame MDCV probe JSON: {error}"
                    ))
                })?;
                let observed = crate::decode_facts::mastering::source_st2086_from_frames(
                    stream,
                    &frames,
                    video_index,
                )
                .map_err(EvaluationError::Unsupported)?
                .ok_or_else(absent)?;
                (observed, st2086_frames_observation(probe_sha256, observed))
            }
        };
    Ok(StaticHdr {
        primaries_rgbw,
        max_1e4_nits,
        min_1e4_nits,
        // FEL reconstruction changes the samples to which the input CLL
        // applied. Preserve its mastering declaration without CLL claims.
        max_cll_fall: None,
        source_observation_sha256,
    })
}

/// The canonical ST 2086 encoding is the eight chromaticity coordinates in
/// `source_st2086`'s `primaries_rgbw` order (0.00002 units) as big-endian
/// u16, then the maximum and minimum luminance (0.0001 cd/m2) as big-endian
/// u32.
fn st2086_frames_observation(
    probe_sha256: [u8; 32],
    (primaries_rgbw, max_1e4_nits, min_1e4_nits): ([u16; 8], u32, u32),
) -> [u8; 32] {
    let mut values = Vec::with_capacity(24);
    for primary in primaries_rgbw {
        values.extend_from_slice(&primary.to_be_bytes());
    }
    values.extend_from_slice(&max_1e4_nits.to_be_bytes());
    values.extend_from_slice(&min_1e4_nits.to_be_bytes());
    let mut hash = Sha256::new();
    for part in [ST2086_FRAMES_DOMAIN, &probe_sha256, &values] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part);
    }
    hash.finalize().into()
}

fn source_plan(
    context: &TitleContext<'_>,
    title: &HeldTitle,
    fitted: &FittedPublication,
    st2086_frames: Option<&[u8]>,
) -> Result<SourcePlan, EvaluationError> {
    let document: serde_json::Value =
        serde_json::from_str(&context.demand.probe_json).map_err(string)?;
    let (video_index, stream) = selected_video_stream(&document)?;
    let cadence = stream["avg_frame_rate"]
        .as_str()
        .and_then(|rate| {
            let (num, den) = rate.split_once('/')?;
            Rational::new(num.parse().ok()?, den.parse().ok()?)
        })
        .ok_or_else(|| {
            EvaluationError::Unsupported("M7 requires an observed average frame cadence".into())
        })?;
    let raster =
        ["width", "height"].map(|axis| stream[axis].as_u64().and_then(|n| u16::try_from(n).ok()));
    let [Some(width), Some(height)] = raster else {
        return Err(EvaluationError::Unsupported(
            "M7 source raster is unspecified".into(),
        ));
    };
    let clock = fitted.clock()?;
    let time_base = Rational::new(clock.source_time_base.0, clock.source_time_base.1)
        .ok_or_else(|| "invalid M7 fitted source time base".to_owned())?;
    let layout = CaptureLayout {
        role: CaptureRole::SourceFel,
        video_index,
        time_base,
        raster: [width, height],
        // The definitive region comes from stable whole and seek RPU
        // readback before any lattice capture or candidate preparation.
        roi: [0, 0, width, height],
    };
    if !supported_raster(layout.raster) {
        return Err(EvaluationError::Unsupported(
            "M7 coded raster is outside the managed full-plane recipe".into(),
        ));
    }
    let static_hdr = static_hdr_authority(stream, video_index, title.probe_sha256, st2086_frames)?;
    let dv_level = context.file.dolby_vision.level.ok_or_else(|| {
        EvaluationError::Unsupported("M7 source Dolby Vision level is absent".into())
    })?;
    let dv_level = u8::try_from(dv_level).map_err(string)?;
    let numerator =
        FRAMES as u128 * u128::from(cadence.denominator()) * u128::from(time_base.denominator());
    let denominator = u128::from(cadence.numerator()) * u128::from(time_base.numerator());
    if !numerator.is_multiple_of(denominator) {
        return Err(EvaluationError::Unsupported(
            "M7 exposure is not integral on the fitted source clock".into(),
        ));
    }
    let ticks = i64::try_from(numerator / denominator).map_err(string)?;
    let index = fitted.index();
    let proofs = index
        .dv_refit_proofs
        .as_ref()
        .ok_or_else(|| "canonical M7 fitting proof absent".to_owned())?;
    let mut windows: Vec<M7WindowRange> = Vec::new();
    // Start after the opening two entries so the independent control
    // generation must really seek. Use the first complete disjoint covered
    // RAP exposures in plan order. Packet/census validation decides coverage.
    for entry in fitted.plan().entries.iter().skip(2) {
        let row = plurx_core::segplan::entry_row(index, entry)
            .ok_or_else(|| "M7 plan is outside its index".to_owned())?;
        let start = clock
            .source_pts(i128::from(entry.start_ticks) + i128::from(proofs[row].first_cto()))
            .map_err(string)?;
        let end = start
            .checked_add(ticks)
            .ok_or_else(|| "M7 exposure clock overflow".to_owned())?;
        if windows
            .last()
            .is_some_and(|previous| previous.end_pts > start)
        {
            continue;
        }
        if fitted.table().curves_at(start).is_some() && fitted.table().curves_at(end - 1).is_some()
        {
            windows.push(M7WindowRange {
                first_pts: start,
                end_pts: end,
            });
            if windows.len() == BASELINE_WINDOWS + 1 {
                break;
            }
        }
    }
    if windows.len() != BASELINE_WINDOWS + 1 {
        return Err(EvaluationError::Unsupported(
            "M7 fitted plan lacks two calibration exposures and one evaluation exposure".into(),
        ));
    }
    Ok(SourcePlan {
        layout,
        cadence,
        dv_level,
        windows,
        static_hdr,
    })
}

fn measured_source_rate_plan(
    coded_bytes: u64,
    presentation_ticks: u64,
    time_base: Rational,
) -> Result<RatePlan, String> {
    let bits = u128::from(coded_bytes) * 8 * u128::from(time_base.denominator());
    let seconds = u128::from(presentation_ticks) * u128::from(time_base.numerator());
    let high = bits
        .checked_div(seconds * 1_000)
        .and_then(|kbps| u32::try_from(kbps).ok())
        .filter(|&kbps| kbps > 0)
        .ok_or("M7 selected source has no supported positive coded video rate")?;
    // Truncate the ceiling in integer space to keep every admitted authored
    // rate within the independently observed selected video budget.
    let low = high - high / (100 / SOURCE_RATE_BAND_PERCENT);
    Ok(RatePlan {
        authored_band_kbps: [low, high],
        initial_target_kbps: high,
        maximum_attempts: 4,
    })
}

fn publication_active_area(
    publications: &[(PublicationReadback, PublicationReadback)],
    raster: [u16; 2],
) -> Result<[u16; 4], String> {
    let mut stable = None;
    for (whole, seek) in publications {
        for frame in whole.frames.iter().chain(&seek.frames) {
            let region = active_area(&frame.rpu_nal, raster).map_err(string)?;
            if stable.is_some_and(|old| old != region) {
                return Err("whole or seek RPU active area varies within the M7 experiment".into());
            }
            stable = Some(region);
        }
    }
    stable.ok_or("M7 fitted readback has no active area evidence".into())
}

#[derive(Serialize)]
struct CalibrationEvidence {
    window: CalibrationWindow,
    samples: u64,
    model_means: [f64; 4],
    whole_readback: [u8; 32],
    seek_readback: [u8; 32],
}

#[expect(
    clippy::too_many_arguments,
    reason = "each calibration retains both independent publications and the held source"
)]
async fn calibrate(
    context: &TitleContext<'_>,
    title: &HeldTitle,
    layout: CaptureLayout,
    frames: &[FrameEvidence],
    whole: &PublicationReadback,
    seek: &PublicationReadback,
    fitted: &FittedPublication,
    scratch: &TempDir,
    scratch_bytes: u64,
) -> Result<CalibrationEvidence, String> {
    let plan = CapturePlan { layout, frames };
    let capture =
        m7_comparison::capture_source_lattice(context, title, plan, scratch, scratch_bytes)
            .await
            .map_err(string)?;
    let frames = frames.to_vec();
    let table = Arc::clone(fitted.table());
    let cancel = context.cancel.clone();
    let [whole_hash, seek_hash] = [whole.readback_sha256, seek.readback_sha256];
    let copy_frames = |publication: &PublicationReadback| {
        publication
            .frames
            .iter()
            .map(|frame| {
                (
                    frame.source,
                    frame.bl_vcl_sha256,
                    frame.rpu_sha256,
                    frame.rpu_nal.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    let whole = copy_frames(whole);
    let seek = copy_frames(seek);
    tokio::task::spawn_blocking(move || {
        use plurx_core::transcode::dv_m7_scoring::capture::FplusFrameEvidence;
        let mut reader = capture
            .reader(CapturePlan {
                layout,
                frames: &frames,
            })
            .map_err(string)?;
        let mut samples = 0u64;
        let mut totals = [0.0; 4];
        let mut hash = Sha256::new();
        hash.update(b"plurx/m7/calibration/v1\0");
        hash.update(whole_hash);
        hash.update(seek_hash);
        for ((observed, whole), seek) in frames.iter().zip(whole).zip(seek) {
            let source = reader
                .next_frame()
                .map_err(string)?
                .ok_or("M7 calibration frame missing")?;
            let whole = FplusFrameEvidence {
                source: whole.0,
                bl_vcl_sha256: whole.1,
                rpu_sha256: whole.2,
                rpu_nal: &whole.3,
            };
            let seek = FplusFrameEvidence {
                source: seek.0,
                bl_vcl_sha256: seek.1,
                rpu_sha256: seek.2,
                rpu_nal: &seek.3,
            };
            let curves = table
                .curves_at(observed.pts)
                .ok_or("M7 calibration frame is not fitted")?;
            let (sums, count) =
                score_baseline_frame(&source, &whole, &seek, curves, || cancel.is_cancelled())
                    .map_err(string)?;
            samples += count;
            for (sum, value) in totals.iter_mut().zip(sums) {
                *sum += value;
            }
            hash.update(observed.ordinal.to_be_bytes());
            hash.update(observed.pts.to_be_bytes());
            hash.update(observed.duration.to_be_bytes());
            hash.update(observed.access_unit_sha256);
            hash.update(observed.rpu_sha256);
            hash.update(observed.bl_vcl_sha256);
        }
        let completion = reader.finish().map_err(string)?;
        if samples == 0 || completion.samples != samples {
            return Err("M7 calibration sample census changed".into());
        }
        let model_means = totals.map(|total| total / samples as f64);
        for mean in model_means {
            hash.update(mean.to_bits().to_be_bytes());
        }
        let (first, last) = frames
            .first()
            .zip(frames.last())
            .ok_or("M7 calibration exposure empty")?;
        Ok(CalibrationEvidence {
            window: CalibrationWindow {
                first_ordinal: first.ordinal,
                last_ordinal: last.ordinal,
                first_pts: first.pts,
                end_pts: last
                    .pts
                    .checked_add(last.duration)
                    .ok_or("M7 calibration extent overflow")?,
                time_base: layout.time_base,
                receipt_sha256: hash.finalize().into(),
            },
            samples,
            model_means,
            whole_readback: whole_hash,
            seek_readback: seek_hash,
        })
    })
    .await
    .map_err(string)?
}

fn frozen_margin(evidence: &[CalibrationEvidence]) -> Result<FrozenMargin, String> {
    if evidence.len() != BASELINE_WINDOWS {
        return Err("M7 protocol requires both independent calibration exposures".into());
    }
    let mut margin = MIN_AUTOMATIC_MARGIN;
    for item in evidence {
        let mut means = item.model_means;
        if means.iter().any(|value| !value.is_finite() || *value < 0.0) {
            return Err("M7 source calibration produced invalid errors".into());
        }
        means.sort_by(f64::total_cmp);
        // The four-model spread accounts for representation uncertainty.
        // Rounding up to hundredths is a protocol policy, independent of the
        // candidate, and does not claim to measure encoder repeatability.
        margin = margin.max(((means[3] - means[0]) * 100.0).ceil() / 100.0);
    }
    FrozenMargin::new(margin).map_err(string)
}

async fn immutable_write_owned(
    directory: PathBuf,
    name: &'static str,
    data: Vec<u8>,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || immutable_write(&directory, name, &data))
        .await
        .map_err(string)?
}

fn immutable_write(directory: &Path, name: &str, data: &[u8]) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(string)?;
    let mut staging = NamedTempFile::new_in(directory).map_err(string)?;
    staging.write_all(data).map_err(string)?;
    staging.as_file().sync_all().map_err(string)?;
    let path = directory.join(name);
    if let Err(error) = staging.persist_noclobber(&path) {
        if error.error.kind() != std::io::ErrorKind::AlreadyExists
            || load_json(&path, None)? != data
        {
            return Err(string(error.error));
        }
    }
    sync_registry(directory)
}

fn sync_registry(directory: &Path) -> Result<(), String> {
    // The data directory itself is the last ancestor that can acquire a
    // new entry in this protocol's durable registry.
    for parent in directory.ancestors().take(5) {
        plurx_core::fs_secure::open_directory_nofollow_blocking(parent)
            .and_then(|fd| fd.sync_all())
            .map_err(string)?;
    }
    Ok(())
}

fn string(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "positive fixture clocks and expected automatic rate plans"
)]
mod tests {
    use super::*;

    fn eclipse_mdcv() -> serde_json::Value {
        serde_json::json!({"side_data_type":"Mastering display metadata",
            "red_x":"34000/50000","red_y":"16000/50000","green_x":"13250/50000","green_y":"34500/50000",
            "blue_x":"7500/50000","blue_y":"3000/50000","white_point_x":"15635/50000","white_point_y":"16450/50000",
            "min_luminance":"1/10000","max_luminance":"10000000/10000"})
    }

    #[test]
    fn static_hdr_binds_the_stream_declaration_or_the_exact_frame_observation() {
        const P3: [u16; 8] = [34000, 16000, 13250, 34500, 7500, 3000, 15635, 16450];
        let probe = [7; 32];
        let declared = serde_json::json!({"index":0,"side_data_list":[eclipse_mdcv()]});
        let silent = serde_json::json!({"index":0,"side_data_list":[{"side_data_type":"DOVI configuration record"}]});
        let frame = serde_json::json!({"stream_index":0,"side_data_list":[eclipse_mdcv()]});
        let frames =
            serde_json::to_vec(&serde_json::json!({"frames":[frame.clone(),frame.clone(),frame]}))
                .unwrap();

        // A stream declaration keeps the historical binding, frames or not.
        for supplied in [None, Some(frames.as_slice())] {
            let hdr = static_hdr_authority(&declared, 0, probe, supplied).unwrap();
            assert_eq!(
                (hdr.primaries_rgbw, hdr.max_1e4_nits, hdr.min_1e4_nits),
                (P3, 10_000_000, 1)
            );
            assert_eq!(hdr.source_observation_sha256, probe);
            assert_eq!(hdr.max_cll_fall, None);
        }

        // Frame-only MDCV binds the domain, the probe and the validated
        // values in their canonical encoding, not FFprobe's bytes.
        let hdr = static_hdr_authority(&silent, 0, probe, Some(&frames)).unwrap();
        assert_eq!(
            (hdr.primaries_rgbw, hdr.max_1e4_nits, hdr.min_1e4_nits),
            (P3, 10_000_000, 1)
        );
        let mut values = Vec::new();
        for primary in P3 {
            values.extend_from_slice(&primary.to_be_bytes());
        }
        values.extend_from_slice(&10_000_000_u32.to_be_bytes());
        values.extend_from_slice(&1_u32.to_be_bytes());
        let mut expected = Sha256::new();
        for part in [b"plurx-m7-st2086-frames-v2".as_slice(), &probe, &values] {
            expected.update((part.len() as u64).to_be_bytes());
            expected.update(part);
        }
        let expected: [u8; 32] = expected.finalize().into();
        assert_eq!(hdr.source_observation_sha256, expected);
        assert_ne!(hdr.source_observation_sha256, probe);
        // An FFprobe that prints the same frames differently (whitespace,
        // key order, an extra field) observes the same values.
        let mut spaced = serde_json::to_vec_pretty(&serde_json::json!({"frames":[
            {"side_data_list":[eclipse_mdcv()],"stream_index":0,"pkt_pos":"1"}]}))
        .unwrap();
        spaced.push(b'\n');
        assert_eq!(
            static_hdr_authority(&silent, 0, probe, Some(&spaced))
                .unwrap()
                .source_observation_sha256,
            expected,
            "the same values are the same binding across FFprobe output changes"
        );
        assert_ne!(
            static_hdr_authority(&silent, 0, [8; 32], Some(&frames))
                .unwrap()
                .source_observation_sha256,
            expected
        );

        // Absence, conflicts, foreign streams and unreadable output.
        let none = serde_json::to_vec(&serde_json::json!({"frames":[{"stream_index":0}]})).unwrap();
        for supplied in [None, Some(none.as_slice())] {
            assert!(matches!(
                static_hdr_authority(&silent, 0, probe, supplied),
                Err(EvaluationError::Unsupported(message))
                    if message == "M7 selected source has no exact ST 2086 declarations"
            ));
        }
        let mut other = eclipse_mdcv();
        other["max_luminance"] = "40000000/10000".into();
        let conflict = serde_json::to_vec(&serde_json::json!({"frames":[
            {"stream_index":0,"side_data_list":[eclipse_mdcv()]},
            {"stream_index":0,"side_data_list":[other]}]}))
        .unwrap();
        assert!(matches!(
            static_hdr_authority(&silent, 0, probe, Some(&conflict)),
            Err(EvaluationError::Unsupported(message)) if message.contains("conflicting")
        ));
        assert!(matches!(
            static_hdr_authority(&silent, 1, probe, Some(&frames)),
            Err(EvaluationError::Unsupported(message)) if message.contains("selected stream differs")
        ));
        assert!(matches!(
            static_hdr_authority(&silent, 0, probe, Some(b"not json")),
            Err(EvaluationError::Unsupported(message)) if message.contains("JSON")
        ));
    }

    #[test]
    fn frame_projection_refusals_are_unsupported_and_interruptions_defer() {
        use std::os::unix::process::ExitStatusExt;
        let exit = |code: i32| std::process::ExitStatus::from_raw(code << 8);
        let signal = |number: i32| std::process::ExitStatus::from_raw(number);
        assert_eq!(frame_projection_verdict(exit(0), b"", 10, 10), Ok(()));
        for (status, len) in [(exit(1), 0), (exit(0), 11)] {
            let error = frame_projection_verdict(status, b"Invalid data", len, 10).unwrap_err();
            assert!(matches!(error, FrameProjectionError::Deterministic(_)));
            assert!(matches!(
                error.into_evaluation(),
                EvaluationError::Unsupported(message) if message.contains("frame MDCV probe")
            ));
        }
        let killed = frame_projection_verdict(signal(9), b"", 0, 10).unwrap_err();
        assert!(matches!(killed, FrameProjectionError::Transient(_)));
        assert!(matches!(
            FrameProjectionError::Transient("deadline".into()).into_evaluation(),
            EvaluationError::Deferred(_)
        ));
    }

    #[test]
    fn the_m7_identity_is_a_deterministic_digest_of_source_and_tools() {
        let source = "1".repeat(64);
        let tools = "2".repeat(64);
        let identity = identity_of(&source, &tools);
        assert_eq!(identity.len(), 64);
        assert!(identity
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert_eq!(identity_of(&source, &tools), identity);
        assert_ne!(identity_of(&"3".repeat(64), &tools), identity);
        assert_ne!(identity_of(&source, &"3".repeat(64)), identity);
        // Length prefixes: moving a byte between the parts is another identity.
        assert_ne!(identity_of("ab", "c"), identity_of("a", "bc"));
        let built = env!("PLURX_M7_SOURCE_SHA256");
        assert_eq!(built.len(), 64);
        assert!(built.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn evaluation_endings_record_their_outcome_and_retries_record_nothing() {
        use plurx_core::store::dv_m7_outcomes::{M7OutcomeKind, MAX_GATE_BYTES};
        let identity = "a".repeat(64);
        let record = |evaluation: Evaluation| outcome_record(&evaluation, &identity);
        assert_eq!(record(Ok(Evaluated::Skipped)), None);
        assert_eq!(record(Err(EvaluationError::Deferred("busy".into()))), None);
        assert_eq!(
            record(Err(
                dv_title::CensusScanError::Transient("killed".into()).into()
            )),
            None
        );
        assert!(matches!(
            EvaluationError::from(dv_title::CensusScanError::Refused("refused".into())),
            EvaluationError::Unsupported(message) if message == "refused"
        ));
        for (evaluation, kind, gate) in [
            (
                Ok(Evaluated::NotApplicable),
                M7OutcomeKind::NotApplicable,
                None,
            ),
            (Ok(Evaluated::Result), M7OutcomeKind::Result, None),
            (
                Err(EvaluationError::Unsupported("no fitted scenes".into())),
                M7OutcomeKind::Unsupported,
                Some("no fitted scenes"),
            ),
            (
                Err(EvaluationError::Inconclusive("rate".into())),
                M7OutcomeKind::Inconclusive,
                Some("rate"),
            ),
            (
                Err(EvaluationError::Unsupported(String::new())),
                M7OutcomeKind::Unsupported,
                Some("unspecified"),
            ),
        ] {
            let outcome = record(evaluation).unwrap();
            assert_eq!(outcome.identity, identity);
            assert_eq!(
                outcome.fit_version,
                plurx_core::transcode::dv_refit::FIT_VERSION
            );
            assert_eq!(outcome.outcome, kind);
            assert_eq!(outcome.gate.as_deref(), gate);
        }
        let long = record(Err(EvaluationError::Unsupported("x".repeat(4096))))
            .unwrap()
            .gate
            .unwrap();
        assert_eq!(long.len(), MAX_GATE_BYTES);
    }

    fn exposure(model_means: [f64; 4]) -> CalibrationEvidence {
        CalibrationEvidence {
            window: CalibrationWindow {
                first_ordinal: 0,
                last_ordinal: 47,
                first_pts: 0,
                end_pts: 2_002,
                time_base: Rational::new(1, 1000).expect("source clock"),
                receipt_sha256: [1; 32],
            },
            samples: 1,
            model_means,
            whole_readback: [2; 32],
            seek_readback: [3; 32],
        }
    }

    #[test]
    fn automatic_rate_ceiling_floors_the_exact_selected_packet_rate() {
        let time_base = Rational::new(1, 1_000).unwrap();
        let plan = measured_source_rate_plan(22_502_252, 2_000, time_base).unwrap();
        assert_eq!(plan.authored_band_kbps, [89_109, 90_009]);
        assert_eq!(plan.initial_target_kbps, 90_009);
        assert!(measured_source_rate_plan(1, 0, time_base).is_err());
        assert!(measured_source_rate_plan(0, 2_000, time_base).is_err());
    }

    #[test]
    fn automatic_margin_uses_only_complete_baseline_model_spreads() {
        assert_eq!(
            frozen_margin(&[exposure([1.0, 1.01, 1.02, 1.03]), exposure([1.0; 4])])
                .expect("both exposures")
                .value(),
            0.05,
        );
        assert_eq!(
            frozen_margin(&[exposure([1.0, 1.01, 1.02, 1.056]), exposure([1.0; 4])])
                .expect("conservative spread")
                .value(),
            0.06,
        );
        assert!(frozen_margin(&[exposure([1.0; 4])]).is_err());
        assert!(frozen_margin(&[exposure([f64::NAN; 4]), exposure([1.0; 4])]).is_err());
    }
}
