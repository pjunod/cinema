//! Immutable verified media description bound into the existing parent recipe.
use crate::error::StoreError;
use crate::playback::candidate::CandidateId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuousFamilyDescription {
    pub version: u8,
    #[serde(default, skip_serializing_if = "revision_is_legacy")]
    pub family_revision: u8,
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
fn revision_is_legacy(value: &u8) -> bool {
    *value == 0
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
impl ContinuousFamilyDescription {
    pub fn valid(&self) -> bool {
        if !matches!(self.version, 1 | 2)
            || !digest(&self.family_id)
            || !matches!(self.mode.as_str(), "autonomous_reserved" | "controlled")
            || self.master != "master.m3u8"
            || match self.version {
                1 => self.video.len() != 2 || self.family_revision != 0,
                2 => {
                    self.mode != "controlled"
                        || !(1..=2).contains(&self.video.len())
                        || usize::from(self.family_revision) != self.video.len()
                }
                _ => true,
            }
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
// recipes remain byte-for-byte equivalent JSON values. V1 is immutable. V2
// admits only an additive original companion while preserving the primary,
// soundtrack, anchor identity and exact accepted owner epoch.
pub(crate) const BIND: &str = "UPDATE media_sessions SET recipe_json =
    json_set(recipe_json, '$.request.continuous_media.family_descriptor', json($1))
    WHERE incarnation_id = $2 AND owner_node_id = $3 AND owner_epoch = $4
      AND state = 'active' AND lease_expires_at_ms > $5 AND json_valid(recipe_json)
      AND json_extract(recipe_json, '$.request.continuous_media.version') = json_extract($1, '$.version')
      AND json_extract(recipe_json, '$.request.continuous_media.role') = 'video'
      AND json_extract(recipe_json, '$.candidate_id') IN
          (SELECT json_extract(value, '$.candidate_id') FROM json_each($1, '$.video'))
      AND (json_extract(recipe_json, '$.request.continuous_media.autonomous_companion') IN
          (SELECT json_extract(value, '$.candidate_id') FROM json_each($1, '$.video'))
          OR (json_extract($1, '$.version') = 2 AND json_array_length($1, '$.video') = 1))
      AND NOT EXISTS (SELECT 1 FROM json_each($1, '$.video')
          WHERE json_extract(value, '$.candidate_id') NOT IN
            (json_extract(recipe_json, '$.candidate_id'),
             json_extract(recipe_json, '$.request.continuous_media.autonomous_companion')))
      AND json_extract(recipe_json, '$.candidate_id') !=
          json_extract(recipe_json, '$.request.continuous_media.autonomous_companion')
      AND (json_type(recipe_json, '$.request.continuous_media.family_descriptor') IS NULL
          OR json_extract(recipe_json, '$.request.continuous_media.family_descriptor') = $1
          OR (json_extract(recipe_json, '$.request.continuous_media.family_descriptor.version') = 2
              AND json_extract($1, '$.version') = 2
              AND json_extract(recipe_json, '$.request.continuous_media.family_descriptor.family_id') = json_extract($1, '$.family_id')
              AND json_extract(recipe_json, '$.request.continuous_media.family_descriptor.mode') = json_extract($1, '$.mode')
              AND json_extract(recipe_json, '$.request.continuous_media.family_descriptor.audio') IS json_extract($1, '$.audio')
              AND json_extract($1, '$.family_revision') = json_extract(recipe_json, '$.request.continuous_media.family_descriptor.family_revision') + 1
              AND NOT EXISTS (SELECT 1 FROM json_each(recipe_json, '$.request.continuous_media.family_descriptor.video') AS old
                  WHERE NOT EXISTS (SELECT 1 FROM json_each($1, '$.video') AS new WHERE json(old.value) = json(new.value)))))";

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, Connection};
    fn description() -> ContinuousFamilyDescription {
        serde_json::from_value(serde_json::json!({
            "version":2,"family_revision":1,"family_id":"a".repeat(64),"mode":"controlled","master":"master.m3u8",
            "video":[{"candidate_id":"1".repeat(32),"rendition_id":"b".repeat(64),"init_id":"c".repeat(64),
                "width":1280,"height":720,"codec":"avc1.640032","timescale":24000,"frame_ticks":1001,
                "segment_ticks":48048,"peak_bps":1000000,"playlist":format!("video/{}/index.m3u8","b".repeat(64))}],"audio":null
        })).expect("descriptor")
    }
    #[test]
    fn versioned_family_binding_only_adds_original_companion_and_preserves_verified_rows() {
        let conn = Connection::open_in_memory().expect("database");
        conn.execute_batch("CREATE TABLE media_sessions(incarnation_id TEXT, owner_node_id TEXT, owner_epoch INTEGER,
            state TEXT, lease_expires_at_ms INTEGER, recipe_json TEXT);").expect("schema");
        let recipe = serde_json::json!({"candidate_id":"1".repeat(32),"request":{"continuous_media":{
            "version":2,"role":"video","autonomous_companion":"2".repeat(32)}}});
        conn.execute(
            "INSERT INTO media_sessions VALUES('generation','owner',1,'active',100,?1)",
            [recipe.to_string()],
        )
        .expect("route");
        let bind = |description: &ContinuousFamilyDescription| {
            conn.execute(
                BIND,
                params![
                    serde_json::to_string(description).expect("JSON"),
                    "generation",
                    "owner",
                    1,
                    1
                ],
            )
            .expect("bind SQL")
        };
        let first = description();
        assert!(first.valid());
        assert_eq!(bind(&first), 1);
        let mut second = first.clone();
        second.family_revision = 2;
        let mut companion = first.video[0].clone();
        companion.candidate_id = crate::playback::candidate::CandidateId([0x22; 16]);
        companion.rendition_id = "d".repeat(64);
        companion.init_id = "e".repeat(64);
        companion.width = 1920;
        companion.height = 1080;
        companion.playlist = format!("video/{}/index.m3u8", companion.rendition_id);
        second.video.push(companion);
        assert!(second.valid());
        let mut substituted = second.clone();
        substituted.video[0].init_id = "f".repeat(64);
        assert_eq!(bind(&substituted), 0);
        let mut unnegotiated = second.clone();
        unnegotiated.video[1].candidate_id = crate::playback::candidate::CandidateId([0x33; 16]);
        assert_eq!(bind(&unnegotiated), 0);
        assert_eq!(bind(&second), 1);
        assert_eq!(bind(&second), 1);
        assert_eq!(bind(&first), 0);
        let mut changed = second.clone();
        changed.family_id = "0".repeat(64);
        assert_eq!(bind(&changed), 0);
    }
}
