//! Shared content-analysis completion, failure, and retry contracts.
//!
//! The daemon owns process execution and the Stores own atomic transitions,
//! but both must agree on the meaning of a failure.  This module keeps that
//! policy pure so SQLite, Hiqlite, the local refusal cache, and operator
//! surfaces cannot silently assign different retry behavior to one code.

use serde::{Deserialize, Serialize};

pub const INDEX_RETRY_BASE_MS: i64 = 30 * 60 * 1_000;
pub const INDEX_RETRY_MAX_MS: i64 = 24 * 60 * 60 * 1_000;
pub const INDEX_RETRY_WINDOW_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
pub const INDEX_FAILURE_POLICY_REVISION: u32 = 1;
pub const MAX_INDEX_DIAGNOSTIC_BYTES: usize = 8 * 1_024;
pub const MAX_INDEX_STDERR_BYTES: usize = 4 * 1_024;
pub const MAX_INDEX_STDERR_LINES: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionProvenance {
    StreamTicks,
    StreamSeconds,
    MatroskaDurationTag,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoCompletionExpectation {
    pub stream_index: u32,
    pub duration_num: u64,
    pub duration_den: u64,
    pub provenance: CompletionProvenance,
    pub source_object_version: String,
}

impl VideoCompletionExpectation {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.duration_num == 0 {
            return Err("video duration is zero");
        }
        if self.duration_den == 0 {
            return Err("video duration denominator is zero");
        }
        if self.source_object_version.is_empty() || self.source_object_version.len() > 512 {
            return Err("source object version is invalid");
        }
        Ok(())
    }

    /// Whether sample coverage reaches the selected video's duration with the
    /// contract's exact two-second tolerance. Wide checked arithmetic turns
    /// impossible comparisons into unverified metadata rather than success.
    pub fn covers(&self, covered_ticks: u64, scale: u32) -> Result<bool, &'static str> {
        self.validate()?;
        if scale == 0 {
            return Err("index timescale is zero");
        }
        let scale = u128::from(scale);
        let covered = u128::from(covered_ticks);
        let left = covered
            .checked_add(scale.checked_mul(2).ok_or("coverage arithmetic overflow")?)
            .and_then(|value| value.checked_mul(u128::from(self.duration_den)))
            .ok_or("coverage arithmetic overflow")?;
        let right = u128::from(self.duration_num)
            .checked_mul(scale)
            .ok_or("expectation arithmetic overflow")?;
        Ok(left >= right)
    }

    pub fn duration_ms_floor(&self) -> Option<i64> {
        let value = u128::from(self.duration_num)
            .checked_mul(1_000)?
            .checked_div(u128::from(self.duration_den))?;
        i64::try_from(value).ok()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexFailureCode {
    IndexBudgetExceeded,
    IndexProbeTimeout,
    IndexSourceIo,
    IndexProcessFailed,
    IndexVideoShortfall,
    IndexCompletionUnverified,
    IndexOutputMalformed,
    IndexRetryWindowExpired,
    Unsupported,
}

impl IndexFailureCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IndexBudgetExceeded => "index_budget_exceeded",
            Self::IndexProbeTimeout => "index_probe_timeout",
            Self::IndexSourceIo => "index_source_io",
            Self::IndexProcessFailed => "index_process_failed",
            Self::IndexVideoShortfall => "index_video_shortfall",
            Self::IndexCompletionUnverified => "index_completion_unverified",
            Self::IndexOutputMalformed => "index_output_malformed",
            Self::IndexRetryWindowExpired => "index_retry_window_expired",
            Self::Unsupported => "unsupported",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "index_budget_exceeded" => Self::IndexBudgetExceeded,
            "index_probe_timeout" => Self::IndexProbeTimeout,
            "index_source_io" => Self::IndexSourceIo,
            "index_process_failed" => Self::IndexProcessFailed,
            "index_video_shortfall" => Self::IndexVideoShortfall,
            "index_completion_unverified" => Self::IndexCompletionUnverified,
            "index_output_malformed" => Self::IndexOutputMalformed,
            "index_retry_window_expired" => Self::IndexRetryWindowExpired,
            "unsupported" => Self::Unsupported,
            _ => return None,
        })
    }

    pub const fn automatically_retryable(self) -> bool {
        matches!(self, Self::IndexBudgetExceeded | Self::IndexProbeTimeout)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexDiagnostic {
    pub version: u8,
    pub code: String,
    pub retryable: bool,
    pub claim_fence: i64,
    pub attempt: i64,
    pub selected_stream: Option<u32>,
    pub expectation_provenance: Option<CompletionProvenance>,
    pub covered_ms: Option<i64>,
    pub expected_ms: Option<i64>,
    pub container_ms: Option<i64>,
    pub fragment_count: Option<u32>,
    pub output_bytes: Option<u64>,
    pub elapsed_ms: Option<u64>,
    pub budget_ms: Option<u64>,
    pub exit_category: Option<String>,
    pub stderr_tail: Vec<String>,
    pub recorded_at_ms: i64,
}

impl IndexDiagnostic {
    pub fn encode_bounded(&self) -> Result<String, String> {
        if self.version != 1 {
            return Err("unsupported index diagnostic version".to_owned());
        }
        if IndexFailureCode::parse(&self.code).is_none() {
            return Err("unknown index diagnostic code".to_owned());
        }
        if self.claim_fence < 0 || self.attempt < 0 || self.recorded_at_ms < 0 {
            return Err("negative index diagnostic field".to_owned());
        }
        if self.stderr_tail.len() > MAX_INDEX_STDERR_LINES {
            return Err("index diagnostic has too many stderr lines".to_owned());
        }
        let stderr_bytes = self.stderr_tail.iter().map(String::len).sum::<usize>();
        if stderr_bytes > MAX_INDEX_STDERR_BYTES
            || self
                .stderr_tail
                .iter()
                .any(|line| line.len() > MAX_INDEX_STDERR_BYTES)
        {
            return Err("index diagnostic stderr exceeds its bound".to_owned());
        }
        if self
            .exit_category
            .as_ref()
            .is_some_and(|value| value.len() > 128)
        {
            return Err("index diagnostic exit category is too long".to_owned());
        }
        let mut bounded = self.clone();
        loop {
            let encoded = serde_json::to_string(&bounded)
                .map_err(|error| format!("encoding index diagnostic: {error}"))?;
            if encoded.len() <= MAX_INDEX_DIAGNOSTIC_BYTES {
                return Ok(encoded);
            }
            let Some(first) = bounded.stderr_tail.first_mut() else {
                return Err("index diagnostic exceeds its serialized bound".to_owned());
            };
            if first.is_empty() {
                bounded.stderr_tail.remove(0);
                continue;
            }
            let remove = first
                .char_indices()
                .nth(first.chars().count().min(256))
                .map(|(index, _)| index)
                .unwrap_or(first.len());
            first.drain(..remove);
        }
    }

    pub fn decode_bounded(value: &str) -> Option<Self> {
        if value.is_empty() || value.len() > MAX_INDEX_DIAGNOSTIC_BYTES {
            return None;
        }
        let decoded: Self = serde_json::from_str(value).ok()?;
        decoded.encode_bounded().ok()?;
        Some(decoded)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexRetryDecision {
    pub retryable: bool,
    pub next_attempt_at_ms: i64,
    pub retry_deadline_ms: i64,
    pub terminal_code: Option<IndexFailureCode>,
}

pub fn index_retry_decision(
    code: IndexFailureCode,
    transient_allowlisted: bool,
    charged_attempt: u32,
    max_attempts: u32,
    now_ms: i64,
    existing_deadline_ms: i64,
) -> IndexRetryDecision {
    let retryable = code.automatically_retryable()
        || matches!(
            code,
            IndexFailureCode::IndexSourceIo | IndexFailureCode::IndexProcessFailed
        ) && transient_allowlisted;
    if !retryable {
        return IndexRetryDecision {
            retryable: false,
            next_attempt_at_ms: i64::MAX,
            retry_deadline_ms: existing_deadline_ms,
            terminal_code: None,
        };
    }
    let deadline = if existing_deadline_ms > 0 {
        existing_deadline_ms
    } else {
        now_ms.saturating_add(INDEX_RETRY_WINDOW_MS)
    };
    if charged_attempt >= max_attempts {
        return IndexRetryDecision {
            retryable: false,
            next_attempt_at_ms: i64::MAX,
            retry_deadline_ms: deadline,
            terminal_code: None,
        };
    }
    let shift = charged_attempt.saturating_sub(1).min(30);
    let delay = INDEX_RETRY_BASE_MS
        .saturating_mul(1_i64.checked_shl(shift).unwrap_or(i64::MAX))
        .min(INDEX_RETRY_MAX_MS);
    let next = now_ms.saturating_add(delay);
    if next >= deadline {
        return IndexRetryDecision {
            retryable: false,
            next_attempt_at_ms: i64::MAX,
            retry_deadline_ms: deadline,
            terminal_code: Some(IndexFailureCode::IndexRetryWindowExpired),
        };
    }
    IndexRetryDecision {
        retryable: true,
        next_attempt_at_ms: next,
        retry_deadline_ms: deadline,
        terminal_code: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expectation(seconds: u64) -> VideoCompletionExpectation {
        VideoCompletionExpectation {
            stream_index: 3,
            duration_num: seconds,
            duration_den: 1,
            provenance: CompletionProvenance::StreamTicks,
            source_object_version: "object-v1".to_owned(),
        }
    }

    #[test]
    fn content_analysis_exact_two_second_completion_boundary_passes() {
        assert_eq!(expectation(12).covers(10_000, 1_000), Ok(true));
        assert_eq!(expectation(12).covers(9_999, 1_000), Ok(false));
    }

    #[test]
    fn content_analysis_retry_deadline_is_fixed_and_finite() {
        let first = index_retry_decision(
            IndexFailureCode::IndexBudgetExceeded,
            false,
            1,
            5,
            10_000,
            0,
        );
        assert!(first.retryable);
        assert_eq!(first.next_attempt_at_ms, 10_000 + INDEX_RETRY_BASE_MS);
        let later = index_retry_decision(
            IndexFailureCode::IndexProbeTimeout,
            false,
            2,
            5,
            20_000,
            first.retry_deadline_ms,
        );
        assert_eq!(later.retry_deadline_ms, first.retry_deadline_ms);
        assert_eq!(
            index_retry_decision(
                IndexFailureCode::IndexBudgetExceeded,
                false,
                5,
                5,
                30_000,
                first.retry_deadline_ms,
            )
            .retryable,
            false
        );
    }

    #[test]
    fn content_analysis_diagnostic_rejects_unbounded_stderr() {
        let diagnostic = IndexDiagnostic {
            version: 1,
            code: IndexFailureCode::IndexProcessFailed.as_str().to_owned(),
            stderr_tail: vec!["x".repeat(MAX_INDEX_STDERR_BYTES + 1)],
            ..IndexDiagnostic::default()
        };
        assert!(diagnostic.encode_bounded().is_err());
    }

    #[test]
    fn content_analysis_diagnostic_trims_escaped_stderr_to_serialized_bound() {
        let diagnostic = IndexDiagnostic {
            version: 1,
            code: IndexFailureCode::IndexProcessFailed.as_str().to_owned(),
            stderr_tail: vec!["\n".repeat(MAX_INDEX_STDERR_BYTES)],
            ..IndexDiagnostic::default()
        };
        let encoded = diagnostic.encode_bounded().expect("bounded diagnostic");
        assert!(encoded.len() <= MAX_INDEX_DIAGNOSTIC_BYTES);
        let decoded = IndexDiagnostic::decode_bounded(&encoded).expect("decoded diagnostic");
        assert!(!decoded.stderr_tail.is_empty());
    }
}
