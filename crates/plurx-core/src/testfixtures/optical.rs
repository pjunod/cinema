//! Deterministic optical-media fixtures shared by core and daemon tests.
//!
//! These fixtures model authored DVD and Blu-ray navigation trees without
//! claiming that their tiny marker payloads are physically playable media.
//! The fake host is the deliberate seam for lifecycle, HTTP, and session
//! contract tests; real demux/seek acceptance remains an opt-in hardware job.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::config::OpticalDriveConfig;
use crate::optical::{
    FingerprintEvidence, HostRequirement, HostRequirementStatus, InspectedChapter, InspectedDisc,
    InspectedStream, InspectedTitle, InspectionResponse, OpticalFormat, OpticalHostAdapter,
    OpticalHostError, OpticalMediaPresence, OpticalTitleLocator, ProtectionFacts, ResolvedInput,
    INSPECTION_SCHEMA_V1,
};
use crate::playback::{PlaybackMediaFacts, SourceDelivery};

#[derive(Debug, Clone)]
pub struct OpticalFolderFixture {
    pub root: PathBuf,
    pub inspection: InspectionResponse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeOpticalEvent {
    Presence(String),
    Inspect {
        drive_id: String,
        expected_generation: String,
    },
    Resolve {
        drive_id: String,
        locator: OpticalTitleLocator,
        angle: u32,
    },
    Eject(String),
}

/// Scriptable path-free host adapter for service and HTTP contract tests.
///
/// Every operation consumes the next scripted result. An exhausted script
/// fails closed instead of silently inventing media state.
#[derive(Debug)]
pub struct FakeOpticalHost {
    requirements: Vec<HostRequirement>,
    presence: Mutex<VecDeque<Result<OpticalMediaPresence, OpticalHostError>>>,
    inspections: Mutex<VecDeque<FakeInspectionStep>>,
    resolutions: Mutex<VecDeque<Result<ResolvedInput, OpticalHostError>>>,
    ejects: Mutex<VecDeque<Result<(), OpticalHostError>>>,
    events: Mutex<Vec<FakeOpticalEvent>>,
}

#[derive(Debug)]
enum FakeInspectionStep {
    Exact(Result<InspectionResponse, OpticalHostError>),
    ForObservedGeneration(OpticalFormat),
}

impl Default for FakeOpticalHost {
    fn default() -> Self {
        Self {
            requirements: vec![HostRequirement {
                id: "fixture_host",
                status: HostRequirementStatus::Met,
                detail: "Deterministic optical fixture host is available".to_owned(),
            }],
            presence: Mutex::new(VecDeque::new()),
            inspections: Mutex::new(VecDeque::new()),
            resolutions: Mutex::new(VecDeque::new()),
            ejects: Mutex::new(VecDeque::new()),
            events: Mutex::new(Vec::new()),
        }
    }
}

impl FakeOpticalHost {
    pub fn push_presence(&self, result: Result<OpticalMediaPresence, OpticalHostError>) {
        self.presence
            .lock()
            .expect("fake presence lock")
            .push_back(result);
    }

    pub fn push_inspection(&self, result: Result<InspectionResponse, OpticalHostError>) {
        self.inspections
            .lock()
            .expect("fake inspection lock")
            .push_back(FakeInspectionStep::Exact(result));
    }

    /// Queue a valid reply whose generation is copied from the service's
    /// request. Exact replies remain available for stale-generation tests.
    pub fn push_inspection_for_observed_generation(&self, format: OpticalFormat) {
        self.inspections
            .lock()
            .expect("fake inspection lock")
            .push_back(FakeInspectionStep::ForObservedGeneration(format));
    }

    pub fn push_resolution(&self, result: Result<ResolvedInput, OpticalHostError>) {
        self.resolutions
            .lock()
            .expect("fake resolution lock")
            .push_back(result);
    }

    pub fn push_eject(&self, result: Result<(), OpticalHostError>) {
        self.ejects
            .lock()
            .expect("fake eject lock")
            .push_back(result);
    }

    pub fn events(&self) -> Vec<FakeOpticalEvent> {
        self.events.lock().expect("fake event lock").clone()
    }

    fn record(&self, event: FakeOpticalEvent) {
        self.events.lock().expect("fake event lock").push(event);
    }
}

#[async_trait]
impl OpticalHostAdapter for FakeOpticalHost {
    fn requirements(&self, _drive: &OpticalDriveConfig) -> Vec<HostRequirement> {
        self.requirements.clone()
    }

