//! A compiled ordinary software-AVC command. The recipe is built from the
//! validated core plan; callers cannot replace its arguments after compilation.

use std::{
    ffi::{OsStr, OsString},
    io,
    mem::size_of,
    path::PathBuf,
    sync::Arc,
};

use crate::{
    domain::MediaFile,
    transcode::{
        vod_pipe_args_with_source_clock, Encoder, FrameDomain, LinuxDolbyExecutionBinding,
        ResolvedTranscode, TranscodeExecution, VodFrameGrid, VodSourceClock,
    },
};

const ITEM_LIMIT: usize = 4_096;
const COLLECTION_LIMIT: usize = 64 * 1_024;
const ENV_ENTRY_LIMIT: usize = 32;
const ENGINE_OBJECT_LIMIT: usize = 65_536;
// The captured dependency report is capped at 1 MiB, while a validated
// Linux package binding can carry up to 4,096 paths of 4 KiB each.
const ENGINE_SNAPSHOT_LIMIT: usize = 32 * 1024 * 1024;
const ENGINE_VERSION_LIMIT: usize = 256;

/// Captured identity fence for the fixed software encoder's loaded objects.
/// These path versions detect recipe drift; they are not native custody or
/// process-closure evidence. The finite profile has no subtitle/font path.
pub struct FiniteEngineSnapshot {
    objects: Vec<(PathBuf, String)>,
    package_binding: Option<LinuxDolbyExecutionBinding>,
}

impl FiniteEngineSnapshot {
    /// Copy the already attested engine list with a fixed metadata ceiling.
    pub fn from_captured(
        objects: &[(PathBuf, String)],
        package_binding: Option<LinuxDolbyExecutionBinding>,
    ) -> io::Result<Self> {
        if objects.is_empty() || objects.len() > ENGINE_OBJECT_LIMIT {
            return Err(invalid("finite engine object count is outside its bound"));
        }
        let mut bytes = objects
            .len()
            .checked_mul(size_of::<(PathBuf, String)>())
            .ok_or_else(|| invalid("finite engine snapshot size overflow"))?;
        for (path, version) in objects {
            let path_bytes = bounded_path_len(path)?;
            if !path.is_absolute()
                || path_bytes == 0
                || version.is_empty()
                || version.len() > ENGINE_VERSION_LIMIT
                || !version
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte == b':' || byte == b'-')
                || version.split(':').count() != 7
            {
                return Err(invalid("finite engine object identity is invalid"));
            }
            bytes = bytes
                .checked_add(path_bytes)
                .and_then(|n| n.checked_add(version.len()))
                .ok_or_else(|| invalid("finite engine snapshot size overflow"))?;
        }
        if bytes > ENGINE_SNAPSHOT_LIMIT {
            return Err(invalid("finite engine snapshot exceeds its bound"));
        }
        let mut copied = Vec::new();
        copied
            .try_reserve_exact(objects.len())
            .map_err(|_| invalid("reserving finite engine snapshot failed"))?;
        if copied
            .capacity()
            .checked_mul(size_of::<(PathBuf, String)>())
            .is_none_or(|capacity| capacity > ENGINE_SNAPSHOT_LIMIT)
        {
            return Err(invalid("finite engine snapshot capacity exceeds its bound"));
        }
        copied.extend(objects.iter().cloned());
        let snapshot = Self {
            objects: copied,
            package_binding,
        };
        if snapshot.resident_memory_bytes()? > ENGINE_SNAPSHOT_LIMIT {
            return Err(invalid(
                "finite engine snapshot allocation exceeds its bound",
            ));
        }
        Ok(snapshot)
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(in crate::adaptive_resources::runtime) fn is_current(&self) -> bool {
        self.checked_current() == Some(true)
    }

