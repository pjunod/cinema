//! Immutable verified media description bound into the existing parent recipe.
use crate::error::StoreError;
use crate::playback::candidate::CandidateId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuousFamilyDescription {
    pub version: u8,
    pub family_id: String,
    pub mode: String,
    pub master: String,
    pub video: Vec<ContinuousVideoDescription>,
    pub audio: Option<ContinuousAudioDescription>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuousVideoDescription {
    pub candidate_id: CandidateId,
    pub rendition_id: String,
    pub init_id: String,
    pub width: u32,
    pub height: u32,
    pub codec: String,
    pub timescale: u32,
    pub frame_ticks: u32,
    pub segment_ticks: u64,
    pub peak_bps: u64,
    pub playlist: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuousAudioDescription {
    pub rendition_id: String,
    pub init_id: String,
    pub codec: String,
    pub channels: u32,
    pub timescale: u32,
    pub peak_bps: u64,
    pub playlist: String,
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
impl ContinuousFamilyDescription {
    pub fn valid(&self) -> bool {
        if self.version != 1
            || !digest(&self.family_id)
            || self.mode != "autonomous_reserved"
            || self.master != "master.m3u8"
            || self.video.len() != 2
        {
            return false;
        }
        let first = &self.video[0];
        let mut candidates = std::collections::BTreeSet::new();
        let mut renditions = std::collections::BTreeSet::new();
        let mut shapes = std::collections::BTreeSet::new();
        self.video.iter().all(|row| {
            digest(&row.rendition_id)
                && digest(&row.init_id)
                && candidates.insert(row.candidate_id.0)
                && renditions.insert(&row.rendition_id)
                && shapes.insert((row.width, row.height))
                && (1..=8192).contains(&row.width)
                && (1..=8192).contains(&row.height)
                && row.codec == "avc1.640032"
                && (1..=1_000_000).contains(&row.timescale)
                && row.frame_ticks > 0
                && row.segment_ticks > 0
                && row.segment_ticks <= 9_007_199_254_740_991
                && row.segment_ticks % u64::from(row.frame_ticks) == 0
                && row.timescale == first.timescale
                && row.frame_ticks == first.frame_ticks
                && row.segment_ticks == first.segment_ticks
                && row.peak_bps > 0
                && row.peak_bps <= 9_007_199_254_740_991
                && row.playlist == format!("video/{}/index.m3u8", row.rendition_id)
        }) && self.audio.as_ref().is_none_or(|row| {
            digest(&row.rendition_id)
                && digest(&row.init_id)
                && !renditions.contains(&row.rendition_id)
                && row.codec == "mp4a.40.2"
                && row.timescale == 48_000
                && (1..=8).contains(&row.channels)
                && row.peak_bps > 0
                && row.peak_bps <= 9_007_199_254_740_991
                && row.playlist == format!("audio/{}/index.m3u8", row.rendition_id)
        })
    }
}

pub(crate) fn encode(
    description: &ContinuousFamilyDescription,
    generation: &str,
    owner: &str,
    epoch: i64,
    now_ms: i64,
) -> Result<String, StoreError> {
    if !description.valid()
        || uuid::Uuid::parse_str(generation).is_err()
        || owner.is_empty()
        || owner.len() > 128
        || owner.bytes().any(|byte| byte.is_ascii_control())
        || epoch <= 0
        || now_ms <= 0
    {
        return Err(StoreError::Task(
            "invalid verified continuous family binding".into(),
        ));
    }
    let json =
        serde_json::to_string(description).map_err(|error| StoreError::Task(error.to_string()))?;
    if json.len() > 32 * 1024 {
        return Err(StoreError::Task(
            "verified continuous family exceeds its bound".into(),
        ));
    }
    Ok(json)
}

// Only the derived proof is added. Source, intent, capabilities and executable
// recipes remain byte-for-byte equivalent JSON values. Replays cannot replace
// an existing description, even under a later owner epoch.
pub(crate) const BIND: &str = "UPDATE media_sessions SET recipe_json =
    json_set(recipe_json, '$.request.continuous_media.family_descriptor', json($1))
    WHERE incarnation_id = $2 AND owner_node_id = $3 AND owner_epoch = $4
      AND state = 'active' AND lease_expires_at_ms > $5 AND json_valid(recipe_json)
      AND json_extract(recipe_json, '$.request.continuous_media.version') = 1
      AND json_extract(recipe_json, '$.request.continuous_media.role') = 'video'
      AND json_extract(recipe_json, '$.candidate_id') IN
          (SELECT json_extract(value, '$.candidate_id') FROM json_each($1, '$.video'))
      AND json_extract(recipe_json, '$.request.continuous_media.autonomous_companion') IN
          (SELECT json_extract(value, '$.candidate_id') FROM json_each($1, '$.video'))
      AND json_extract(recipe_json, '$.candidate_id') !=
          json_extract(recipe_json, '$.request.continuous_media.autonomous_companion')
      AND (json_type(recipe_json, '$.request.continuous_media.family_descriptor') IS NULL
          OR json_extract(recipe_json, '$.request.continuous_media.family_descriptor') = $1)";
