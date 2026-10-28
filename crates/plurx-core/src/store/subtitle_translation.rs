//! Versioned progressive translation identities and immutable cue encoding.
//!
//! A preparation recipe is not proof of source byte equality. Only a verified
//! complete source digest can enter an asset identity; operation epochs fence
//! work without changing already accepted immutable segment bytes.

use crate::error::StoreError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const TRANSLATION_VERSION: u8 = 1;
pub const MAX_TRANSLATION_DURATION_MS: i64 = 14_400_000;
pub const MAX_TRANSLATION_SEGMENT_BYTES: usize = 16 * 1024;
pub const MAX_TRANSLATION_CUES: usize = 128;
pub const MAX_TRANSLATION_COMMAND_BYTES: usize = 128 * 1024;
pub const MAX_TRANSLATION_PAGE_ENTRIES: usize = 64;
pub const MAX_TRANSLATION_CORES: i64 = 2048;

fn invalid(reason: &str) -> StoreError {
    StoreError::Task(format!("invalid translation {reason}"))
}

pub fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Sorting recursively also works when another crate enables preserve_order.
fn ordered(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, ordered(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(ordered).collect()),
        scalar => scalar,
    }
}

pub fn canonical_bytes(value: &impl Serialize) -> Result<Vec<u8>, StoreError> {
    let value = serde_json::to_value(value).map_err(|_| invalid("JSON value"))?;
    serde_json::to_vec(&ordered(value)).map_err(|_| invalid("JSON encoding"))
}

pub fn digest(value: &impl Serialize) -> Result<String, StoreError> {
    Ok(hex::encode(Sha256::digest(canonical_bytes(value)?)))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRevision {
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
}

impl SourceRevision {
    pub fn validate(&self) -> Result<(), StoreError> {
        if self.file_id <= 0 || self.source_size <= 0 {
            return Err(invalid("source revision"));
        }
        Ok(())
    }
}

/// Speech inference tasks share source clocks and publication fences. The
/// omitted default preserves the serialized identity of existing profiles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpeechTask {
    #[default]
    #[serde(rename = "translate")]
    TranslateToEnglish,
    #[serde(rename = "transcribe")]
    Transcribe,
}
impl SpeechTask {
    pub fn is_translation(&self) -> bool {
        *self == Self::TranslateToEnglish
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::TranslateToEnglish => "translate",
            Self::Transcribe => "transcribe",
        }
    }
    pub fn output_language(self, spoken_language: &str) -> &str {
        match self {
            Self::TranslateToEnglish => "en",
            Self::Transcribe => spoken_language,
        }
    }
}

/// Selected production definitions are server-owned. A valid parameter shape
/// alone never registers a profile or qualifies a model/engine capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranslationProfile {
    #[serde(default, skip_serializing_if = "SpeechTask::is_translation")]
    pub task: SpeechTask,
    pub version: u8,
    pub model_id: String,
    pub model_sha256: String,
    pub adapter_contract: String,
    pub pipeline_revision: String,
    pub core_ms: u32,
    pub left_context_ms: u32,
    pub right_context_ms: u32,
    pub timeline_contract: String,
}

impl TranslationProfile {
    pub fn validate(&self) -> Result<(), StoreError> {
        if self.version != TRANSLATION_VERSION
            || !identifier(&self.model_id)
            || !valid_digest(&self.model_sha256)
            || self.adapter_contract != "whisper.cpp-1.9.2-cpu-v1"
            || self.pipeline_revision != "translation-window-v1"
            || self.timeline_contract != "source-audio-clock-v1"
            || !(1_000..=30_000).contains(&self.core_ms)
            || self.left_context_ms > 5_000
            || self.right_context_ms > 5_000
        {
            return Err(invalid("profile"));
        }
        Ok(())
    }

