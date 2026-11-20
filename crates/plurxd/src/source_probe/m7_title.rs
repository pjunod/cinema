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
        whole_source_census: None,
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

pub(super) async fn evaluate(
    context: &TitleContext<'_>,
    stats: &plurx_core::scan::dv_title::DvTitleStats,
) -> Result<(), String> {
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
    if let Some(result) = historical(root.clone(), title.content_sha256).await? {
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
        return Ok(());
    }
    if let Some(alias) = repetition(root.clone(), title.content_sha256).await? {
        tracing::info!(
            file_id=context.file.id,
            fixture_sha256=%alias.fixture_sha256, source_sha256=%alias.source_sha256,
            protocol_sha256=%alias.protocol_sha256, repeat_count=alias.repeat_count,
            "physical repeat fixture shares the sealed M7 source experiment"
        );
        return Ok(());
    }
    use plurx_core::scan::dv_title::{TitleConstant, TitleElKind};
    if stats.el_kind != TitleConstant::Consistent(TitleElKind::Fel) {
        return Ok(());
    }
    measure_new_title(context, title, &root).await
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

use super::{
    dv_title::{self, M7WindowRange},
    m7_candidate::{prepare_authored, FrozenRecipe, RatePlan, StaticHdr},
    m7_comparison::{
        self, ComparisonInput, EvaluationWindow, FrozenProtocol, Measurement, PublicationReadback,
    },
};
use plurx_core::transcode::{
    dv_m7_receipt::{CalibrationWindow, M7Receipt, MODEL_VERSION},
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
) -> Result<(), String> {
    // A prior native terminal report survives changes in the current helper
    // and the catalogue's bounded cache. A routine rescan cannot reopen it.
    if let Some(result) = native_terminal(root, title.content_sha256).await? {
        tracing::info!(file_id=context.file.id, disposition=?result.disposition,
            protocol_sha256=%result.protocol_sha256, report_sha256=%result.report_sha256,
            binding_key=%result.binding_key, "retained native M7 experiment");
        return Ok(());
    }
    let scratch = tempfile::Builder::new()
        .prefix("m7-title-")
        .tempdir_in(&context.state.runtime_cache_dir)
        .map_err(string)?;
    let expected_version = title.source.object_version().to_owned();
    let prepared = super::run_source_admitted_probe(
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
            let mut source = source_plan(context, &title, &fitted)?;
            let reserve_bytes = m7_comparison::title_scratch_bytes(source.layout).map_err(string)?;
            let reservation = crate::playback_cache::reserve(scratch.path(), reserve_bytes)
                .await.map_err(string)?;
            let (_, census) = dv_title::scan_with_m7_census(
                context.file, &context.demand.probe_json, context.tools,
                &context.state.runtime_cache_dir, &source.windows,
            ).await?;
            title.whole_source_census = Some(census);
            let verified = title.whole_source_census.as_ref().ok_or("M7 title census disappeared")?;
            let windows = source.windows.iter().map(|range| {
                verified.window(
                    &title.source, context.tools, source.layout, range.first_pts, range.end_pts,
                )
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
            let recipe_sha = source.recipe.recipe_sha256();
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
                "recipe_sha256": hex::encode(recipe_sha),
                "recipe": &source.recipe,
            })).map_err(string)?;
            let protocol_sha: [u8; 32] = Sha256::digest(&freeze).into();
            let directory = root.join("experiments")
                .join(hex::encode(title.content_sha256)).join(hex::encode(protocol_sha));
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
                        return Err("M7 original evaluation RPU active area disagrees with fitted readback".into());
                    }
                }
                let authored = prepare_authored(
                    context, &title, &frozen, &source.recipe, planes, source.dv_level,
                    &scratch, context.cancel,
                ).await.map_err(string)?;
                prepared_windows.push(EvaluationWindow {
                    layout: source.layout, source_frames: frames, authored, whole, seek,
                });
            }
            Ok::<_, String>((directory, ComparisonInput {
                title, frozen, windows: prepared_windows, scratch_bytes: reserve_bytes,
            }, reservation))
        },
    ).await?;
    let (directory, input, _reservation) = prepared;
    let result = m7_comparison::execute(context, input)
        .await
        .map_err(string)?;
    match result {
        m7_comparison::ComparisonRun::Reused(receipt) => {
            tracing::info!(file_id=context.file.id, disposition=?receipt.disposition(),
                binding_key=%receipt.binding_key(), "reused exact M7 measurement");
        }
        m7_comparison::ComparisonRun::Measured(measurement) => {
            if context.cancel.is_cancelled() {
                return Err("M7 was cancelled before receipt publication".into());
            }
            let receipt = persist_measurement(directory, measurement).await?;
            let probe =
                M7SourceProbe::from_probe_json(&context.demand.probe_json).map_err(string)?;
            let recorded = context
                .fence
                .record_dv_m7_receipt(probe, receipt)
                .await
                .map_err(string)?;
            if !recorded {
                return Err("M7 receipt publication lost its title authority".into());
            }
        }
    }
    Ok(())
}