    /// `None` means the registered check acquired a digest File whose close
    /// could not be confirmed. Callers must retain the original role charge.
    pub(in crate::adaptive_resources::runtime) fn checked_current(&self) -> Option<bool> {
        if !self.objects.iter().all(|(path, expected)| {
            let Ok(metadata) = std::fs::metadata(path) else {
                return false;
            };
            #[cfg(unix)]
            let actual = {
                use std::os::unix::fs::MetadataExt;
                format!(
                    "{}:{}:{}:{}:{}:{}:{}",
                    metadata.dev(),
                    metadata.ino(),
                    metadata.size(),
                    metadata.mtime(),
                    metadata.mtime_nsec(),
                    metadata.ctime(),
                    metadata.ctime_nsec()
                )
            };
            #[cfg(not(unix))]
            let actual = String::new();
            &actual == expected
        }) {
            return Some(false);
        }
        if let Some(binding) = &self.package_binding {
            for object in binding.objects().iter() {
                match object.checked_current() {
                    Some(true) => {}
                    result => return result,
                }
            }
        }
        Some(true)
    }

    fn resident_memory_bytes(&self) -> io::Result<usize> {
        let mut bytes = size_of::<Self>()
            .checked_add(2 * size_of::<usize>()) // Arc strong/weak counters.
            .and_then(|n| {
                n.checked_add(
                    self.objects
                        .capacity()
                        .checked_mul(size_of::<(PathBuf, String)>())?,
                )
            })
            .ok_or_else(|| invalid("finite engine snapshot memory overflow"))?;
        for (path, version) in &self.objects {
            bytes = bytes
                .checked_add(path.capacity())
                .and_then(|n| n.checked_add(version.capacity()))
                .ok_or_else(|| invalid("finite engine snapshot memory overflow"))?;
        }
        if let Some(binding) = &self.package_binding {
            bytes = bytes
                .checked_add(size_of::<LinuxDolbyExecutionBinding>())
                .and_then(|n| n.checked_add(4 * size_of::<usize>()))
                .and_then(|n| n.checked_add(binding.requested_heap_bytes()?))
                .ok_or_else(|| invalid("finite package snapshot memory overflow"))?;
            for object in binding.objects().iter() {
                let path_bytes = bounded_path_len(&object.path)?;
                if !object.path.is_absolute() || path_bytes == 0 {
                    return Err(invalid("finite package object path is invalid"));
                }
            }
        }
        Ok(bytes)
    }
}

