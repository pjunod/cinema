use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{OpticalTitleLocator, PlaybackSourceRef, SourceRefError, MAX_OPTICAL_ID_BYTES};

pub const OPTICAL_SESSION_PAYLOAD_V1: u16 = 1;
const MAX_OPTICAL_SESSION_PAYLOAD_BYTES: usize = 16 * 1024;

/// Source extension stored beside, rather than inside, legacy file recipes.
///
/// Existing `SessionRequest` JSON is a `deny_unknown_fields` rolling-upgrade
/// contract. Optical sessions therefore use their own explicitly versioned
/// payload and old file recipes remain byte-for-byte unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableOpticalSessionSource {
    pub version: u16,
    pub source: PlaybackSourceRef,
    pub locator: OpticalTitleLocator,
    pub output_identity: String,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum OpticalSessionPayloadError {
    #[error(
        "optical session payload is empty or exceeds {MAX_OPTICAL_SESSION_PAYLOAD_BYTES} bytes"
    )]
    Size,
    #[error("unsupported optical session payload version {0}")]
    Version(u16),
    #[error("optical session payload must name an optical source")]
    FileSource,
    #[error("invalid optical source: {0}")]
    Source(#[from] SourceRefError),
    #[error("optical title locator selection must be at least one")]
    Locator,
    #[error("invalid optical output identity")]
    OutputIdentity,
    #[error("invalid optical session JSON: {0}")]
    Json(String),
}

impl DurableOpticalSessionSource {
    pub fn validate(&self) -> Result<(), OpticalSessionPayloadError> {
        if self.version != OPTICAL_SESSION_PAYLOAD_V1 {
            return Err(OpticalSessionPayloadError::Version(self.version));
        }
        self.source.validate()?;
        if !matches!(self.source, PlaybackSourceRef::Optical { .. }) {
            return Err(OpticalSessionPayloadError::FileSource);
        }
        let selection = match self.locator {
            OpticalTitleLocator::Dvd { title_number } => title_number,
            OpticalTitleLocator::Bluray { playlist_number } => playlist_number,
        };
        if selection == 0 {
            return Err(OpticalSessionPayloadError::Locator);
        }
        let Some(digest) = self.output_identity.strip_prefix("optical-output-v1:") else {
            return Err(OpticalSessionPayloadError::OutputIdentity);
        };
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(OpticalSessionPayloadError::OutputIdentity);
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<String, OpticalSessionPayloadError> {
        self.validate()?;
        let encoded = serde_json::to_string(self)
            .map_err(|error| OpticalSessionPayloadError::Json(error.to_string()))?;
        if encoded.is_empty() || encoded.len() > MAX_OPTICAL_SESSION_PAYLOAD_BYTES {
            return Err(OpticalSessionPayloadError::Size);
        }
        Ok(encoded)
    }

    pub fn decode(encoded: &str) -> Result<Self, OpticalSessionPayloadError> {
        if encoded.is_empty() || encoded.len() > MAX_OPTICAL_SESSION_PAYLOAD_BYTES {
            return Err(OpticalSessionPayloadError::Size);
        }
        let payload = serde_json::from_str::<Self>(encoded)
            .map_err(|error| OpticalSessionPayloadError::Json(error.to_string()))?;
        payload.validate()?;
        Ok(payload)
    }
}

#[derive(Serialize)]
struct OutputIdentityInput<'a> {
    version: u16,
    source: &'a PlaybackSourceRef,
    locator: OpticalTitleLocator,
    audio_selection: Option<&'a str>,
    subtitle_selection: Option<&'a str>,
    plan_digest: &'a str,
}

/// Derive the optical-only output/cache namespace for one executable plan.
///
/// Media generation is part of `source`, so identical discs inserted twice do
/// not share segment or index bytes in v1. Title, angle, track selection and
/// the existing plan digest all participate without inventing a file id.
pub fn optical_output_identity(
    source: &PlaybackSourceRef,
    locator: OpticalTitleLocator,
    audio_selection: Option<&str>,
    subtitle_selection: Option<&str>,
    plan_digest: &str,
) -> Result<String, OpticalSessionPayloadError> {
    source.validate()?;
    if !matches!(source, PlaybackSourceRef::Optical { .. }) {
        return Err(OpticalSessionPayloadError::FileSource);
    }
    for value in [audio_selection, subtitle_selection, Some(plan_digest)]
        .into_iter()
        .flatten()
    {
        if value.is_empty() || value.len() > MAX_OPTICAL_ID_BYTES {
            return Err(OpticalSessionPayloadError::OutputIdentity);
        }
    }
    let input = OutputIdentityInput {
        version: OPTICAL_SESSION_PAYLOAD_V1,
        source,
        locator,
        audio_selection,
        subtitle_selection,
        plan_digest,
    };
    let canonical = serde_json::to_vec(&input)
        .map_err(|error| OpticalSessionPayloadError::Json(error.to_string()))?;
    Ok(format!(
        "optical-output-v1:{}",
        hex::encode(Sha256::digest(canonical))
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(generation: &str) -> PlaybackSourceRef {
        PlaybackSourceRef::Optical {
            owner_node_id: "node-a".to_owned(),
            drive_id: "drive-a".to_owned(),
            media_generation: generation.to_owned(),
            disc_id: "disc-a".to_owned(),
            title_id: "title-a".to_owned(),
            angle: 1,
        }
    }

    #[test]
    fn optical_session_payload_round_trips_without_a_path_or_file_id() {
        let source = source("generation-a");
        let locator = OpticalTitleLocator::Dvd { title_number: 2 };
        let payload = DurableOpticalSessionSource {
            version: OPTICAL_SESSION_PAYLOAD_V1,
            output_identity: optical_output_identity(
                &source,
                locator,
                Some("audio-2"),
                Some("subtitle-1"),
                "plan-a",
            )
            .expect("identity"),
            source,
            locator,
        };
        let encoded = payload.encode().expect("encode");
        assert!(!encoded.contains("path"));
        assert!(!encoded.contains("file_id"));
        assert_eq!(
            DurableOpticalSessionSource::decode(&encoded).expect("decode"),
            payload
        );
    }

    #[test]
    fn optical_output_identity_fences_insertions_and_selection() {
        let locator = OpticalTitleLocator::Bluray {
            playlist_number: 800,
        };
        let first = optical_output_identity(&source("generation-a"), locator, None, None, "plan")
            .expect("first");
        let swapped = optical_output_identity(&source("generation-b"), locator, None, None, "plan")
            .expect("swapped");
        let selected = optical_output_identity(
            &source("generation-a"),
            locator,
            Some("commentary"),
            None,
            "plan",
        )
        .expect("selected");
        assert_ne!(first, swapped);
        assert_ne!(first, selected);
    }
}
