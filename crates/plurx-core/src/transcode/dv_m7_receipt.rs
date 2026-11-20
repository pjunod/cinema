//! Bounded M7 measurements tied to the exact compared experiments.
//! The full score report is written to the caller's owned receipt storage.

use super::{
    dv_m7_scoring::{
        aggregation::{
            comparison_disposition, mean_gain, window_checks, window_checks_from_shadow,
            ComparisonReport, Disposition, Distribution, FrozenMargin, MeanGain, ModelStatistics,
            WindowChecks, AGGREGATION_REVISION, MAX_EVALUATION_WINDOWS, MAX_FRAMES_PER_WINDOW,
            MAX_SITES_PER_FRAME,
        },
        Model, SCORING_REVISION,
    },
    dv_refit::FIT_VERSION,
    Rational,
};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest as _, Sha256};
use std::io::{self, Write};

pub const PROBE_KEY: &str = "plurx_dv_m7_v1";
pub const MODEL_VERSION: &str = "plurx-m7-four-model-v1";
pub const MAX_RECEIPT_BYTES: usize = 128 * 1024;
pub const MAX_RECENT_RECEIPTS: usize = 8;
pub(crate) const SCHEMA: u32 = 1;
pub(crate) const MEASUREMENT_FIELDS: [&str; 6] = [
    crate::store::CONTENT_ENCODING_PROBE_KEY,
    "plurx_dv_title_v1",
    "plurx_dv_fel_v1",
    "plurx_dv_fel_requests_v3",
    "plurx_dv_fel_variants_v3",
    PROBE_KEY,
];

const MODELS: usize = Model::ALL.len();
const MAX_CATALOGUE_PROBE_BYTES: usize = 2 * 1024 * 1024;
const MAX_SOURCE_PROBE_BYTES: usize = 64 * 1024;
const MAX_SCORE: f64 = 65_535.0;
type Digest = [u8; 32];

#[derive(Debug, thiserror::Error)]
pub enum M7ReceiptError {
    #[error("invalid or incomplete M7 comparison binding")]
    Binding,
    #[error("invalid M7 comparison statistics or checks")]
    Statistics,
    #[error("invalid M7 receipt identity")]
    Identity,
    #[error("M7 receipt or source probe exceeds its size limit")]
    Size,
    #[error("M7 requires a source probe object")]
    SourceProbe,
    #[error("M7 receipt JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("writing the full M7 comparison report: {0}")]
    Report(#[from] io::Error),
}

/// The ordinary probe facts. Its measurement siblings can change during M7
/// without changing the input to the experiment.
#[derive(Debug, Clone)]
pub struct M7SourceProbe {
    value: serde_json::Value,
    sha256: Digest,
}

impl M7SourceProbe {
    pub fn from_probe_json(document: &str) -> Result<Self, M7ReceiptError> {
        if document.len() > MAX_CATALOGUE_PROBE_BYTES {
            return Err(M7ReceiptError::Size);
        }
        let mut value: serde_json::Value = serde_json::from_str(document)?;
        let object = value.as_object_mut().ok_or(M7ReceiptError::SourceProbe)?;
        for field in MEASUREMENT_FIELDS {
            object.remove(field);
        }
        value.sort_all_objects();
        let encoded = serde_json::to_vec(&value)?;
        if encoded.len() > MAX_SOURCE_PROBE_BYTES {
            return Err(M7ReceiptError::Size);
        }
        let sha256 = hash(b"plurx/m7/source-probe/v1\0", &encoded);
        Ok(Self { value, sha256 })
    }

    pub fn sha256(&self) -> Digest {
        self.sha256
    }