/// Immutable argv and child configuration for the fixed ordinary AVC pump.
/// This owns only planning data; it holds no source descriptor or child proof.
pub struct SoftwareAvcCommand {
    executable: super::SoftwareAvcExecutable,
    engine_snapshot: Arc<FiniteEngineSnapshot>,
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
        executable: super::SoftwareAvcExecutable,
        engine_snapshot: FiniteEngineSnapshot,
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
        if !executable.path().is_absolute() || bounded_path_len(executable.path())? == 0 {
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
            executable,
            engine_snapshot: Arc::new(engine_snapshot),
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

    pub(in crate::adaptive_resources::runtime) fn program(&self) -> &std::path::Path {
        self.executable.path()
    }

    pub(in crate::adaptive_resources::runtime) fn executable(
        &self,
    ) -> &super::SoftwareAvcExecutable {
        &self.executable
    }

    pub(in crate::adaptive_resources::runtime) fn engine_snapshot(
        &self,
    ) -> Arc<FiniteEngineSnapshot> {
        Arc::clone(&self.engine_snapshot)
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
            .checked_add(self.executable.resident_memory_bytes()?)
            .and_then(|n| n.checked_add(self.engine_snapshot.resident_memory_bytes().ok()?))
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

    /// Requested command-owned heap, including the one shared engine snapshot.
    /// The legacy resident estimate also counts the inline command and
    /// executable layouts; those are already part of the enclosing request.
    pub(in crate::adaptive_resources::runtime) fn requested_heap_bytes(&self) -> Option<usize> {
        self.resident_memory_bytes()
            .ok()?
            .checked_sub(size_of::<Self>())?
            .checked_sub(size_of::<super::SoftwareAvcExecutable>())
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
        let fixed_recipe_key = matches!(
            key_text.as_ref(),
            "FONTCONFIG_FILE" | "FONTCONFIG_PATH" | "FONTCONFIG_SYSROOT"
        );
        #[cfg(target_os = "linux")]
        let fixed_recipe_key = fixed_recipe_key || key_text == "LD_LIBRARY_PATH";
        if !fixed_recipe_key {
            return Err(invalid(
                "environment key is outside the fixed software recipe",
            ));
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

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use super::*;
    #[cfg(target_os = "linux")]
    use crate::transcode::LinuxExecutionObject;

    #[cfg(target_os = "linux")]
    #[test]
    fn finite_engine_snapshot_detects_changed_dependency_before_publication() {
        use std::os::unix::fs::MetadataExt;

        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/finite-engine-snapshot-tests");
        std::fs::create_dir_all(&directory).expect("repo-local test directory");
        let path = directory.join(format!("dependency-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"first").expect("initial dependency");
        let metadata = std::fs::metadata(&path).expect("initial identity");
        let version = format!(
            "{}:{}:{}:{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.size(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec()
        );
        let snapshot = FiniteEngineSnapshot::from_captured(&[(path.clone(), version)], None)
            .expect("bounded captured dependency");
        assert!(snapshot.is_current());
        std::fs::write(&path, b"replaced dependency bytes").expect("mutated dependency");
        assert!(!snapshot.is_current());
        std::fs::remove_file(&path).expect("remove test dependency");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn finite_engine_snapshot_checks_bound_package_objects_too() {
        use crate::transcode::LinuxExecutionObjectKind;
        use std::os::unix::fs::MetadataExt;

        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/finite-engine-snapshot-tests");
        std::fs::create_dir_all(&directory).expect("repo-local test directory");
        let dependency = directory.join(format!("dependency-{}", uuid::Uuid::new_v4()));
        let package = directory.join(format!("package-{}", uuid::Uuid::new_v4()));
        std::fs::write(&dependency, b"dependency").expect("captured dependency");
        std::fs::write(&package, b"package").expect("captured package");
        let version = |path: &PathBuf| {
            let metadata = std::fs::metadata(path).expect("captured native identity");
            format!(
                "{}:{}:{}:{}:{}:{}:{}",
                metadata.dev(),
                metadata.ino(),
                metadata.size(),
                metadata.mtime(),
                metadata.mtime_nsec(),
                metadata.ctime(),
                metadata.ctime_nsec()
            )
        };
        let binding = LinuxDolbyExecutionBinding::new(
            vec![LinuxExecutionObject {
                path: package.clone(),
                version: Some(version(&package)),
                kind: LinuxExecutionObjectKind::ProducerExecutable,
            }],
            Vec::new(),
        )
        .expect("valid captured package binding");
        let snapshot = FiniteEngineSnapshot::from_captured(
            &[(dependency.clone(), version(&dependency))],
            Some(binding),
        )
        .expect("bounded package fence");
        assert!(snapshot.is_current());
        std::fs::write(&package, b"changed package bytes").expect("package drift");
        assert!(!snapshot.is_current());
        std::fs::remove_file(&dependency).expect("remove dependency");
        std::fs::remove_file(&package).expect("remove package");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn finite_environment_accepts_bounded_loader_and_font_entries() {
        let env = vec![
            (
                OsString::from("LD_LIBRARY_PATH"),
                OsString::from("/opt/ffmpeg/lib"),
            ),
            (
                OsString::from("FONTCONFIG_FILE"),
                OsString::from("/etc/fonts/fonts.conf"),
            ),
        ];
        assert!(validate_env(&env).is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn finite_environment_rejects_unbounded_or_unapproved_entries() {
        let too_long = vec![(
            OsString::from("LD_LIBRARY_PATH"),
            OsString::from("x".repeat(ITEM_LIMIT + 1)),
        )];
        assert!(validate_env(&too_long).is_err());

        let unapproved = vec![(OsString::from("PATH"), OsString::from("/usr/bin"))];
        assert!(validate_env(&unapproved).is_err());

        let nul_value = vec![(
            OsString::from("LD_LIBRARY_PATH"),
            OsString::from("/opt/ffmpeg\0/lib"),
        )];
        assert!(validate_env(&nul_value).is_err());
    }
}
