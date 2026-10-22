//! A compiled ordinary software-AVC command. The recipe is built from the
//! validated core plan; callers cannot replace its arguments after compilation.

use std::{
    ffi::{OsStr, OsString},
    io,
    mem::size_of,
    path::PathBuf,
};

use crate::{
    domain::MediaFile,
    transcode::{
        vod_pipe_args_with_source_clock, Encoder, FrameDomain, ResolvedTranscode,
        TranscodeExecution, VodFrameGrid, VodSourceClock,
    },
};

const ITEM_LIMIT: usize = 4_096;
const COLLECTION_LIMIT: usize = 64 * 1_024;
const ENV_ENTRY_LIMIT: usize = 32;

/// Immutable argv and child configuration for the fixed ordinary AVC pump.
/// This owns only planning data; it holds no source descriptor or child proof.
pub struct SoftwareAvcCommand {
    program: PathBuf,
    source_path: PathBuf,
    source_id: i64,
    scanner_size: i64,
    scanner_mtime: i64,
    grid: VodFrameGrid,
    start_seconds: f64,
    args: Vec<String>,
    env: Vec<(OsString, OsString)>,
    runtime_cache: PathBuf,
    source_count: usize,
}

impl SoftwareAvcCommand {
    #[allow(clippy::too_many_arguments)]
    pub fn compile(
        source: &MediaFile,
        plan: &ResolvedTranscode,
        execution: &TranscodeExecution,
        grid: VodFrameGrid,
        duration_seconds: f64,
        reorder: bool,
        source_clock: VodSourceClock,
        program: PathBuf,
        env: Vec<(OsString, OsString)>,
        runtime_cache: PathBuf,
    ) -> io::Result<Self> {
        if !source.path.is_absolute() || bounded_path_len(&source.path)? == 0 {
            return Err(invalid("source path must be an absolute local path"));
        }
        if execution.source_path != source.path {
            return Err(invalid(
                "execution source does not match the planned source",
            ));
        }
        if !program.is_absolute() || bounded_path_len(&program)? == 0 {
            return Err(invalid("program path must be absolute and bounded"));
        }
        if !runtime_cache.is_absolute() || bounded_path_len(&runtime_cache)? == 0 {
            return Err(invalid("runtime cache path must be absolute and bounded"));
        }
        if !duration_seconds.is_finite()
            || duration_seconds <= 0.0
            || !execution.start_seconds.is_finite()
            || execution.start_seconds < 0.0
            || duration_seconds <= execution.start_seconds
        {
            return Err(invalid(
                "VOD duration and start are outside the recipe range",
            ));
        }
        if plan.encoder() != Encoder::Software
            || plan.decode().backend() != crate::transcode::DecodeBackend::Software
            || plan.decode().surface().decode_domain() != FrameDomain::SystemMemory
            || plan.decode().surface().renderer_domain() != FrameDomain::SystemMemory
            || plan.decode().surface().encoder_upload_domain().is_some()
            || plan.output_contract().output_codec() != "h264"
            || plan.options().strict_dolby.is_some()
            || plan.macos_processing_identity().is_some()
            || plan.options().subtitle_burn.is_some()
            || execution.subtitle_file.is_some()
            || !source.dolby_vision.is_empty()
        {
            return Err(invalid(
                "plan is not an ordinary software AVC recipe without subtitles or DV",
            ));
        }

        let has_audio = !source.audio_streams.is_empty();
        if plan.options().input_has_audio != has_audio {
            return Err(invalid(
                "plan audio inputs do not match the held source facts",
            ));
        }
        if let Some(index) = plan.options().audio_index {
            if !source
                .audio_streams
                .iter()
                .any(|stream| stream.index == index)
            {
                return Err(invalid("selected audio stream is absent from the source"));
            }
        }
        if VodFrameGrid::new(grid.numerator, grid.denominator) != Some(grid) {
            return Err(invalid("VOD frame grid is invalid"));
        }

        validate_env(&env)?;
        let mut held_source = source.clone();
        held_source.path = PathBuf::from("/dev/fd/3");
        let mut held_execution = execution.clone();
        held_execution.source_path = held_source.path.clone();
        held_execution.subtitle_file = None;
        let mut args = vod_pipe_args_with_source_clock(
            &held_source,
            plan,
            &held_execution,
            grid,
            duration_seconds,
            reorder,
            source_clock,
        );
        let expected_sources = if has_audio { 2 } else { 1 };
        let mut inputs = 0usize;
        for index in 0..args.len().saturating_sub(1) {
            if args[index] == "-i" {
                inputs += 1;
                args[index + 1] = match inputs {
                    1 => "/dev/fd/3".to_owned(),
                    2 if has_audio => "/dev/fd/4".to_owned(),
                    _ => return Err(invalid("recipe contains an unexpected input")),
                };
            }
        }
        if inputs != expected_sources {
            return Err(invalid("recipe input count differs from held source facts"));
        }
        let command = Self {
            program,
            source_path: source.path.clone(),
            source_id: source.id,
            scanner_size: source.size,
            scanner_mtime: source.mtime,
            grid,
            start_seconds: execution.start_seconds,
            args,
            env,
            runtime_cache,
            source_count: expected_sources,
        };
        validate_collection_limits(&command)?;
        command.resident_memory_bytes()?;
        Ok(command)
    }

