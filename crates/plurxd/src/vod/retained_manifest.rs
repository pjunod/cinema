//! Private bounded completed-artifact hints. Decoding never grants authority.
use super::output_measurement::{CompleteOutputObservation, ObservedOutputMember, OutputOrigin};
use super::*;
use plurx_core::output_measurement::{FullOutputMeasurement, WireDuration};
use std::io::{Read, Write};

pub(super) const MANIFEST_NAME: &str = "complete-v1.json";
pub(super) const MAX_MANIFEST: u64 = 4 * 1024 * 1024;

/// Logical delivery facts are separate from the original salted execution.
/// No process digest is stripped and no durable idempotency key is reused.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LogicalOutput {
    version: u32,
    file_id: i64,
    kind: SessionKind,
    audio_index: Option<i64>,
    audio_offset_ms: i64,
    subtitle_burn: Option<i64>,
    hdr10: bool,
    copy_video_args: Vec<String>,
    audio_claim: Option<plurx_core::playback::audio::AudioClaim>,
    audio_delivery: Option<plurx_core::playback::audio::AudioDelivery>,
    candidate: Option<LogicalCandidate>,
    owner_node_id: Option<String>,
    production: Option<LogicalProduction>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LogicalCandidate {
    id: plurx_core::playback::candidate::CandidateId,
    digest: [u8; 32],
    geometry: bool,
    grade: plurx_core::transcode::OutputGrade,
    profile: Option<LogicalProfile>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum LogicalProfile {
    H264Sdr1440P30V1,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LogicalProduction {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dv_processing: Option<String>,
    plan: String,
    executable: String,
    build: String,
    subtitle: Option<String>,
    args: Vec<String>,
}

impl LogicalOutput {
    pub(super) fn resolve(
        request: &SessionRequest,
        encoding: Option<&crate::vodencode::Encoding>,
        file: &MediaFile,
        video: CopyVideoOptions,
    ) -> Self {
        Self {
            version: 1,
            file_id: request.file_id,
            kind: request.kind,
            audio_index: request.audio_index,
            audio_offset_ms: request.audio_offset_ms,
            subtitle_burn: request.subtitle_burn,
            hdr10: request.hdr10,
            copy_video_args: if encoding.is_none() {
                plurx_core::transcode::copy_video_args(file, video)
            } else {
                Vec::new()
            },
            audio_claim: request.audio_claim.clone(),
            audio_delivery: encoding
                .and_then(|e| e.options.audio.clone())
                .or_else(|| request.audio_delivery.clone()),
            candidate: request
                .candidate_context
                .as_ref()
                .map(|c| LogicalCandidate {
                    id: c.candidate_id,
                    digest: c.recipe_digest,
                    geometry: c.normalized_geometry,
                    grade: c.grade,
                    profile: c.profile.map(|profile| match profile {
                        plurx_core::transcode::AutoQualityRateProfile::H264Sdr1440P30V1 => {
                            LogicalProfile::H264Sdr1440P30V1
                        }
                    }),
                }),
            owner_node_id: request
                .candidate_context
                .as_ref()
                .and_then(|c| c.owner_node_id.clone()),
            production: encoding.map(|e| LogicalProduction {
                dv_processing: match &e.dv_processing {
                    plurx_core::transcode::dv_processing::DvSelection::Selected(plan) => {
                        Some(plan.semantic_digest().as_str().to_owned())
                    }
                    plurx_core::transcode::dv_processing::DvSelection::KeepExisting(_) => None,
                },
                plan: e.plan.plan_digest().to_owned(),
                executable: e.executable.digest.clone(),
                build: e.ffmpeg_build.clone(),
                subtitle: e.subtitle_digest.clone(),
                args: {
                    let grid = e.grid.plan(file.duration_ms.unwrap_or(0), 0);
                    let end = grid
                        .entries
                        .last()
                        .map_or(0, |entry| entry.start_ticks + entry.duration_ticks);
                    e.args(file, 0.0, end as f64 / f64::from(grid.timescale))
                },
            }),
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ArtifactManifest {
    version: u32,
    pub(super) id: String,
    pub(super) origin: OutputOrigin,
    pub(super) logical: LogicalOutput,
    init_bytes: u64,
    members: Vec<ObservedOutputMember>,
}

impl ArtifactManifest {
    /// Canonical typed metadata seals every actual member digest/length and
    /// logical fact. The execution origin alone does not bind those bytes.
    pub(super) fn seal(&self) -> io::Result<[u8; 32]> {
        self.observation()?;
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_MANIFEST {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(Sha256::digest(bytes).into())
    }
    pub(super) fn from_artifact(
        artifact: &super::retained::RetainedVodArtifact,
        logical: LogicalOutput,
    ) -> Self {
        Self {
            version: 1,
            id: artifact.id.to_string(),
            origin: artifact.observation.preimage.clone(),
            logical,
            init_bytes: artifact.init_bytes,
            members: artifact.observation.members.clone(),
        }
    }

    pub(super) fn observation(&self) -> io::Result<CompleteOutputObservation> {
        if self.version != 1
            || self.logical.version != 1
            || uuid::Uuid::parse_str(&self.id).is_err()
            || self.init_bytes == 0
            || self.members.is_empty()
            || self.members.len() > 8192
            || self.origin.playlist.len() > MAX_MANIFEST as usize
            || self.origin.source_version.is_empty()
            || self.origin.recipe_key.len() != 64
            || !valid_digest(&self.origin.recipe_key)
            || !valid_digest(&self.origin.served_init)
            || !valid_digest(&self.origin.muxer_init)
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let playlist =
            std::str::from_utf8(&self.origin.playlist).map_err(|_| io::ErrorKind::InvalidData)?;
        if !playlist.ends_with("#EXT-X-ENDLIST\n") {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let target: Vec<_> = playlist
            .lines()
            .filter_map(|l| l.strip_prefix("#EXT-X-TARGETDURATION:"))
            .collect();
        let target = match target.as_slice() {
            [value] => value.parse::<u64>().ok(),
            _ => None,
        }
        .ok_or(io::ErrorKind::InvalidData)?;
        let durations: Option<Vec<_>> = playlist
            .lines()
            .filter_map(|l| l.strip_prefix("#EXTINF:"))
            .map(|d| WireDuration::parse(d.strip_suffix(',')?))
            .collect();
        let durations = durations
            .filter(|d| d.len() == self.members.len())
            .ok_or(io::ErrorKind::InvalidData)?;
        let names: Vec<_> = playlist
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        if names.len() != self.members.len()
            || names
                .iter()
                .enumerate()
                .any(|(i, n)| *n != segment_name(i as u64))
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let origin = self.origin.identity();
        let mut reducer = FullOutputMeasurement::new(origin, target, &durations);
        for (i, member) in self.members.iter().enumerate() {
            reducer.observe(origin, i, member.bytes);
        }
        let rates = reducer
            .rates(true)
            .map_err(|_| io::ErrorKind::InvalidData)?;
        Ok(CompleteOutputObservation {
            preimage: self.origin.clone(),
            rates,
            epoch: self.origin.epoch,
            served_init: self.origin.served_init.clone(),
            members: self.members.clone(),
        })
    }

    pub(super) fn charge(&self) -> io::Result<u64> {
        self.observation()?
            .rates
            .wire_bytes
            .checked_add(self.init_bytes)
            .and_then(|bytes| bytes.checked_add(MAX_MANIFEST))
            .ok_or_else(|| io::ErrorKind::InvalidData.into())
    }

    pub(super) fn matches_request(
        &self,
        rendition: &Rendition,
        incoming_logical: &Option<LogicalOutput>,
    ) -> bool {
        incoming_logical.as_ref() == Some(&self.logical)
            && rendition
                .source
                .as_ref()
                .is_some_and(|s| s.unchanged() && s.object_version() == self.origin.source_version)
            && rendition.playlist.as_slice() == self.origin.playlist.as_slice()
    }
    #[cfg(test)]
    pub(super) fn matches(&self, rendition: &Rendition) -> bool {
        self.matches_request(rendition, &rendition.recipe.retained_logical)
    }

    pub(super) fn read(directory: &Path) -> io::Result<Self> {
        let metadata = std::fs::symlink_metadata(directory)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let path = directory.join(MANIFEST_NAME);
        let mut file = open_regular(&path)?;
        let len = file.metadata()?.len();
        if len == 0 || len > MAX_MANIFEST {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut bytes = Vec::with_capacity(len as usize);
        (&mut file).take(MAX_MANIFEST + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != len {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let value: Self = serde_json::from_slice(&bytes).map_err(|_| io::ErrorKind::InvalidData)?;
        if directory.file_name().and_then(|n| n.to_str()) != Some(value.id.to_string().as_str()) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        value.observation()?;
        Ok(value)
    }

    pub(super) fn commit(&self, directory: &Path) -> io::Result<()> {
        self.observation()?;
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_MANIFEST {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let temporary = directory.join(".complete.tmp");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(temporary, directory.join(MANIFEST_NAME))?;
        #[cfg(unix)]
        std::fs::File::open(directory)?.sync_all()?;
        Ok(())
    }

    pub(super) fn validate_files(&self, directory: &Path, deadline: Instant) -> io::Result<()> {
        self.observation()?;
        verify_file(
            &directory.join(INIT_NAME),
            self.init_bytes,
            &self.origin.served_init,
            deadline,
        )?;
        for (index, member) in self.members.iter().enumerate() {
            verify_file(
                &directory.join(segment_name(index as u64)),
                member.bytes,
                &hex::encode(member.digest),
                deadline,
            )?;
        }
        Ok(())
    }

    pub(super) fn into_artifact(
        self,
        directory: PathBuf,
    ) -> io::Result<super::retained::RetainedVodArtifact> {
        let observation = self.observation()?;
        let charge = self.charge()?;
        let seal = self.seal()?;
        Ok(super::retained::RetainedVodArtifact {
            private_preparation_origin: std::sync::OnceLock::new(),
            id: uuid::Uuid::parse_str(&self.id).map_err(|_| io::ErrorKind::InvalidData)?,
            observation,
            directory,
            charge,
            init_bytes: self.init_bytes,
            repair: Mutex::new(()),
            recipe_key: self.origin.recipe_key,
            source_version: self.origin.source_version,
            candidate: None,
            audio_delivery: self.logical.audio_delivery.clone(),
            durable: true,
            logical: Some(self.logical),
            validated: AtomicBool::new(false),
            sealed_identity: std::sync::OnceLock::from(seal),
        })
    }
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn open_regular(path: &Path) -> io::Result<std::fs::File> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(file)
}

fn verify_file(path: &Path, expected: u64, digest: &str, deadline: Instant) -> io::Result<()> {
    let mut file = open_regular(path)?;
    let before = file.metadata()?;
    if before.len() != expected {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 65_536];
    let mut bytes = 0_u64;
    loop {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .ok_or(io::ErrorKind::InvalidData)?;
        if bytes > expected {
            return Err(io::ErrorKind::InvalidData.into());
        }
        hash.update(&buffer[..read]);
    }
    let after = file.metadata()?;
    if bytes != expected
        || hex::encode(hash.finalize()) != digest
        || before.len() != after.len()
        || before.modified()? != after.modified()?
    {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(())
}
