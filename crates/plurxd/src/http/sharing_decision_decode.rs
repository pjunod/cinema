//! Closed owned decoder for the complete actual engine decision wire.
use plurx_core::sharing_resources::SharingResourceUnsupported;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AudioTrackDto {
    pub index: i64,
    pub codec: String,
    pub channels: Option<i64>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub default: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubTrackDto {
    pub index: i64,
    pub codec: String,
    pub language: Option<String>,
    pub title: Option<String>,
    pub default: bool,
    pub forced: bool,
    pub text: bool,
    pub native: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overlay: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceSummary {
    pub container: Option<String>,
    pub video_codec: Option<String>,
    pub video_profile: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub bit_depth: Option<i64>,
    pub hdr: Option<String>,
    pub hdr_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dv_profile: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dv_el_present: Option<bool>,
    pub bitrate: Option<i64>,
    pub duration_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    pub kind: String,
    pub label: String,
    pub start_ms: i64,
    pub end_ms: i64,

    pub chapter: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detector_version: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionSelection {
    pub audio_index: Option<i64>,
    pub subtitle_index: Option<i64>,
    pub subtitle_requires_burn_in: bool,
    pub subtitle_burn_in_blocked_by_hdr: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle_route: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum DownmixMatrix {
    RequiresLayoutMeasurement { source_channels: u8 },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum AudioAction {
    None,
    Copy {
        codec: String,
        channels: u8,
    },
    Encode {
        codec: String,
        channels: u8,
        #[serde(skip_serializing_if = "Option::is_none")]
        layout: Option<String>,
        bitrate_kbps: u32,
        sample_rate: u32,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AudioDelivery {
    pub action: AudioAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub downmix: Option<DownmixMatrix>,
    pub reason: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlaybackMethod {
    DirectPlay,
    Remux,
    Transcode,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum DeliveryPlan {
    Direct {
        url: String,
    },
    Remux {
        url: String,
        sessions_url: String,
        aac: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        requires_hls: bool,
        preserve_dolby_vision: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        audio: Option<i64>,
    },
    Transcode {
        sessions_url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        audio: Option<i64>,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EngineDecision {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_aware_auto_protocol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_candidate_id: Option<plurx_core::playback::candidate::CandidateId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_candidates: Option<Vec<plurx_core::playback::candidate::QualityCandidate>>,
    pub file_id: i64,
    pub vod_indexed: bool,

    pub method: PlaybackMethod,
    pub reasons: Vec<String>,
    pub transcode_audio: bool,
    pub delivered_audio: AudioDelivery,
    pub preserve_dolby_vision: bool,
    #[serde(default)]
    pub convert_dolby_vision: bool,
    pub container: String,
    pub delivered_dynamic_range: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_dolby_vision_profile: Option<u8>,

    pub play_url: String,
    pub delivery: DeliveryPlan,
    pub source: SourceSummary,
    pub audio: Vec<AudioTrackDto>,
    pub subtitles: Vec<SubTrackDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection: Option<DecisionSelection>,
    pub markers: Vec<Marker>,
    pub audio_offset_ms: i64,
    pub declared_offset_ms: Option<i64>,
    pub ladder: Vec<Rung>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prior_kbps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefer_segmented: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rung {
    height: i64,
    total_kbps: u32,
    peak_kbps: u32,
}

/// Retains the whole validated payload. Deserialization is private so raw
/// peer JSON never becomes an accepted decision without these checks.
pub(super) struct DecodedDecision(Value);
impl DecodedDecision {
    pub(super) fn parse(payload: Value, file_id: i64) -> Result<Self, SharingResourceUnsupported> {
        let decoded: EngineDecision =
            serde_json::from_value(payload.clone()).map_err(|_| SharingResourceUnsupported)?;
        if file_id < 0
            || decoded.file_id != file_id
            || decoded.prior_kbps.is_some()
            || !matches!(
                (&decoded.method, &decoded.delivery),
                (PlaybackMethod::DirectPlay, DeliveryPlan::Direct { .. })
                    | (PlaybackMethod::Remux, DeliveryPlan::Remux { .. })
                    | (PlaybackMethod::Transcode, DeliveryPlan::Transcode { .. })
            )
            || decoded.reasons.len() > 64
            || decoded.audio.len() > 64
            || decoded.subtitles.len() > 64
            || decoded.markers.len() > 512
            || decoded.ladder.len() > 64
            || decoded
                .quality_candidates
                .as_ref()
                .is_some_and(|values| values.len() > 64)
            || serde_json::to_value(&decoded).map_err(|_| SharingResourceUnsupported)? != payload
        {
            return Err(SharingResourceUnsupported);
        }
        Ok(Self(payload))
    }
    pub(super) fn into_payload(self) -> Value {
        self.0
    }
}

/// Bound JSON heap shape while parsing the peer body, before a generic Value
/// or the typed mirror can allocate an attacker-sized tree. Duplicate keys
/// cannot change the accepted engine object through last-key-wins behavior.
pub(super) fn bounded_decision_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Value, D::Error> {
    use serde::de::{DeserializeSeed, Error, MapAccess, SeqAccess, Visitor};
    struct Seed<'a> {
        left: &'a mut usize,
        depth: usize,
    }
    impl<'de> DeserializeSeed<'de> for Seed<'_> {
        type Value = Value;
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
            if self.depth > 32 || *self.left == 0 {
                return Err(D::Error::custom("decision JSON shape exceeds bound"));
            }
            *self.left -= 1;
            d.deserialize_any(self)
        }
    }
    impl<'de> Visitor<'de> for Seed<'_> {
        type Value = Value;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("bounded engine JSON")
        }
        fn visit_bool<E: Error>(self, v: bool) -> Result<Value, E> {
            Ok(Value::Bool(v))
        }
        fn visit_i64<E: Error>(self, v: i64) -> Result<Value, E> {
            Ok(Value::Number(v.into()))
        }
        fn visit_u64<E: Error>(self, v: u64) -> Result<Value, E> {
            Ok(Value::Number(v.into()))
        }
        fn visit_f64<E: Error>(self, v: f64) -> Result<Value, E> {
            serde_json::Number::from_f64(v)
                .map(Value::Number)
                .ok_or_else(|| E::custom("invalid engine number"))
        }
        fn visit_str<E: Error>(self, v: &str) -> Result<Value, E> {
            Ok(Value::String(v.to_owned()))
        }
        fn visit_string<E: Error>(self, v: String) -> Result<Value, E> {
            Ok(Value::String(v))
        }
        fn visit_unit<E: Error>(self) -> Result<Value, E> {
            Ok(Value::Null)
        }
        fn visit_none<E: Error>(self) -> Result<Value, E> {
            Ok(Value::Null)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
            let mut values = Vec::new();
            while let Some(value) = seq.next_element_seed(Seed {
                left: self.left,
                depth: self.depth + 1,
            })? {
                values.push(value);
            }
            Ok(Value::Array(values))
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
            let mut values = serde_json::Map::new();
            while let Some(key) = map.next_key::<String>()? {
                if key.len() > 128 || values.contains_key(&key) || *self.left == 0 {
                    return Err(A::Error::custom("invalid engine object shape"));
                }
                *self.left -= 1;
                let value = map.next_value_seed(Seed {
                    left: self.left,
                    depth: self.depth + 1,
                })?;
                values.insert(key, value);
            }
            Ok(Value::Object(values))
        }
    }
    Seed {
        left: &mut 16_384,
        depth: 0,
    }
    .deserialize(deserializer)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(deserialize_with = "bounded_decision_value")]
        decision: Value,
    }
    #[test]
    fn sharing_decision_json_budget_refuses_duplicate_keys_depth_and_heap_amplification() {
        let good = serde_json::json!({"decision":[true]});
        let parsed: Envelope = serde_json::from_value(good).expect("small bounded value");
        assert_eq!(parsed.decision, serde_json::json!([true]));
        assert!(
            serde_json::from_str::<Envelope>(r#"{"decision":{"file_id":1,"file_id":2}}"#).is_err()
        );
        let accepted = serde_json::json!({"decision":vec![true;16_382]});
        assert!(serde_json::from_value::<Envelope>(accepted).is_ok());
        let amplified = serde_json::json!({"decision":vec![true;16_384]});
        assert!(serde_json::from_value::<Envelope>(amplified).is_err());
        let mut deep = Value::Bool(true);
        for _ in 0..33 {
            deep = Value::Array(vec![deep]);
        }
        assert!(serde_json::from_value::<Envelope>(serde_json::json!({"decision":deep})).is_err());
        assert!(serde_json::from_value::<Envelope>(
            serde_json::json!({"decision":{"x".repeat(129):true}})
        )
        .is_err());
    }
}