    pub(in crate::adaptive_resources::runtime) fn matches_producer(
        &self,
        producer: &super::SoftwareAvcPumpPlan,
    ) -> bool {
        producer.matches_clock(self.grid, self.start_seconds)
    }

    pub(in crate::adaptive_resources::runtime) fn matches_source(
        &self,
        source: &super::FiniteSourceRequest,
    ) -> bool {
        source.path == self.source_path
            && source.media_file_id == self.source_id.to_string()
            && source.scanner_size == self.scanner_size
            && source.scanner_mtime == self.scanner_mtime
    }

    pub(in crate::adaptive_resources::runtime) fn program(&self) -> &PathBuf {
        &self.program
    }

    pub(in crate::adaptive_resources::runtime) fn args(&self) -> &[String] {
        &self.args
    }

    pub(in crate::adaptive_resources::runtime) fn env(&self) -> &[(OsString, OsString)] {
        &self.env
    }

    pub(in crate::adaptive_resources::runtime) fn runtime_cache(&self) -> &PathBuf {
        &self.runtime_cache
    }

    pub(in crate::adaptive_resources::runtime) fn source_count(&self) -> usize {
        self.source_count
    }

    /// Owned command heap capacity only; excludes std::process/Tokio and OS
    /// baselines, descriptor tables, and child allocations.
    pub(in crate::adaptive_resources::runtime) fn resident_memory_bytes(
        &self,
    ) -> io::Result<usize> {
        let mut total = size_of::<Self>()
            .checked_add(self.program.capacity())
            .and_then(|n| n.checked_add(self.source_path.capacity()))
            .and_then(|n| n.checked_add(self.runtime_cache.capacity()))
            .and_then(|n| n.checked_add(self.args.capacity().checked_mul(size_of::<String>())?))
            .and_then(|n| {
                n.checked_add(
                    self.env
                        .capacity()
                        .checked_mul(size_of::<(OsString, OsString)>())?,
                )
            })
            .ok_or_else(|| invalid("command memory accounting overflow"))?;
        for arg in &self.args {
            total = total
                .checked_add(arg.capacity())
                .ok_or_else(|| invalid("command memory accounting overflow"))?;
        }
        for (key, value) in &self.env {
            total = total
                .checked_add(key.capacity())
                .and_then(|n| n.checked_add(value.capacity()))
                .ok_or_else(|| invalid("command memory accounting overflow"))?;
        }
        Ok(total)
    }
}

fn bounded_path_len(path: &std::path::Path) -> io::Result<usize> {
    let text = path.as_os_str().to_string_lossy();
    let len = text.len();
    if len > ITEM_LIMIT || text.contains('\0') {
        return Err(invalid("command path exceeds its bound"));
    }
    Ok(len)
}

fn validate_env(env: &[(OsString, OsString)]) -> io::Result<()> {
    if env.len() > ENV_ENTRY_LIMIT {
        return Err(invalid("child environment has too many entries"));
    }
    for (key, value) in env {
        let key_text = key.to_string_lossy();
        let key_len = os_len(key)?;
        let value = os_len(value)?;
        if key_len == 0 || key_text.contains('=') || value > ITEM_LIMIT {
            return Err(invalid("child environment entry exceeds its bound"));
        }
    }
    Ok(())
}

fn os_len(value: &OsStr) -> io::Result<usize> {
    let text = value.to_string_lossy();
    if text.contains('\0') || text.len() > ITEM_LIMIT {
        return Err(invalid("child environment value exceeds its bound"));
    }
    Ok(text.len())
}

fn validate_collection_limits(command: &SoftwareAvcCommand) -> io::Result<()> {
    if command.args.len() > ITEM_LIMIT {
        return Err(invalid("recipe has too many arguments"));
    }
    let mut args_bytes = 0usize;
    for arg in &command.args {
        if arg.len() > ITEM_LIMIT || arg.contains('\0') {
            return Err(invalid("recipe argument exceeds its bound"));
        }
        args_bytes = args_bytes
            .checked_add(arg.len())
            .ok_or_else(|| invalid("recipe argument accounting overflow"))?;
    }
    if args_bytes > COLLECTION_LIMIT {
        return Err(invalid("recipe arguments exceed their collection bound"));
    }
    let mut env_bytes = 0usize;
    for (key, value) in &command.env {
        let key_bytes = os_len(key)?;
        let value_bytes = os_len(value)?;
        env_bytes = env_bytes
            .checked_add(key_bytes)
            .and_then(|n| n.checked_add(value_bytes))
            .ok_or_else(|| invalid("environment accounting overflow"))?;
    }
    if env_bytes > COLLECTION_LIMIT {
        return Err(invalid("child environment exceeds its collection bound"));
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