    pub(crate) fn value(&self) -> &serde_json::Value {
        &self.value
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationWindow {
    pub first_ordinal: u64,
    pub last_ordinal: u64,
    pub first_pts: i64,
    pub end_pts: i64,
    #[serde(deserialize_with = "read_rational")]
    pub time_base: Rational,
    /// Binds the calibration protocol and the census that froze the margin.
    pub receipt_sha256: Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowBinding {
    pub first_ordinal: u64,
    pub last_ordinal: u64,
    pub first_pts: i64,
    pub end_pts: i64,
    #[serde(deserialize_with = "read_rational")]
    pub time_base: Rational,
    pub frames: usize,
    pub sites_per_frame: usize,
    pub source_census_sha256: Digest,
    pub candidate_census_sha256: Digest,
    pub authored_crop_sha256: Digest,
    pub encoder_sha256: Digest,
    pub encoded_sha256: Digest,
    pub authored_sha256: Digest,
    pub authoring_receipt_sha256: Digest,
    pub source_vcl_sha256: Digest,
    pub whole_publication_sha256: Digest,
    pub whole_readback_sha256: Digest,
    pub whole_rpu_sha256: Digest,
    pub seek_publication_sha256: Digest,
    pub seek_readback_sha256: Digest,
    pub seek_rpu_sha256: Digest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct M7Binding {
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub probe_revision: i64,
    pub source_sha256: Digest,
    pub held_source_version: String,
    pub probe_sha256: Digest,
    pub scanner_digest: String,
    pub renderer_digest: String,
    pub scorer_sha256: Digest,
    pub model_version: String,
    pub scoring_revision: u32,
    pub aggregation_revision: u32,
    pub protocol_sha256: Digest,
    pub recipe_sha256: Digest,
    pub fit_method: String,
    pub fit_table_sha256: Digest,
    pub calibration: Vec<CalibrationWindow>,
    pub margin: f64,
    pub windows: Vec<WindowBinding>,
}

impl M7Binding {
    pub fn key(&self) -> Result<String, M7ReceiptError> {
        self.validate()?;
        Ok(hex::encode(hash(
            b"plurx/m7/experiment/v1\0",
            &serde_json::to_vec(self)?,
        )))
    }

    fn validate(&self) -> Result<(), M7ReceiptError> {
        if self.file_id <= 0
            || self.source_size <= 0
            || self.source_mtime < 0
            || self.probe_revision <= 0
            || self.held_source_version.is_empty()
            || self.held_source_version.len() > 256
            || !hex_digest(&self.scanner_digest)
            || !hex_digest(&self.renderer_digest)
            || self.model_version != MODEL_VERSION
            || self.fit_method != FIT_VERSION
            || self.scoring_revision != SCORING_REVISION
            || self.aggregation_revision != AGGREGATION_REVISION
            || FrozenMargin::new(self.margin).is_err()
            || !all_digests([
                self.source_sha256,
                self.probe_sha256,
                self.scorer_sha256,
                self.protocol_sha256,
                self.recipe_sha256,
                self.fit_table_sha256,
            ])
            || !(1..=MAX_EVALUATION_WINDOWS).contains(&self.calibration.len())
            || !(1..=MAX_EVALUATION_WINDOWS).contains(&self.windows.len())
        {
            return Err(M7ReceiptError::Binding);
        }
        let clock = self.windows[0].time_base;
        let mut spans = Vec::with_capacity(self.calibration.len() + self.windows.len());
        for window in &self.calibration {
            if window.time_base != clock || !all_digests([window.receipt_sha256]) {
                return Err(M7ReceiptError::Binding);
            }
            spans.push((
                window.first_ordinal,
                window.last_ordinal,
                window.first_pts,
                window.end_pts,
            ));
        }
        for window in &self.windows {
            if window.time_base != clock
                || !(1..=MAX_FRAMES_PER_WINDOW).contains(&window.frames)
                || !(1..=MAX_SITES_PER_FRAME).contains(&window.sites_per_frame)
                || window.last_ordinal.checked_sub(window.first_ordinal)
                    != Some((window.frames - 1) as u64)
                || !all_digests([
                    window.source_census_sha256,
                    window.candidate_census_sha256,
                    window.authored_crop_sha256,
                    window.encoder_sha256,
                    window.encoded_sha256,
                    window.authored_sha256,
                    window.authoring_receipt_sha256,
                    window.source_vcl_sha256,
                    window.whole_publication_sha256,
                    window.whole_readback_sha256,
                    window.whole_rpu_sha256,
                    window.seek_publication_sha256,
                    window.seek_readback_sha256,
                    window.seek_rpu_sha256,
                ])
            {
                return Err(M7ReceiptError::Binding);
            }
            spans.push((
                window.first_ordinal,
                window.last_ordinal,
                window.first_pts,
                window.end_pts,
            ));
        }
        spans.sort_unstable();
        if spans
            .iter()
            .any(|(first, last, pts, end)| first > last || pts >= end)
            || spans
                .windows(2)
                .any(|pair| pair[0].1 >= pair[1].0 || pair[0].3 > pair[1].2)
        {
            return Err(M7ReceiptError::Binding);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowSummary {
    statistics: [ModelStatistics; MODELS],
    checks: [WindowChecks; MODELS],
    frames_with_shadow: [usize; MODELS],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Summary {
    windows: Vec<WindowSummary>,
    pooled_statistics: [ModelStatistics; MODELS],
    pooled_mean_gain: [MeanGain; MODELS],
    disposition: Disposition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    binding: M7Binding,
    measured_at_ms: i64,
    report_sha256: Digest,
    summary: Summary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredReceipt {
    schema: u32,
    binding_key: String,
    receipt_sha256: String,
    payload: Payload,
}

/// An M7 receipt can be created from the scored report or restored by the
/// store after exact-binding and integrity checks.
#[derive(Debug, Clone)]
pub struct M7Receipt(StoredReceipt);

impl M7Receipt {
    pub fn from_report(
        binding: M7Binding,
        measured_at_ms: i64,
        report: &ComparisonReport,
        report_writer: impl Write,
    ) -> Result<Self, M7ReceiptError> {
        let binding_key = binding.key()?;
        if measured_at_ms <= 0
            || binding.margin.to_bits() != report.margin.value().to_bits()
            || binding.windows.len() != report.windows.len()
        {
            return Err(M7ReceiptError::Statistics);
        }
        let mut windows = Vec::with_capacity(report.windows.len());
        for (plan, window) in binding.windows.iter().zip(&report.windows) {
            if window.frames.len() != plan.frames
                || window
                    .frames
                    .iter()
                    .flatten()
                    .any(|model| !valid_model(model, plan.sites_per_frame))
                || (0..MODELS).any(|model| {
                    window.checks[model]
                        != window_checks(
                            &window.statistics[model],
                            &window.frames,
                            model,
                            report.margin,
                        )
                })
            {
                return Err(M7ReceiptError::Statistics);
            }
            windows.push(WindowSummary {
                statistics: window.statistics,
                checks: window.checks,
                frames_with_shadow: std::array::from_fn(|model| {
                    window
                        .frames
                        .iter()
                        .filter(|frame| {
                            frame[model].served_fplus.bands[0].is_some()
                                && frame[model].candidate.bands[0].is_some()
                        })
                        .count()
                }),
            });
        }
        let summary = Summary {
            windows,
            pooled_statistics: report.pooled_statistics,
            pooled_mean_gain: report.pooled_mean_gain,
            disposition: report.disposition,
        };
        summary.validate(&binding)?;
        let mut writer = DigestWriter {
            sink: report_writer,
            digest: Sha256::new(),
        };
        serde_json::to_writer(&mut writer, report)?;
        writer.flush()?;
        let payload = Payload {
            binding,
            measured_at_ms,
            report_sha256: writer.digest.finalize().into(),
            summary,
        };
        let receipt_sha256 = payload_digest(&payload)?;
        let receipt = Self(StoredReceipt {
            schema: SCHEMA,
            binding_key,
            receipt_sha256,
            payload,
        });
        receipt.encode()?;
        Ok(receipt)
    }

    pub fn binding(&self) -> &M7Binding {
        &self.0.payload.binding
    }

    pub(crate) fn measured_at_ms(&self) -> i64 {
        self.0.payload.measured_at_ms
    }

    pub fn binding_key(&self) -> &str {
        &self.0.binding_key
    }

    pub fn report_sha256(&self) -> Digest {
        self.0.payload.report_sha256
    }

    pub fn disposition(&self) -> Disposition {
        self.0.payload.summary.disposition
    }

    /// Compact validated receipt for an owned full-report publication journal.
    pub fn encode_recovery(&self) -> Result<String, M7ReceiptError> {
        self.encode()
    }

    /// Restore a pending publication only for the independently checked
    /// binding and the digest of the already persisted full report.
    pub fn decode_recovery(
        encoded: &str,
        binding: &M7Binding,
        full_report_sha256: Digest,
    ) -> Result<Self, M7ReceiptError> {
        let receipt = Self::decode(encoded, binding)?;
        if receipt.report_sha256() != full_report_sha256 {
            return Err(M7ReceiptError::Identity);
        }
        Ok(receipt)
    }

    pub(crate) fn encode(&self) -> Result<String, M7ReceiptError> {
        let encoded = serde_json::to_string(&self.0)?;
        if encoded.len() > MAX_RECEIPT_BYTES {
            return Err(M7ReceiptError::Size);
        }
        Ok(encoded)
    }

    pub(crate) fn decode(encoded: &str, binding: &M7Binding) -> Result<Self, M7ReceiptError> {
        if encoded.len() > MAX_RECEIPT_BYTES {
            return Err(M7ReceiptError::Size);
        }
        let receipt: StoredReceipt = serde_json::from_str(encoded)?;
        let payload = &receipt.payload;
        if receipt.schema != SCHEMA
            || receipt.binding_key != binding.key()?
            || &payload.binding != binding
            || payload.binding.key()? != receipt.binding_key
            || payload.measured_at_ms <= 0
            || !all_digests([payload.report_sha256])
            || payload_digest(payload)? != receipt.receipt_sha256
        {
            return Err(M7ReceiptError::Identity);
        }
        payload.summary.validate(binding)?;
        Ok(Self(receipt))
    }
}

impl Summary {
    fn validate(&self, binding: &M7Binding) -> Result<(), M7ReceiptError> {
        let margin = FrozenMargin::new(binding.margin).map_err(|_| M7ReceiptError::Binding)?;
        if self.windows.len() != binding.windows.len() {
            return Err(M7ReceiptError::Statistics);
        }
        let mut pooled_count = 0;
        for (window, plan) in self.windows.iter().zip(&binding.windows) {
            let sites = plan.frames * plan.sites_per_frame;
            pooled_count += sites;
            for model in 0..MODELS {
                let statistics = &window.statistics[model];
                let observed = window.checks[model];
                let frames_with_shadow = window.frames_with_shadow[model];
                let extrema = (
                    observed.largest_served_fplus_frame_shadow_p99,
                    observed.largest_candidate_frame_shadow_p99,
                );
                if !valid_model(statistics, sites)
                    || frames_with_shadow > plan.frames
                    || !match extrema {
                        (None, None) => {
                            frames_with_shadow == 0 && statistics.served_fplus.bands[0].is_none()
                        }
                        (Some(served), Some(candidate)) => {
                            frames_with_shadow > 0
                                && statistics.served_fplus.bands[0].is_some()
                                && (0.0..=MAX_SCORE).contains(&served)
                                && (0.0..=MAX_SCORE).contains(&candidate)
                        }
                        _ => false,
                    }
                    || observed
                        != window_checks_from_shadow(
                            statistics,
                            margin,
                            frames_with_shadow == plan.frames,
                            extrema,
                        )
                {
                    return Err(M7ReceiptError::Statistics);
                }
            }
        }
        for (model, stats) in self.pooled_statistics.iter().enumerate() {
            if !valid_model(stats, pooled_count)
                || self.pooled_mean_gain[model] != mean_gain(stats, margin)
            {
                return Err(M7ReceiptError::Statistics);
            }
        }
        let checks = self
            .windows
            .iter()
            .flat_map(|window| window.checks.into_iter().flat_map(WindowChecks::checks))
            .chain(self.pooled_mean_gain.map(|gain| gain.check));
        if self.disposition != comparison_disposition(checks) {
            return Err(M7ReceiptError::Statistics);
        }
        Ok(())
    }
}

fn valid_model(stats: &ModelStatistics, count: usize) -> bool {
    let served = &stats.served_fplus;
    let candidate = &stats.candidate;
    let paired = &stats.paired;
    if !valid_distribution(served.full, count, 0.0)
        || !valid_distribution(candidate.full, count, 0.0)
        || !valid_distribution(paired.advantage, count, -MAX_SCORE)
        || paired
            .candidate_closer_count
            .checked_add(paired.candidate_worse_count)
            .and_then(|value| value.checked_add(paired.ties))
            != Some(count)
    {
        return false;
    }
    let mut band_count = 0usize;
    for (served, candidate) in served.bands.into_iter().zip(candidate.bands) {
        match (served, candidate) {
            (None, None) => {}
            (Some(served), Some(candidate))
                if served.count <= count
                    && valid_distribution(served, served.count, 0.0)
                    && valid_distribution(candidate, served.count, 0.0) =>
            {
                band_count += served.count;
            }
            _ => return false,
        }
    }
    band_count == count
}

fn valid_distribution(stats: Distribution, count: usize, lower: f64) -> bool {
    let rounding = f64::EPSILON * MAX_SCORE * 4.0;
    count > 0
        && stats.count == count
        && (lower..=MAX_SCORE).contains(&stats.min)
        && (stats.min..=MAX_SCORE).contains(&stats.max)
        && (stats.min - rounding..=stats.max + rounding).contains(&stats.mean)
        && (stats.min..=stats.max).contains(&stats.p95)
        && (stats.p95..=stats.max).contains(&stats.p99)
}

fn read_rational<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Rational, D::Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Wire {
        numerator: u32,
        denominator: u32,
    }

    let wire = Wire::deserialize(deserializer)?;
    Rational::new(wire.numerator, wire.denominator)
        .filter(|value| {
            value.numerator() == wire.numerator && value.denominator() == wire.denominator
        })
        .ok_or_else(|| serde::de::Error::custom("invalid M7 clock"))
}

fn all_digests<const N: usize>(digests: [Digest; N]) -> bool {
    digests.into_iter().all(|digest| digest != [0; 32])
}

fn hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hash(domain: &[u8], bytes: &[u8]) -> Digest {
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(bytes);
    hash.finalize().into()
}

fn payload_digest(payload: &Payload) -> Result<String, M7ReceiptError> {
    Ok(hex::encode(hash(
        b"plurx/m7/receipt/v1\0",
        &serde_json::to_vec(payload)?,
    )))
}

struct DigestWriter<W> {
    sink: W,
    digest: Sha256,
}

impl<W: Write> Write for DigestWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.sink.write(buffer)?;
        self.digest.update(&buffer[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.sink.flush()
    }
}