struct SourcePlan {
    layout: CaptureLayout,
    cadence: Rational,
    dv_level: u8,
    windows: Vec<M7WindowRange>,
    recipe: FrozenRecipe,
}

fn source_plan(
    context: &TitleContext<'_>,
    title: &HeldTitle,
    fitted: &FittedPublication,
) -> Result<SourcePlan, String> {
    let document: serde_json::Value =
        serde_json::from_str(&context.demand.probe_json).map_err(string)?;
    let video_index = crate::decode_facts::first_playable_video_index(&document)
        .ok_or("M7 source has no selected video stream")?;
    let stream = document["streams"]
        .as_array()
        .and_then(|streams| {
            streams
                .iter()
                .find(|stream| stream["index"].as_u64() == Some(u64::from(video_index)))
        })
        .ok_or("M7 selected stream is absent")?;
    let cadence = stream["avg_frame_rate"]
        .as_str()
        .and_then(|rate| {
            let (num, den) = rate.split_once('/')?;
            Rational::new(num.parse().ok()?, den.parse().ok()?)
        })
        .ok_or("M7 requires an observed average frame cadence")?;
    let raster =
        ["width", "height"].map(|axis| stream[axis].as_u64().and_then(|n| u16::try_from(n).ok()));
    let [Some(width), Some(height)] = raster else {
        return Err("M7 source raster is unspecified".into());
    };
    let clock = fitted.clock()?;
    let time_base = Rational::new(clock.source_time_base.0, clock.source_time_base.1)
        .ok_or("invalid M7 fitted source time base")?;
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
        return Err("M7 coded raster is outside the managed full-plane recipe".into());
    }
    let (primaries_rgbw, max_1e4_nits, min_1e4_nits) =
        crate::decode_facts::mastering::source_st2086(stream)?
            .ok_or("M7 selected source has no exact ST 2086 declarations")?;
    let held_rate = plurx_core::transcode::enhancement_stream_rate(&document, video_index)
        .ok_or("M7 selected source has no independently bound video rate")?;
    let high = u32::try_from(held_rate / 1000).map_err(string)?;
    // New-title rate trials may match but never exceed the measured source
    // video budget. The one-percent interval is an explicit rate-control
    // policy of this protocol and carries no delivery claim.
    let low = high.saturating_sub(high / (100 / SOURCE_RATE_BAND_PERCENT));
    let recipe = FrozenRecipe::new(
        cadence,
        StaticHdr {
            primaries_rgbw,
            max_1e4_nits,
            min_1e4_nits,
            // FEL reconstruction changes the samples to which the input CLL
            // applied. Preserve its mastering declaration without CLL claims.
            max_cll_fall: None,
            source_observation_sha256: title.probe_sha256,
        },
        RatePlan {
            authored_band_kbps: [low, high],
            initial_target_kbps: high,
            maximum_attempts: 4,
        },
    )
    .map_err(string)?;
    let dv_level = context
        .file
        .dolby_vision
        .level
        .ok_or("M7 source Dolby Vision level is absent")?;
    let dv_level = u8::try_from(dv_level).map_err(string)?;
    let numerator =
        FRAMES as u128 * u128::from(cadence.denominator()) * u128::from(time_base.denominator());
    let denominator = u128::from(cadence.numerator()) * u128::from(time_base.numerator());
    if numerator % denominator != 0 {
        return Err("M7 exposure is not integral on the fitted source clock".into());
    }
    let ticks = i64::try_from(numerator / denominator).map_err(string)?;
    let index = fitted.index();
    let proofs = index
        .dv_refit_proofs
        .as_ref()
        .ok_or("canonical M7 fitting proof absent")?;
    let mut windows: Vec<M7WindowRange> = Vec::new();
    // Start after the opening two entries so the independent control
    // generation must really seek. Use the first complete disjoint covered
    // RAP exposures in plan order. Packet/census validation decides coverage.
    for entry in fitted.plan().entries.iter().skip(2) {
        let row =
            plurx_core::segplan::entry_row(index, entry).ok_or("M7 plan is outside its index")?;
        let start = clock
            .source_pts(i128::from(entry.start_ticks) + i128::from(proofs[row].first_cto()))
            .map_err(string)?;
        let end = start
            .checked_add(ticks)
            .ok_or("M7 exposure clock overflow")?;
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
        return Err(
            "M7 fitted plan lacks two calibration exposures and one evaluation exposure".into(),
        );
    }
    Ok(SourcePlan {
        layout,
        cadence,
        dv_level,
        windows,
        recipe,
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

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeTerminal {
    source_sha256: String,
    protocol_sha256: String,
    binding_key: String,
    binding_sha256: String,
    report_sha256: String,
    disposition: plurx_core::transcode::dv_m7_scoring::aggregation::Disposition,
}

async fn native_terminal(root: &Path, source: [u8; 32]) -> Result<Option<NativeTerminal>, String> {
    let source = hex::encode(source);
    let root = root.join("experiments").join(&source);
    tokio::task::spawn_blocking(move || {
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(string(error)),
        };
        for entry in entries {
            let entry = entry.map_err(string)?;
            let path = entry.path();
            if !entry.file_type().map_err(string)?.is_dir() || !path.join("terminal.json").is_file()
            {
                continue;
            }
            let result: NativeTerminal =
                serde_json::from_slice(&load_json(&path.join("terminal.json"), None)?)
                    .map_err(string)?;
            if result.source_sha256 != source
                || entry.file_name().to_str() != Some(result.protocol_sha256.as_str())
                || !digest_text(&result.protocol_sha256)
                || !digest_text(&result.binding_key)
                || !digest_text(&result.binding_sha256)
                || !digest_text(&result.report_sha256)
            {
                return Err("invalid native M7 terminal result".into());
            }
            load_json(
                &path.join("native-protocol.json"),
                Some(&result.protocol_sha256),
            )?;
            load_json(
                &path.join("comparison-result.json"),
                Some(&result.report_sha256),
            )?;
            load_json(
                &path.join("measurement-binding.json"),
                Some(&result.binding_sha256),
            )?;
            return Ok(Some(result));
        }
        Ok(None)
    })
    .await
    .map_err(string)?
}

async fn persist_measurement(
    directory: PathBuf,
    measurement: Measurement,
) -> Result<M7Receipt, String> {
    tokio::task::spawn_blocking(move || {
        let mut full_report = NamedTempFile::new_in(&directory).map_err(string)?;
        let receipt = M7Receipt::from_report(
            measurement.custody,
            measurement.measured_at_ms,
            &measurement.report,
            full_report.as_file_mut(),
        )
        .map_err(string)?;
        full_report.as_file().sync_all().map_err(string)?;
        let report_sha256 = hex::encode(receipt.report_sha256());
        let path = directory.join("comparison-result.json");
        if let Err(error) = full_report.persist_noclobber(&path) {
            if error.error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(string(error.error));
            }
            load_json(&path, Some(&report_sha256))?;
        }
        let binding = serde_json::to_vec(receipt.binding()).map_err(string)?;
        let binding_sha256 = hex::encode(Sha256::digest(&binding));
        immutable_write(&directory, "measurement-binding.json", &binding)?;
        let terminal = NativeTerminal {
            source_sha256: hex::encode(receipt.binding().source_sha256),
            protocol_sha256: hex::encode(receipt.binding().protocol_sha256),
            binding_key: receipt.binding_key().to_owned(),
            binding_sha256,
            report_sha256,
            disposition: receipt.disposition(),
        };
        immutable_write(
            &directory,
            "terminal.json",
            &serde_json::to_vec(&terminal).map_err(string)?,
        )?;
        Ok(receipt)
    })
    .await
    .map_err(string)?
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
mod tests {
    use super::*;

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