    pub fn id(&self) -> Result<String, StoreError> {
        self.validate()?;
        digest(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RangeRecipe {
    pub asset_id: String,
    pub operation_id: String,
    pub operation_epoch: i64,
    pub source: SourceRevision,
    pub source_sha256: String,
    pub profile: TranslationProfile,
    /// Audio ordinal, passed to FFmpeg as 0:a:{audio_index}.
    pub audio_index: i64,
    pub spoken_language: String,
    pub duration_ms: i64,
    pub core_index: i64,
}

impl RangeRecipe {
    pub fn validate(&self) -> Result<(), StoreError> {
        self.source.validate()?;
        self.profile.validate()?;
        if !valid_digest(&self.asset_id)
            || !valid_digest(&self.source_sha256)
            || uuid::Uuid::parse_str(&self.operation_id).is_err()
            || self.operation_epoch <= 0
            || !(0..=255).contains(&self.audio_index)
            || !(2..=3).contains(&self.spoken_language.len())
            || !self
                .spoken_language
                .bytes()
                .all(|byte| byte.is_ascii_lowercase())
            || !(1..=MAX_TRANSLATION_DURATION_MS).contains(&self.duration_ms)
            || !(0..MAX_TRANSLATION_CORES).contains(&self.core_index)
            || self.core_count() > MAX_TRANSLATION_CORES
        {
            return Err(invalid("range recipe"));
        }
        self.core_bounds()?;
        if self.asset_id != self.expected_asset_id()? {
            return Err(invalid("asset identity"));
        }
        Ok(())
    }

    pub fn core_count(&self) -> i64 {
        let width = i64::from(self.profile.core_ms.max(1));
        self.duration_ms.div_euclid(width) + i64::from(self.duration_ms.rem_euclid(width) != 0)
    }

    pub fn core_bounds(&self) -> Result<(i64, i64), StoreError> {
        let width = i64::from(self.profile.core_ms);
        let start = self
            .core_index
            .checked_mul(width)
            .filter(|start| *start >= 0 && *start < self.duration_ms)
            .ok_or_else(|| invalid("core start"))?;
        let end = start
            .checked_add(width)
            .map(|end| end.min(self.duration_ms))
            .filter(|end| *end > start)
            .ok_or_else(|| invalid("core end"))?;
        Ok((start, end))
    }

    pub fn context_bounds(&self) -> Result<(i64, i64), StoreError> {
        let (start, end) = self.core_bounds()?;
        Ok((
            start
                .saturating_sub(i64::from(self.profile.left_context_ms))
                .max(0),
            end.saturating_add(i64::from(self.profile.right_context_ms))
                .min(self.duration_ms),
        ))
    }

    pub fn expected_asset_id(&self) -> Result<String, StoreError> {
        digest(&json!({
            "version": TRANSLATION_VERSION,
            "task": self.profile.task.name(),
            "source": self.source,
            "source_sha256": self.source_sha256,
            "audio_index": self.audio_index,
            "spoken_language": self.spoken_language,
            "output_language": self.profile.task.output_language(&self.spoken_language),
            "profile_id": self.profile.id()?,
            "duration_ms": self.duration_ms,
        }))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranslationCue {
    pub cue_id: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

pub fn cue_id(
    asset: &str,
    core_index: i64,
    ordinal: usize,
    start_ms: i64,
    end_ms: i64,
    text: &str,
) -> Result<String, StoreError> {
    digest(&(
        TRANSLATION_VERSION,
        asset,
        core_index,
        ordinal,
        start_ms,
        end_ms,
        text,
    ))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionProvenance {
    pub engine_sha256: String,
    pub model_sha256: String,
    pub operation_epoch: i64,
    pub platform: String,
    pub profile_id: String,
    pub source_sha256: String,
    pub worker_fence: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentDraft {
    pub asset_id: String,
    pub core_index: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub cues: Vec<TranslationCue>,
    pub provenance: ExecutionProvenance,
}

impl SegmentDraft {
    pub fn segment_id(&self) -> String {
        // The identifier is independent of inference bytes and their checksum.
        hex::encode(Sha256::digest(format!(
            "translation-core-v1:{}:{}",
            self.asset_id, self.core_index
        )))
    }

    pub fn validate(&self, recipe: &RangeRecipe) -> Result<(), StoreError> {
        recipe.validate()?;
        let bounds = recipe.core_bounds()?;
        let p = &self.provenance;
        if self.asset_id != recipe.asset_id
            || self.core_index != recipe.core_index
            || (self.start_ms, self.end_ms) != bounds
            || self.cues.len() > MAX_TRANSLATION_CUES
            || !valid_digest(&p.engine_sha256)
            || p.model_sha256 != recipe.profile.model_sha256
            || p.source_sha256 != recipe.source_sha256
            || p.profile_id != recipe.profile.id()?
            || p.operation_epoch != recipe.operation_epoch
            || p.worker_fence <= 0
            || !matches!(
                p.platform.as_str(),
                "macos-arm64"
                    | "macos-x86_64"
                    | "macos-universal"
                    | "linux-aarch64"
                    | "linux-arm64"
                    | "linux-x86_64"
                    | "linux-amd64"
            )
        {
            return Err(invalid("segment provenance or bounds"));
        }
        let mut previous = self.start_ms;
        for (ordinal, cue) in self.cues.iter().enumerate() {
            if cue.start_ms < previous
                || cue.start_ms < self.start_ms
                || cue.end_ms > self.end_ms
                || cue.end_ms <= cue.start_ms
                || cue.text.trim().is_empty()
                || cue.text.len() > 4096
                || cue
                    .text
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t')
                || cue.cue_id
                    != cue_id(
                        &self.asset_id,
                        self.core_index,
                        ordinal,
                        cue.start_ms,
                        cue.end_ms,
                        &cue.text,
                    )?
            {
                return Err(invalid("cue"));
            }
            previous = cue.start_ms;
        }
        if self.canonical_body()?.len() > MAX_TRANSLATION_SEGMENT_BYTES {
            return Err(invalid("segment byte bound"));
        }
        Ok(())
    }

    /// Exact immutable wire bytes. The checksum and changing server clock live
    /// outside this body. Epoch/fence counters cross JavaScript as strings.
    pub fn canonical_body(&self) -> Result<Vec<u8>, StoreError> {
        let p = &self.provenance;
        canonical_bytes(&json!({
            "version": TRANSLATION_VERSION,
            "asset_id": self.asset_id,
            "segment_id": self.segment_id(),
            "core_index": self.core_index,
            "start_ms": self.start_ms,
            "end_ms": self.end_ms,
            "cues": self.cues,
            "provenance": {
                "engine_sha256": p.engine_sha256,
                "model_sha256": p.model_sha256,
                "operation_epoch": p.operation_epoch.to_string(),
                "platform": p.platform,
                "profile_id": p.profile_id,
                "source_sha256": p.source_sha256,
                "worker_fence": p.worker_fence.to_string(),
            },
        }))
    }
}
