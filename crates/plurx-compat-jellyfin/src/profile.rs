//! Bounded protocol predicates, independent of native delivery planning.
//! Unknown property/operator names fail deserialization; no predicate is discarded.
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum Property {
    AudioChannels,
    AudioBitrate,
    AudioProfile,
    Width,
    Height,
    Has64BitOffsets,
    PacketLength,
    VideoBitDepth,
    VideoBitrate,
    VideoFramerate,
    VideoLevel,
    VideoProfile,
    VideoTimestamp,
    IsAnamorphic,
    RefFrames,
    NumAudioStreams,
    NumVideoStreams,
    IsSecondaryAudio,
    VideoCodecTag,
    IsAvc,
    IsInterlaced,
    AudioSampleRate,
    AudioBitDepth,
    VideoRangeType,
    NumStreams,
}
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub enum Operator {
    Equals,
    NotEquals,
    LessThanEqual,
    GreaterThanEqual,
    EqualsAny,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
pub struct Condition {
    pub condition: Operator,
    pub property: Property,
    pub value: Option<String>,
    #[serde(default)]
    pub is_required: bool,
}
#[derive(Clone, Debug)]
pub enum Fact {
    Number(f64),
    Text(String),
    Boolean(bool),
}
pub type Facts = BTreeMap<Property, Fact>;

impl Property {
    fn numeric(self) -> bool {
        matches!(
            self,
            Self::AudioChannels
                | Self::AudioBitrate
                | Self::Width
                | Self::Height
                | Self::PacketLength
                | Self::VideoBitDepth
                | Self::VideoBitrate
                | Self::VideoFramerate
                | Self::VideoLevel
                | Self::RefFrames
                | Self::NumAudioStreams
                | Self::NumVideoStreams
                | Self::AudioSampleRate
                | Self::AudioBitDepth
                | Self::NumStreams
        )
    }
    fn boolean(self) -> bool {
        matches!(
            self,
            Self::Has64BitOffsets
                | Self::IsAnamorphic
                | Self::IsSecondaryAudio
                | Self::IsAvc
                | Self::IsInterlaced
        )
    }
}
impl Condition {
    pub fn is_valid(&self) -> bool {
        let Some(value) = self
            .value
            .as_deref()
            .filter(|s| !s.is_empty() && s.len() <= 1024)
        else {
            return false;
        };
        let values: Vec<_> = if self.condition == Operator::EqualsAny {
            value.split('|').collect()
        } else {
            vec![value]
        };
        if values.len() > 64 || values.iter().any(|v| v.is_empty()) {
            return false;
        }
        if self.property.numeric() {
            return values
                .iter()
                .all(|v| v.parse::<f64>().is_ok_and(|v| v.is_finite()));
        }
        if self.property.boolean() {
            return matches!(self.condition, Operator::Equals | Operator::NotEquals)
                && value.to_ascii_lowercase().parse::<bool>().is_ok();
        }
        matches!(
            self.condition,
            Operator::Equals | Operator::NotEquals | Operator::EqualsAny
        )
    }
    /// Optional missing *known* source facts follow the pinned reference.
    /// Malformed values still refuse, and native HDR/source safety remains separate.
    pub fn accepts(&self, facts: &Facts) -> bool {
        if !self.is_valid() {
            return false;
        }
        let Some(value) = self
            .value
            .as_deref()
            .filter(|s| !s.is_empty() && s.len() <= 1024)
        else {
            return false;
        };
        let values: Vec<_> = if self.condition == Operator::EqualsAny {
            value.split('|').collect()
        } else {
            vec![value]
        };
        if values.len() > 64 {
            return false;
        }
        if self.property.numeric() {
            let Ok(numbers) = values
                .iter()
                .map(|v| v.parse::<f64>())
                .collect::<Result<Vec<_>, _>>()
            else {
                return false;
            };
            if numbers.iter().any(|n| !n.is_finite()) {
                return false;
            }
            let Some(fact) = facts.get(&self.property) else {
                return !self.is_required;
            };
            let Fact::Number(current) = fact else {
                return false;
            };
            if !current.is_finite() {
                return false;
            }
            return match self.condition {
                Operator::Equals => *current == numbers[0],
                Operator::NotEquals => *current != numbers[0],
                Operator::LessThanEqual => *current <= numbers[0],
                Operator::GreaterThanEqual => *current >= numbers[0],
                Operator::EqualsAny => numbers.contains(current),
            };
        }
        if self.property.boolean() {
            if !matches!(self.condition, Operator::Equals | Operator::NotEquals) {
                return false;
            }
            let Ok(expected) = value.to_ascii_lowercase().parse::<bool>() else {
                return false;
            };
            let Some(fact) = facts.get(&self.property) else {
                return !self.is_required;
            };
            let Fact::Boolean(current) = fact else {
                return false;
            };
            return (*current == expected) == (self.condition == Operator::Equals);
        }
        if !matches!(
            self.condition,
            Operator::Equals | Operator::NotEquals | Operator::EqualsAny
        ) {
            return false;
        }
        let Some(fact) = facts.get(&self.property) else {
            return !self.is_required;
        };
        let Fact::Text(current) = fact else {
            return false;
        };
        if current.is_empty() {
            return !self.is_required;
        }
        let equal = values.iter().any(|v| v.eq_ignore_ascii_case(current));
        if self.condition == Operator::NotEquals {
            !equal
        } else {
            equal
        }
    }
}
pub fn all_conditions(conditions: &[Condition], facts: &Facts) -> bool {
    conditions.len() <= 64 && conditions.iter().all(|condition| condition.accepts(facts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn condition(property: &str, operator: &str, value: &str, required: bool) -> Condition {
        serde_json::from_value(
            json!({"Property":property,"Condition":operator,"Value":value,"IsRequired":required}),
        )
        .expect("condition")
    }
    #[test]
    fn profile_conditions_preserve_known_missing_and_refuse_unknown_or_malformed_constraints() {
        let facts = Facts::new();
        assert!(condition("Height", "LessThanEqual", "1080", false).accepts(&facts));
        assert!(!condition("Height", "LessThanEqual", "1080", true).accepts(&facts));
        for value in ["NaN", "inf", "nonsense", ""] {
            assert!(!condition("Height", "LessThanEqual", value, false).accepts(&facts));
        }
        assert!(serde_json::from_value::<Condition>(json!({"Property":"FutureHDRRule","Condition":"Equals","Value":"true","IsRequired":false})).is_err());
        assert!(serde_json::from_value::<Condition>(
            json!({"Property":"Height","Condition":"FutureOperator","Value":"1"})
        )
        .is_err());
    }
    #[test]
    fn profile_conditions_apply_numeric_string_boolean_and_joint_limits_without_union() {
        let facts = BTreeMap::from([
            (Property::Height, Fact::Number(2160.0)),
            (Property::VideoProfile, Fact::Text("Main 10".into())),
            (Property::IsInterlaced, Fact::Boolean(false)),
        ]);
        assert!(condition("VideoProfile", "EqualsAny", "main|MAIN 10", true).accepts(&facts));
        assert!(condition("IsInterlaced", "NotEquals", "TRUE", true).accepts(&facts));
        assert!(!condition("VideoProfile", "LessThanEqual", "main", true).accepts(&facts));
        assert!(!all_conditions(
            &[
                condition("Height", "LessThanEqual", "1080", true),
                condition("VideoProfile", "Equals", "Main 10", true)
            ],
            &facts
        ));
        assert!(all_conditions(
            &[
                condition("Height", "LessThanEqual", "2160", true),
                condition("VideoProfile", "Equals", "Main 10", true)
            ],
            &facts
        ));
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub enum CodecKind {
    Video,
    VideoAudio,
    Audio,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
pub struct CodecRule {
    #[serde(rename = "Type")]
    pub kind: CodecKind,
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub container: Option<String>,
    #[serde(default)]
    pub sub_container: Option<String>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
    #[serde(default)]
    pub apply_conditions: Vec<Condition>,
}
fn selected(selector: Option<&str>, value: Option<&str>) -> bool {
    let Some(selector) = selector.filter(|s| !s.is_empty()) else {
        return true;
    };
    value.is_some_and(|value| {
        selector
            .split(',')
            .any(|name| name.trim().eq_ignore_ascii_case(value))
    })
}
impl CodecRule {
    /// Evaluate only the exact source/output tuple presented by the adapter.
    /// ApplyConditions govern relevance; Conditions restrict an applicable tuple.
    pub fn accepts(
        &self,
        kind: CodecKind,
        codec: Option<&str>,
        container: Option<&str>,
        hls_subcontainer: Option<&str>,
        facts: &Facts,
    ) -> bool {
        if self
            .conditions
            .iter()
            .chain(&self.apply_conditions)
            .any(|condition| !condition.is_valid())
        {
            return false;
        }
        if self.conditions.len() > 64
            || self.apply_conditions.len() > 64
            || [&self.codec, &self.container, &self.sub_container]
                .iter()
                .any(|s| {
                    s.as_ref()
                        .is_some_and(|s| s.len() > 4096 || s.split(',').count() > 32)
                })
        {
            return false;
        }
        if self.kind != kind {
            return true;
        }
        let container_matches = if self
            .container
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case("hls"))
            && hls_subcontainer.is_some()
        {
            selected(self.sub_container.as_deref(), hls_subcontainer)
        } else {
            selected(self.container.as_deref(), container)
        };
        if !container_matches || !selected(self.codec.as_deref(), codec) {
            return true;
        }
        !all_conditions(&self.apply_conditions, facts) || all_conditions(&self.conditions, facts)
    }
}
#[cfg(test)]
mod rule_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn codec_rules_keep_container_codec_and_apply_conditions_attached() {
        let rule: CodecRule = serde_json::from_value(json!({"Type":"Video", "Codec":"h264", "Container":"mp4",
            "ApplyConditions":[{"Property":"Height","Condition":"GreaterThanEqual","Value":"1080","IsRequired":true}],
            "Conditions":[{"Property":"VideoProfile","Condition":"Equals","Value":"main","IsRequired":true}]})).expect("rule");
        let facts = Facts::from([
            (Property::Height, Fact::Number(2160.0)),
            (Property::VideoProfile, Fact::Text("high".into())),
        ]);
        assert!(!rule.accepts(CodecKind::Video, Some("h264"), Some("mp4"), None, &facts));
        assert!(rule.accepts(CodecKind::Video, Some("hevc"), Some("mp4"), None, &facts));
        assert!(rule.accepts(CodecKind::Video, Some("h264"), Some("mkv"), None, &facts));
        let lower = Facts::from([(Property::Height, Fact::Number(720.0))]);
        assert!(rule.accepts(CodecKind::Video, Some("h264"), Some("mp4"), None, &lower));
        assert!(serde_json::from_value::<CodecRule>(
            json!({"Type":"Video", "FutureConstraint":true})
        )
        .is_err());
    }
}

#[cfg(test)]
mod malformed_rule_tests {
    use super::*;
    #[test]
    fn malformed_apply_predicate_cannot_silently_remove_a_codec_constraint() {
        let rule: CodecRule = serde_json::from_str(r#"{"Type":"Video","Codec":"h264","ApplyConditions":[{"Property":"Height","Condition":"LessThanEqual","Value":"NaN"}],"Conditions":[{"Property":"VideoLevel","Condition":"LessThanEqual","Value":"1","IsRequired":true}]}"#).expect("shape");
        assert!(!rule.accepts(
            CodecKind::Video,
            Some("h264"),
            Some("mp4"),
            None,
            &Facts::new()
        ));
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub enum MediaKind {
    Video,
    Audio,
    Photo,
    Subtitle,
    Lyric,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
pub struct ContainerRule {
    #[serde(rename = "Type")]
    pub kind: MediaKind,
    #[serde(default)]
    pub container: Option<String>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
}
impl ContainerRule {
    pub fn accepts(&self, kind: MediaKind, container: Option<&str>, facts: &Facts) -> bool {
        if self.conditions.len() > 64
            || self.conditions.iter().any(|c| !c.is_valid())
            || self
                .container
                .as_ref()
                .is_some_and(|c| c.len() > 4096 || c.split(',').count() > 32)
        {
            return false;
        }
        self.kind != kind
            || !selected(self.container.as_deref(), container)
            || all_conditions(&self.conditions, facts)
    }
}
#[cfg(test)]
mod container_tests {
    use super::*;
    #[test]
    fn container_predicates_stay_scoped_and_required_absent_facts_refuse() {
        let rule: ContainerRule = serde_json::from_str(r#"{"Type":"Video","Container":"mp4","Conditions":[{"Property":"Has64BitOffsets","Condition":"Equals","Value":"false","IsRequired":true}]}"#).expect("rule");
        assert!(!rule.accepts(MediaKind::Video, Some("mp4"), &Facts::new()));
        assert!(rule.accepts(MediaKind::Video, Some("mkv"), &Facts::new()));
        assert!(rule.accepts(
            MediaKind::Video,
            Some("mp4"),
            &Facts::from([(Property::Has64BitOffsets, Fact::Boolean(false))])
        ));
    }
}

/// One ordered output tuple. The adapter must validate its native delivery
/// against this entry; entries are never merged into a combined capability.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
pub struct TranscodingRule {
    #[serde(rename = "Type")]
    pub kind: MediaKind,
    pub container: String,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub protocol: String,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
    #[serde(default)]
    pub max_audio_channels: Option<String>,
    #[serde(default)]
    pub estimate_content_length: bool,
    #[serde(default, rename = "EnableMpegtsM2TsMode")]
    pub enable_mpegts_m2_ts_mode: bool,
    #[serde(default)]
    pub transcode_seek_info: Option<String>,
    #[serde(default)]
    pub copy_timestamps: bool,
    #[serde(default)]
    pub enable_subtitles_in_manifest: bool,
    #[serde(default)]
    pub min_segments: u32,
    #[serde(default)]
    pub segment_length: u32,
    #[serde(default)]
    pub break_on_non_key_frames: bool,
    #[serde(default)]
    pub enable_audio_vbr_encoding: Option<bool>,
}
impl TranscodingRule {
    pub fn valid(&self) -> bool {
        let codec_list = |s: Option<&str>| {
            s.is_some_and(|s| {
                s.len() <= 4096
                    && s.split(',').count() <= 32
                    && s.split(',').all(|v| {
                        !v.trim().is_empty()
                            && v.trim()
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    })
            })
        };
        self.kind == MediaKind::Video
            && self.container.len() <= 32
            && !self.container.is_empty()
            && self.container.bytes().all(|b| b.is_ascii_alphanumeric())
            && matches!(self.protocol.as_str(), "hls" | "http")
            && self
                .context
                .as_deref()
                .is_none_or(|s| matches!(s, "Streaming" | "Static"))
            && codec_list(self.video_codec.as_deref())
            && codec_list(self.audio_codec.as_deref())
            && self.conditions.len() <= 64
            && self.conditions.iter().all(Condition::is_valid)
            && self
                .max_audio_channels
                .as_deref()
                .is_none_or(|s| s.parse::<u32>().is_ok_and(|n| (1..=32).contains(&n)))
            && self
                .transcode_seek_info
                .as_deref()
                .is_none_or(|s| matches!(s, "Auto" | "Bytes"))
            && self.min_segments <= 64
            && self.segment_length <= 60
    }
    pub fn accepts_output(&self, container: &str, video: &str, audio: &str, facts: &Facts) -> bool {
        let contains = |list: Option<&str>, codec: &str| {
            list.is_some_and(|list| {
                list.split(',')
                    .any(|v| v.trim().eq_ignore_ascii_case(codec))
            })
        };
        self.valid()
            && self.container.eq_ignore_ascii_case(container)
            && contains(self.video_codec.as_deref(), video)
            && contains(self.audio_codec.as_deref(), audio)
            && all_conditions(&self.conditions, facts)
            && self.max_audio_channels.as_deref().is_none_or(|s| {
                let Some(Fact::Number(channels)) = facts.get(&Property::AudioChannels) else {
                    return false;
                };
                channels.is_finite() && *channels <= s.parse::<f64>().unwrap_or(0.0)
            })
    }
}

#[cfg(test)]
mod transcoding_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn transcoding_profiles_keep_each_output_tuple_and_conditions_separate() {
        let rules: Vec<TranscodingRule> = serde_json::from_value(json!([
            {"Type":"Video","Container":"mp4","Protocol":"hls","VideoCodec":"hevc","AudioCodec":"aac","MaxAudioChannels":"2"},
            {"Type":"Video","Container":"webm","Protocol":"http","VideoCodec":"vp9","AudioCodec":"opus"}
        ])).expect("profiles");
        let facts = Facts::from([(Property::AudioChannels, Fact::Number(2.0))]);
        assert!(rules[0].accepts_output("mp4", "hevc", "aac", &facts));
        assert!(!rules
            .iter()
            .any(|r| r.accepts_output("mp4", "vp9", "opus", &facts)));
        assert!(!rules[0].accepts_output("mp4", "hevc", "aac", &Facts::new()));
        let six = Facts::from([(Property::AudioChannels, Fact::Number(6.0))]);
        assert!(!rules[0].accepts_output("mp4", "hevc", "aac", &six));
    }
    #[test]
    fn transcoding_profiles_refuse_unknown_constraints_and_unbounded_selectors() {
        let mut value = json!({"Type":"Video","Container":"mp4","Protocol":"hls","VideoCodec":"h264","AudioCodec":"aac"});
        value["Conditions"] = json!([{"Property":"FutureHdr","Condition":"Equals","Value":"true"}]);
        assert!(serde_json::from_value::<TranscodingRule>(value.clone()).is_err());
        value["Conditions"] = json!([]);
        value["VideoCodec"] = json!("h264,,hevc");
        assert!(!serde_json::from_value::<TranscodingRule>(value.clone())
            .expect("shape")
            .valid());
        value["VideoCodec"] = json!("h264");
        value["MaxAudioChannels"] = json!("NaN");
        assert!(!serde_json::from_value::<TranscodingRule>(value)
            .expect("shape")
            .valid());
    }
}
