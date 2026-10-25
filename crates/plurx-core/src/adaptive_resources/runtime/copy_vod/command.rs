//! Immutable, source-derived command for one fixed ordinary rolling copy.
//! It carries no native File, launch authority, or physical-close proof.
use crate::adaptive_resources::runtime::finite_vod::SoftwareAvcExecutable;
use crate::domain::MediaFile;
use crate::playback::audio::AudioDelivery;
use crate::transcode::{self, CopyVideoOptions, Pacing};
use std::io;
use std::mem::size_of;
use std::path::{Path, PathBuf};

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, reason)
}

/// Exact one-input copy argv; the graph still has to acquire and verify the
/// selected media and executable SourceV1 members before native root entry.
pub struct CopyPipeCommand {
    source: Option<MediaFile>,
    video: CopyVideoOptions,
    executable: SoftwareAvcExecutable,
    args: Vec<String>,
    runtime_cache: PathBuf,
}

impl CopyPipeCommand {
    pub fn compile(
        source: MediaFile,
        video: CopyVideoOptions,
        executable: SoftwareAvcExecutable,
        runtime_cache: PathBuf,
        audio_index: Option<i64>,
        transcode_audio: bool,
        pacing: Pacing,
        audio_delivery: Option<&AudioDelivery>,
    ) -> io::Result<Self> {
        if !source.path.is_absolute()
            || source.path.as_os_str().len() > 4096
            || source.path.as_os_str().as_encoded_bytes().contains(&0)
            || !runtime_cache.is_absolute()
            || runtime_cache.as_os_str().len() > 4096
            || runtime_cache.as_os_str().as_encoded_bytes().contains(&0)
            || source.audio_offset_ms != 0
        {
            return Err(invalid(
                "rolling copy requires bounded one-input paths and no second demuxer",
            ));
        }
        // A second input would need a separately acquired SourceV1 cell and
        // independent inherited file description. This first fixed profile
        // deliberately admits the ordinary one-input audio/video copy only.
        let mut input = source.clone();
        input.path = PathBuf::from("/proc/self/fd/3");
        let args = transcode::copy_pipe_args_with_audio_delivery(
            &input,
            0.0,
            audio_index,
            transcode_audio,
            pacing,
            video,
            audio_delivery,
        );
        if args.len() > 128
            || args
                .iter()
                .any(|arg| arg.len() > 4096 || arg.contains('\0'))
            || args.last().map(String::as_str) != Some("pipe:1")
            || args.windows(2).filter(|pair| pair[0] == "-i").count() != 1
        {
            return Err(invalid(
                "rolling copy argv exceeds the fixed one-input graph",
            ));
        }
        let command = Self {
            source: Some(source),
            video,
            executable,
            args,
            runtime_cache,
        };
        command.resident_memory_bytes()?;
        Ok(command)
    }

    pub(in crate::adaptive_resources::runtime) fn source(&self) -> &MediaFile {
        self.source
            .as_ref()
            .expect("copy parser source not yet moved")
    }
    pub(in crate::adaptive_resources::runtime) fn video(&self) -> CopyVideoOptions {
        self.video
    }
    pub(in crate::adaptive_resources::runtime) fn executable(&self) -> &SoftwareAvcExecutable {
        &self.executable
    }
    pub(in crate::adaptive_resources::runtime) fn program(&self) -> &Path {
        self.executable.path()
    }
    pub(in crate::adaptive_resources::runtime) fn args(&self) -> &[String] {
        &self.args
    }
    pub(in crate::adaptive_resources::runtime) fn runtime_cache(&self) -> &Path {
        &self.runtime_cache
    }
    pub(in crate::adaptive_resources::runtime) fn resident_memory_bytes(
        &self,
    ) -> io::Result<usize> {
        let mut bytes = size_of::<Self>()
            .checked_add(
                self.source
                    .as_ref()
                    .and_then(MediaFile::requested_heap_bytes)
                    .ok_or_else(|| invalid("source heap overflow"))?,
            )
            .and_then(|n| n.checked_add(self.executable.resident_memory_bytes().ok()?))
            .and_then(|n| n.checked_add(self.runtime_cache.capacity()))
            .and_then(|n| n.checked_add(self.args.capacity().checked_mul(size_of::<String>())?))
            .ok_or_else(|| invalid("copy command memory overflow"))?;
        for arg in &self.args {
            bytes = bytes
                .checked_add(arg.capacity())
                .ok_or_else(|| invalid("copy argv memory overflow"))?;
        }
        Ok(bytes)
    }
}