    async fn presence(
        &self,
        drive: &OpticalDriveConfig,
    ) -> Result<OpticalMediaPresence, OpticalHostError> {
        self.record(FakeOpticalEvent::Presence(drive.id.clone()));
        self.presence
            .lock()
            .expect("fake presence lock")
            .pop_front()
            .unwrap_or(Err(OpticalHostError::Failed))
    }

    async fn inspect(
        &self,
        drive: &OpticalDriveConfig,
        expected_generation: &str,
    ) -> Result<InspectionResponse, OpticalHostError> {
        self.record(FakeOpticalEvent::Inspect {
            drive_id: drive.id.clone(),
            expected_generation: expected_generation.to_owned(),
        });
        match self
            .inspections
            .lock()
            .expect("fake inspection lock")
            .pop_front()
        {
            Some(FakeInspectionStep::Exact(result)) => result,
            Some(FakeInspectionStep::ForObservedGeneration(OpticalFormat::Dvd)) => {
                Ok(dvd_inspection_fixture(expected_generation))
            }
            Some(FakeInspectionStep::ForObservedGeneration(OpticalFormat::Bluray)) => {
                Ok(bluray_inspection_fixture(expected_generation))
            }
            None => Err(OpticalHostError::Failed),
        }
    }

    fn resolve_input(
        &self,
        drive: &OpticalDriveConfig,
        locator: OpticalTitleLocator,
        angle: u32,
    ) -> Result<ResolvedInput, OpticalHostError> {
        self.record(FakeOpticalEvent::Resolve {
            drive_id: drive.id.clone(),
            locator,
            angle,
        });
        self.resolutions
            .lock()
            .expect("fake resolution lock")
            .pop_front()
            .unwrap_or(Err(OpticalHostError::InvalidMount))
    }

    async fn eject(&self, drive: &OpticalDriveConfig) -> Result<(), OpticalHostError> {
        self.record(FakeOpticalEvent::Eject(drive.id.clone()));
        self.ejects
            .lock()
            .expect("fake eject lock")
            .pop_front()
            .unwrap_or(Err(OpticalHostError::Failed))
    }
}

pub fn optical_drive_fixture(id: &str, mount_path: PathBuf) -> OpticalDriveConfig {
    OpticalDriveConfig {
        id: id.to_owned(),
        label: format!("Fixture {id}"),
        device_path: PathBuf::from(format!("/fixture/dev/{id}")),
        mount_path,
    }
}

/// Publish a tiny deterministic navigation-tree fixture under
/// `target/fixtures/optical/`.
///
/// Marker payloads make identity, swap, restart, and route tests reproducible.
/// They are intentionally not accepted as demux/seek evidence; tests that open
/// a title must use the fake adapter or the opt-in physical-media runner.
pub fn optical_folder_fixture(format: OpticalFormat, generation: &str) -> OpticalFolderFixture {
    let (name, files): (&str, &[(&str, &[u8])]) = match format {
        OpticalFormat::Dvd => (
            "dvd-v1",
            &[
                ("VIDEO_TS/VIDEO_TS.IFO", b"PLURX-DVD-VMG-v1\n"),
                ("VIDEO_TS/VTS_01_0.IFO", b"PLURX-DVD-VTS-v1\n"),
                ("VIDEO_TS/VTS_01_1.VOB", b"PLURX-DVD-TITLE-v1\n"),
            ],
        ),
        OpticalFormat::Bluray => (
            "bluray-v1",
            &[
                ("BDMV/index.bdmv", b"PLURX-BDMV-INDEX-v1\n"),
                ("BDMV/MovieObject.bdmv", b"PLURX-BDMV-MOVIE-v1\n"),
                ("BDMV/PLAYLIST/00001.mpls", b"PLURX-BDMV-PLAYLIST-v1\n"),
                ("BDMV/STREAM/00001.m2ts", b"PLURX-BDMV-STREAM-v1\n"),
            ],
        ),
    };
    let root = super::fixture_dir().join("optical").join(name);
    for (relative, bytes) in files {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("fixture file parent"))
            .expect("creating optical fixture tree");
        let stem = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("fixture file name");
        super::publish_fixture_if_absent(&path, stem, |temporary| {
            std::fs::write(temporary, bytes).expect("writing optical fixture");
        });
    }
    let inspection = match format {
        OpticalFormat::Dvd => dvd_inspection_fixture(generation),
        OpticalFormat::Bluray => bluray_inspection_fixture(generation),
    };
    OpticalFolderFixture { root, inspection }
}

