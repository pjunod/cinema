//! Bounded catalog wire forms; no native paths or storage authority.
use crate::{identity::WireId, ticks::Ticks};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub enum ItemType {
    Movie,
    Series,
    Season,
    Episode,
    CollectionFolder,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct UserData {
    pub playback_position_ticks: Ticks,
    pub played: bool,
    pub play_count: u64,
    pub is_favorite: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub played_percentage: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unplayed_item_count: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Item {
    pub id: WireId,
    pub server_id: String,
    pub name: String,
    pub sort_name: String,
    #[serde(rename = "Type")]
    pub item_type: ItemType,
    pub is_folder: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<WireId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series_id: Option<WireId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season_id: Option<WireId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub production_year: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_time_ticks: Option<Ticks>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_number: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_index_number: Option<i32>,
    pub genres: Vec<String>,
    pub provider_ids: BTreeMap<String, String>,
    pub image_tags: BTreeMap<String, String>,
    pub backdrop_image_tags: Vec<String>,
    pub user_data: UserData,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    pub child_count: u64,
    pub recursive_item_count: u64,
    pub media_source_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_created: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_sources: Option<Vec<MediaSource>>,
    pub location_type: &'static str,
    pub etag: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Items<T> {
    pub items: Vec<T>,
    pub total_record_count: u64,
    pub start_index: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PublicSystemInfo {
    pub server_name: String,
    pub version: &'static str,
    pub product_name: &'static str,
    pub id: String,
    pub startup_wizard_completed: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct UserPolicy {
    // The facade exposes no administrative or remote-control surface.
    pub is_administrator: bool,
    pub is_hidden: bool,
    pub is_disabled: bool,
    pub enable_media_playback: bool,
    pub enable_remote_control_of_other_users: bool,
    pub enable_shared_device_control: bool,
    pub enable_content_downloading: bool,
    pub enable_all_folders: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct User {
    pub id: WireId,
    pub name: String,
    pub server_id: String,
    pub server_name: String,
    pub has_password: bool,
    pub has_configured_password: bool,
    pub enable_auto_login: bool,
    pub policy: UserPolicy,
}

/// Wire stream indices are source/global indices, never positions in this vector.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct MediaStream {
    pub index: i64,
    #[serde(rename = "Type")]
    pub stream_type: StreamType,
    pub codec: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_title: Option<String>,
    pub is_default: bool,
    pub is_forced: bool,
    pub is_external: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub enum StreamType {
    Video,
    Audio,
    Subtitle,
}

/// Probe data alone does not grant delivery. Negotiation supplies these flags.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct MediaSource {
    pub id: WireId,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_time_ticks: Option<Ticks>,
    pub media_streams: Vec<MediaStream>,
    pub supports_direct_play: bool,
    pub supports_direct_stream: bool,
    pub supports_transcoding: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_audio_stream_index: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_subtitle_stream_index: Option<i64>,
}

// Authentication responses contain a credential and intentionally have no Debug.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct AuthenticationResult {
    pub user: User,
    pub access_token: String,
    pub server_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_envelope_preserves_zero_position_and_real_empty_page_offset() {
        let data = UserData {
            playback_position_ticks: Ticks::from_milliseconds(0).expect("zero"),
            played: false,
            play_count: 0,
            is_favorite: false,
            played_percentage: None,
            unplayed_item_count: None,
        };
        let encoded = serde_json::to_value(data).expect("serialize");
        assert_eq!(encoded["PlaybackPositionTicks"], 0);
        assert!(!encoded
            .as_object()
            .expect("object")
            .contains_key("PlayedPercentage"));
        let page = Items {
            items: Vec::<Item>::new(),
            total_record_count: 2500,
            start_index: 2500,
        };
        let encoded = serde_json::to_value(page).expect("serialize");
        assert_eq!(encoded["TotalRecordCount"], 2500);
        assert_eq!(encoded["StartIndex"], 2500);
        assert!(encoded["Items"].as_array().expect("array").is_empty());
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    #[test]
    fn source_wire_keeps_global_stream_zero_and_omits_unprobed_facts_and_paths() {
        let source = MediaSource {
            id: WireId::parse("123456789abc4def8123456789abcdef").expect("wire ID"),
            name: "Edition".into(),
            container: None,
            run_time_ticks: None,
            media_streams: vec![MediaStream {
                index: 0,
                stream_type: StreamType::Audio,
                codec: "aac".into(),
                language: None,
                display_title: None,
                is_default: true,
                is_forced: false,
                is_external: false,
                width: None,
                height: None,
                channels: None,
                sample_rate: None,
                bit_depth: None,
                profile: None,
            }],
            supports_direct_play: false,
            supports_direct_stream: false,
            supports_transcoding: false,
            default_audio_stream_index: Some(0),
            default_subtitle_stream_index: None,
        };
        let value = serde_json::to_value(source).expect("source JSON");
        assert_eq!(value["DefaultAudioStreamIndex"], 0);
        assert_eq!(value["MediaStreams"][0]["Index"], 0);
        assert_eq!(value["MediaStreams"][0]["Type"], "Audio");
        for field in [
            "Path",
            "RunTimeTicks",
            "Container",
            "DefaultSubtitleStreamIndex",
        ] {
            assert!(!value.as_object().expect("object").contains_key(field));
        }
        assert!(!value["SupportsDirectPlay"]
            .as_bool()
            .expect("explicit delivery decision"));
    }
}
