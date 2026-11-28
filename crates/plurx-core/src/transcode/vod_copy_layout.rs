/// The actual VOD copy layout is sealed with the route before publication.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VodCopyLayout {
    PendingV1,
    Muxed,
    SplitV1 {
        video: VodSplitVideoIdentity,
        soundtracks: Vec<VodSoundtrackIdentity>,
    },
}

/// Exact video rendition and index used to cut the issued soundtrack plans.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VodSplitVideoIdentity {
    pub rendition_id: String,
    /// None identifies a node-local index. Shared indexes carry their catalog key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VodSoundtrackIdentity {
    pub source_index: i64,
    pub rendition_id: String,
    pub init_id: String,
    pub source_copy: bool,
}

impl VodCopyLayout {
    /// Bounds the soundtrack identities within the 32 KiB durable route budget.
    pub const MAX_SOUNDTRACKS: usize = 128;

    pub fn valid(&self) -> bool {
        let Self::SplitV1 { video, soundtracks } = self else {
            return true;
        };
        let is_sha256 = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        };
        if !is_sha256(&video.rendition_id)
            || !video
                .index_key
                .as_deref()
                .is_none_or(|key| is_sha256(key.strip_prefix("copy-index-v3:").unwrap_or(key)))
            || soundtracks.is_empty()
            || soundtracks.len() > Self::MAX_SOUNDTRACKS
        {
            return false;
        }
        let mut source_indices = std::collections::HashSet::new();
        let mut rendition_ids = std::collections::HashSet::from([video.rendition_id.as_str()]);
        soundtracks.iter().all(|track| {
            (0..=1_024).contains(&track.source_index)
                && source_indices.insert(track.source_index)
                && is_sha256(&track.rendition_id)
                && rendition_ids.insert(track.rendition_id.as_str())
                && is_sha256(&track.init_id)
        })
    }
}