pub fn dvd_inspection_fixture(generation: &str) -> InspectionResponse {
    inspection_fixture(
        generation,
        OpticalFormat::Dvd,
        OpticalTitleLocator::Dvd { title_number: 1 },
        "mpeg",
        "mpeg2video",
        "11",
    )
}

pub fn bluray_inspection_fixture(generation: &str) -> InspectionResponse {
    inspection_fixture(
        generation,
        OpticalFormat::Bluray,
        OpticalTitleLocator::Bluray { playlist_number: 1 },
        "mpegts",
        "h264",
        "22",
    )
}

fn inspection_fixture(
    generation: &str,
    format: OpticalFormat,
    locator: OpticalTitleLocator,
    container: &str,
    video_codec: &str,
    digest_byte: &str,
) -> InspectionResponse {
    let duration_ms = 120_000;
    let facts = PlaybackMediaFacts {
        duration_ms: Some(duration_ms),
        container: Some(container.to_owned()),
        video_codec: Some(video_codec.to_owned()),
        width: Some(720),
        height: Some(if format == OpticalFormat::Dvd {
            480
        } else {
            1080
        }),
        probed: true,
        source_delivery: SourceDelivery::ManagedOpticalTitle,
        ..PlaybackMediaFacts::default()
    };
    let probe_json = serde_json::to_string(&serde_json::json!({
        "format": {"duration": "120.000000", "format_name": container},
        "streams": [{
            "index": 0,
            "codec_type": "video",
            "codec_name": video_codec,
            "width": facts.width,
            "height": facts.height
        }],
        "chapters": [
            {"id": 0, "start_time": "0.000000"},
            {"id": 1, "start_time": "60.000000"}
        ]
    }))
    .expect("fixture probe JSON");
    InspectionResponse {
        schema_version: INSPECTION_SCHEMA_V1,
        expected_generation: generation.to_owned(),
        disc: InspectedDisc {
            format,
            volume_label: Some("PLURX_FIXTURE".to_owned()),
            protection: ProtectionFacts {
                detected: Some(false),
                implementation_available: Some(false),
                handled: Some(true),
                scheme: None,
            },
            fingerprint: FingerprintEvidence {
                version: 1,
                complete: true,
                digest: digest_byte.repeat(32),
                navigation_bytes: 4096,
                bounded_sample_bytes: 65_536,
            },
            titles: vec![InspectedTitle {
                title_id: "title-1".to_owned(),
                locator,
                duration_ms: Some(duration_ms as u64),
                angles: 1,
                facts,
                probe_json,
                streams: vec![InspectedStream {
                    index: 0,
                    kind: "video".to_owned(),
                    codec: Some(video_codec.to_owned()),
                    language: None,
                    title: None,
                    channels: None,
                    default: true,
                    forced: false,
                }],
                chapters: vec![
                    InspectedChapter {
                        index: 0,
                        start_ms: Some(0),
                        accurate: true,
                    },
                    InspectedChapter {
                        index: 1,
                        start_ms: Some(60_000),
                        accurate: true,
                    },
                ],
                suggested_feature_score: Some(100),
                suggestion_reasons: vec!["longest authored title".to_owned()],
            }],
        },
        diagnostics: "authored logical fixture".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::optical::validate_inspection;

    #[test]
    fn optical_fixture_replies_are_valid_and_distinct() {
        let dvd = dvd_inspection_fixture("dvd-generation");
        let bluray = bluray_inspection_fixture("bluray-generation");
        validate_inspection(&dvd).expect("DVD fixture");
        validate_inspection(&bluray).expect("Blu-ray fixture");
        assert_ne!(dvd.disc.fingerprint.digest, bluray.disc.fingerprint.digest);
        assert_ne!(dvd.disc.format, bluray.disc.format);
    }

    #[test]
    fn optical_folder_fixtures_publish_expected_navigation_shapes() {
        let dvd = optical_folder_fixture(OpticalFormat::Dvd, "dvd-generation");
        let bluray = optical_folder_fixture(OpticalFormat::Bluray, "bluray-generation");
        assert!(dvd.root.join("VIDEO_TS/VTS_01_0.IFO").is_file());
        assert!(bluray.root.join("BDMV/PLAYLIST/00001.mpls").is_file());
        assert_eq!(dvd.inspection.disc.format, OpticalFormat::Dvd);
        assert_eq!(bluray.inspection.disc.format, OpticalFormat::Bluray);
    }
}
