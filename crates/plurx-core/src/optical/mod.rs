//! Physical optical-media domain contracts.
//!
//! Identity and title selection are intentionally separate from filesystem
//! paths. A client names an inspected opaque title; only the drive owner lowers
//! that title to a local device or mount after checking the insertion
//! generation. No type in this module grants permission to open a path.

mod capability;
mod input;
mod inspector;

pub use capability::{classify_help_output, OpticalCapabilities, OpticalCapability};
pub use input::{InputBuildError, OpticalTitleLocator, ResolvedInput};
pub use inspector::{
    validate_inspection, FingerprintEvidence, InspectedChapter, InspectedDisc, InspectedStream,
    InspectedTitle, InspectionError, InspectionResponse, ProtectionFacts, INSPECTION_SCHEMA_V1,
};

use serde::{Deserialize, Serialize};

/// Opaque identifiers are bounded before they can reach a cache key, log, or
/// replicated payload. The exact value is not interpreted outside the drive
/// owner that minted it.
pub const MAX_OPTICAL_ID_BYTES: usize = 192;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpticalFormat {
    Dvd,
    Bluray,
}

/// The durable or request identity of a playback source.
///
/// The file variant preserves the existing public contract. Optical identity
/// carries no device path and cannot be lowered away from its owner node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlaybackSourceRef {
    File {
        file_id: i64,
    },
    Optical {
        owner_node_id: String,
        drive_id: String,
        media_generation: String,
        disc_id: String,
        title_id: String,
        angle: u32,
    },
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum SourceRefError {
    #[error("file id must be positive")]
    FileId,
    #[error("{0} is empty")]
    Empty(&'static str),
    #[error("{0} exceeds {MAX_OPTICAL_ID_BYTES} bytes")]
    TooLong(&'static str),
    #[error("angle must be at least one")]
    Angle,
}

impl PlaybackSourceRef {
    pub fn validate(&self) -> Result<(), SourceRefError> {
        match self {
            Self::File { file_id } => (*file_id > 0).then_some(()).ok_or(SourceRefError::FileId),
            Self::Optical {
                owner_node_id,
                drive_id,
                media_generation,
                disc_id,
                title_id,
                angle,
            } => {
                for (name, value) in [
                    ("owner node id", owner_node_id),
                    ("drive id", drive_id),
                    ("media generation", media_generation),
                    ("disc id", disc_id),
                    ("title id", title_id),
                ] {
                    validate_id(name, value)?;
                }
                if *angle == 0 {
                    return Err(SourceRefError::Angle);
                }
                Ok(())
            }
        }
    }
}

fn validate_id(name: &'static str, value: &str) -> Result<(), SourceRefError> {
    if value.is_empty() {
        return Err(SourceRefError::Empty(name));
    }
    if value.len() > MAX_OPTICAL_ID_BYTES {
        return Err(SourceRefError::TooLong(name));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optical_source_identity_has_no_path_and_rejects_unbounded_fields() {
        let source = PlaybackSourceRef::Optical {
            owner_node_id: "node-a".into(),
            drive_id: "media-room".into(),
            media_generation: "generation-a".into(),
            disc_id: "optical-v1:disc".into(),
            title_id: "opaque-title".into(),
            angle: 1,
        };
        source.validate().expect("valid source");
        let encoded = serde_json::to_value(&source).expect("serialize");
        assert!(encoded.get("path").is_none());

        let mut invalid = source;
        let PlaybackSourceRef::Optical { title_id, .. } = &mut invalid else {
            unreachable!()
        };
        *title_id = "x".repeat(MAX_OPTICAL_ID_BYTES + 1);
        assert_eq!(invalid.validate(), Err(SourceRefError::TooLong("title id")));
    }

    #[test]
    fn optical_file_identity_refuses_sentinel_ids() {
        assert_eq!(
            PlaybackSourceRef::File { file_id: 0 }.validate(),
            Err(SourceRefError::FileId)
        );
        assert_eq!(
            PlaybackSourceRef::File { file_id: -1 }.validate(),
            Err(SourceRefError::FileId)
        );
    }
}
