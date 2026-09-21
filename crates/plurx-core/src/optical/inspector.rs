use serde::{Deserialize, Serialize};

use super::{OpticalFormat, OpticalTitleLocator, MAX_OPTICAL_ID_BYTES};

pub const INSPECTION_SCHEMA_V1: u32 = 1;
const MAX_TITLES: usize = 512;
const MAX_STREAMS_PER_TITLE: usize = 256;
const MAX_CHAPTERS_PER_TITLE: usize = 4096;
const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtectionFacts {
    pub detected: Option<bool>,
    pub implementation_available: Option<bool>,
    pub handled: Option<bool>,
    pub scheme: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FingerprintEvidence {
    pub version: u32,
    pub complete: bool,
    pub digest: String,
    pub navigation_bytes: u64,
    pub bounded_sample_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectedStream {
    pub index: i64,
    pub kind: String,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub channels: Option<u32>,
    pub default: bool,
    pub forced: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectedChapter {
    pub index: u32,
    pub start_ms: Option<u64>,
    pub accurate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectedTitle {
    pub title_id: String,
    pub locator: OpticalTitleLocator,
    pub duration_ms: Option<u64>,
    pub angles: u32,
    pub streams: Vec<InspectedStream>,
    pub chapters: Vec<InspectedChapter>,
    pub suggested_feature_score: Option<u16>,
    pub suggestion_reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectedDisc {
    pub format: OpticalFormat,
    pub volume_label: Option<String>,
    pub protection: ProtectionFacts,
    pub fingerprint: FingerprintEvidence,
    pub titles: Vec<InspectedTitle>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectionResponse {
    pub schema_version: u32,
    pub expected_generation: String,
    pub disc: InspectedDisc,
    pub diagnostics: String,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum InspectionError {
    #[error("unsupported inspection schema")]
    Schema,
    #[error("inspection generation is empty or too long")]
    Generation,
    #[error("inspection contains too many titles")]
    Titles,
    #[error("inspection title id is empty or too long")]
    TitleId,
    #[error("inspection title has no angles")]
    Angles,
    #[error("inspection title contains too many streams")]
    Streams,
    #[error("inspection title contains too many chapters")]
    Chapters,
    #[error("inspection diagnostics exceed the bounded limit")]
    Diagnostics,
}

pub fn validate_inspection(response: &InspectionResponse) -> Result<(), InspectionError> {
    if response.schema_version != INSPECTION_SCHEMA_V1 {
        return Err(InspectionError::Schema);
    }
    if response.expected_generation.is_empty()
        || response.expected_generation.len() > MAX_OPTICAL_ID_BYTES
    {
        return Err(InspectionError::Generation);
    }
    if response.disc.titles.len() > MAX_TITLES {
        return Err(InspectionError::Titles);
    }
    if response.diagnostics.len() > MAX_DIAGNOSTIC_BYTES {
        return Err(InspectionError::Diagnostics);
    }
    for title in &response.disc.titles {
        if title.title_id.is_empty() || title.title_id.len() > MAX_OPTICAL_ID_BYTES {
            return Err(InspectionError::TitleId);
        }
        if title.angles == 0 {
            return Err(InspectionError::Angles);
        }
        if title.streams.len() > MAX_STREAMS_PER_TITLE {
            return Err(InspectionError::Streams);
        }
        if title.chapters.len() > MAX_CHAPTERS_PER_TITLE {
            return Err(InspectionError::Chapters);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response() -> InspectionResponse {
        InspectionResponse {
            schema_version: INSPECTION_SCHEMA_V1,
            expected_generation: "generation-a".into(),
            disc: InspectedDisc {
                format: OpticalFormat::Dvd,
                volume_label: Some("Fixture".into()),
                protection: ProtectionFacts {
                    detected: Some(false),
                    implementation_available: Some(false),
                    handled: Some(true),
                    scheme: None,
                },
                fingerprint: FingerprintEvidence {
                    version: 1,
                    complete: true,
                    digest: "00".repeat(32),
                    navigation_bytes: 12,
                    bounded_sample_bytes: 4096,
                },
                titles: vec![InspectedTitle {
                    title_id: "title-a".into(),
                    locator: OpticalTitleLocator::Dvd { title_number: 1 },
                    duration_ms: Some(60_000),
                    angles: 1,
                    streams: Vec::new(),
                    chapters: Vec::new(),
                    suggested_feature_score: None,
                    suggestion_reasons: Vec::new(),
                }],
            },
            diagnostics: String::new(),
        }
    }

    #[test]
    fn optical_helper_reply_is_versioned_and_bounded() {
        validate_inspection(&response()).expect("valid response");
        let mut oversized = response();
        oversized.diagnostics = "x".repeat(MAX_DIAGNOSTIC_BYTES + 1);
        assert_eq!(
            validate_inspection(&oversized),
            Err(InspectionError::Diagnostics)
        );
    }
}
