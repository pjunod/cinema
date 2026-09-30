//! Bounded Linux helper for observing, inspecting and ejecting optical media.
//!
//! The daemon supplies only trusted startup-config paths. This process keeps
//! filesystem parsing and device ioctls outside the long-lived server and
//! emits one versioned JSON reply on standard output.

#[cfg(target_os = "linux")]
fn main() {
    linux::run_main();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("plurx-optical-helper is supported on Linux only");
    std::process::exit(1);
}

#[cfg(unix)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod linux {

    use std::collections::BTreeSet;
    use std::fs::{self, File};
    use std::io::{Read, Seek, SeekFrom};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
    #[cfg(target_os = "linux")]
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    use clap::{Parser, Subcommand};
    use plurx_core::domain::{AudioStream, DolbyVisionFacts, SubtitleStream};
    use plurx_core::optical::{
        FingerprintEvidence, InspectedChapter, InspectedDisc, InspectedStream, InspectedTitle,
        InspectionResponse, OpticalFormat, OpticalTitleLocator, ProtectionFacts, ResolvedInput,
        INSPECTION_SCHEMA_V1, MAX_BLURAY_PLAYLIST_NUMBER, OPTICAL_FINGERPRINT_V1,
    };
    use plurx_core::playback::{PlaybackMediaFacts, SourceDelivery};
    use serde::Serialize;
    use serde_json::Value;
    use sha2::{Digest, Sha256};

    const MAX_NAVIGATION_FILES: usize = 4096;
    const MAX_NAVIGATION_ENTRIES: usize = 16_384;
    const MAX_NAVIGATION_DEPTH: usize = 16;
    const MAX_PLAYLIST_ENTRIES: usize = 4096;
    const MAX_NAVIGATION_BYTES: u64 = 16 * 1024 * 1024;
    const SAMPLE_BYTES: u64 = 64 * 1024;
    const MAX_TITLES: usize = 512;
    const MAX_STREAMS_PER_TITLE: usize = 256;
    const MAX_CHAPTERS_PER_TITLE: usize = 4096;
    const MAX_PROBE_BYTES: usize = 1024 * 1024;
    const MAX_PROBE_STDERR_BYTES: usize = 64 * 1024;
    const MAX_PROBE_TEXT_BYTES: usize = 512;
    const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024;
    const PROTECTION_ERROR_PREFIX: &str = "optical-protection-unsupported:";

    #[derive(Parser)]
    #[command(name = "plurx-optical-helper")]
    struct Args {
        #[command(subcommand)]
        command: Action,
    }

    #[derive(Subcommand)]
    enum Action {
        Presence(Paths),
        Inspect(InspectArgs),
        Eject(Paths),
    }

    #[derive(clap::Args, Clone)]
    struct Paths {
        #[arg(long)]
        device: PathBuf,
        #[arg(long)]
        mount: Option<PathBuf>,
    }

    #[derive(clap::Args)]
    struct InspectArgs {
        #[command(flatten)]
        paths: Paths,
        #[arg(long)]
        schema: u32,
        #[arg(long)]
        expected_generation: String,
    }

    #[derive(Serialize)]
    struct PresenceReply {
        presence: &'static str,
    }

    #[derive(Serialize)]
    struct FailureReply<'a> {
        code: &'static str,
        message: &'a str,
    }

    pub(super) fn run_main() {
        if let Err(error) = run(Args::parse()) {
            if let Some(message) = error.strip_prefix(PROTECTION_ERROR_PREFIX) {
                let reply = FailureReply {
                    code: "optical_protection_unsupported",
                    message,
                };
                if write_json(&reply).is_err() {
                    eprintln!("{message}");
                }
            } else {
                eprintln!("{error}");
            }
            std::process::exit(1);
        }
    }

    fn run(args: Args) -> Result<(), String> {
        match args.command {
            Action::Presence(paths) => {
                let presence = presence(&paths)?;
                write_json(&PresenceReply { presence })
            }
            Action::Inspect(args) => write_json(&inspect(args)?),
            Action::Eject(paths) => eject(&paths),
        }
    }

    fn write_json(value: &impl Serialize) -> Result<(), String> {
        serde_json::to_writer(std::io::stdout().lock(), value).map_err(|error| error.to_string())
    }

    fn validate_paths(paths: &Paths) -> Result<(), String> {
        if !paths.device.is_absolute() {
            return Err("device path must be absolute".into());
        }
        if let Some(mount) = &paths.mount {
            if !mount.is_absolute() {
                return Err("mount path must be absolute".into());
            }
            if mount.exists() && !mount.is_dir() {
                return Err("mount path is not a directory".into());
            }
        }
        if let Ok(metadata) = fs::metadata(&paths.device) {
            if !metadata.file_type().is_block_device() {
                return Err("configured device is not a block device".into());
            }
        }
        Ok(())
    }

    fn mount_has_media(mount: Option<&Path>) -> bool {
        mount.is_some_and(|mount| {
            mount.join("VIDEO_TS").is_dir()
                || mount.join("video_ts").is_dir()
                || mount.join("BDMV").is_dir()
                || mount.join("bdmv").is_dir()
        })
    }

    fn presence(paths: &Paths) -> Result<&'static str, String> {
        validate_paths(paths)?;
        if !paths.device.exists() {
            return Ok(if mount_has_media(paths.mount.as_deref()) {
                "present"
            } else {
                "unknown"
            });
        }
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&paths.device)
        {
            Ok(file) => file,
            Err(_) if mount_has_media(paths.mount.as_deref()) => return Ok("present"),
            Err(error) => return Err(format!("cannot open optical device: {error}")),
        };
        const CDROM_DRIVE_STATUS: libc::c_ulong = 0x5326;
        const CDROM_MEDIA_CHANGED: libc::c_ulong = 0x5325;
        const CDSL_CURRENT: libc::c_int = i32::MAX;
        let status = unsafe {
            libc::ioctl(
                std::os::fd::AsRawFd::as_raw_fd(&file),
                CDROM_DRIVE_STATUS,
                CDSL_CURRENT,
            )
        };
        Ok(match status {
            4 => {
                let changed = unsafe {
                    libc::ioctl(
                        std::os::fd::AsRawFd::as_raw_fd(&file),
                        CDROM_MEDIA_CHANGED,
                        CDSL_CURRENT,
                    )
                };
                if changed == 1 {
                    "changed"
                } else {
                    "present"
                }
            }
            1 | 2 => "empty",
            _ => "unknown",
        })
    }

    fn eject(paths: &Paths) -> Result<(), String> {
        validate_paths(paths)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&paths.device)
            .map_err(|error| format!("cannot open optical device: {error}"))?;
        const CDROMEJECT: libc::c_ulong = 0x5309;
        let result = unsafe { libc::ioctl(std::os::fd::AsRawFd::as_raw_fd(&file), CDROMEJECT) };
        if result == 0 {
            Ok(())
        } else {
            Err(format!("eject failed: {}", std::io::Error::last_os_error()))
        }
    }

    fn inspect(args: InspectArgs) -> Result<InspectionResponse, String> {
        validate_paths(&args.paths)?;
        if args.schema != INSPECTION_SCHEMA_V1 {
            return Err("unsupported inspection schema".into());
        }
        if !(1..=192).contains(&args.expected_generation.len()) {
            return Err("expected generation is invalid".into());
        }
        let mount = args
            .paths
            .mount
            .as_deref()
            .ok_or_else(|| "inspection requires a configured read-only mount".to_owned())?;
        let mount = fs::canonicalize(mount)
            .map_err(|error| format!("cannot resolve optical mount: {error}"))?;
        require_read_only_mount(&mount)?;
        require_mount_matches_device(&args.paths.device, &mount)?;
        let mut paths = args.paths;
        paths.mount = Some(mount.clone());
        let dvd_root = find_case_path(&mount, "VIDEO_TS", true)?;
        let bluray_root = find_case_path(&mount, "BDMV", true)?;
        let (format, navigation_root, locators) = match (dvd_root, bluray_root) {
            (Some(root), None) => {
                let ifo = find_case_path(&root, "VIDEO_TS.IFO", false)?
                    .ok_or_else(|| "DVD VIDEO_TS.IFO is missing".to_owned())?;
                let count = dvd_title_count(&ifo)?;
                let locators = (1..=count.min(MAX_TITLES as u32))
                    .map(|title_number| OpticalTitleLocator::Dvd { title_number })
                    .collect();
                (OpticalFormat::Dvd, root, locators)
            }
            (None, Some(root)) => {
                let locators = bluray_playlists(&root)?;
                (OpticalFormat::Bluray, root, locators)
            }
            (Some(_), Some(_)) => {
                return Err("mounted media has ambiguous DVD and Blu-ray navigation roots".into());
            }
            (None, None) => {
                return Err("mounted media is not DVD-Video or Blu-ray".into());
            }
        };
        if locators.is_empty() {
            return Err("no playable optical titles were found".into());
        }
        let fingerprint = fingerprint(format, &mount, &navigation_root)?;
        let ffprobe = std::env::var_os("PLURX_OPTICAL_FFPROBE")
            .filter(|value| !value.is_empty())
            .or_else(|| std::env::var_os("PLURX_FFPROBE").filter(|value| !value.is_empty()))
            .unwrap_or_else(|| "ffprobe".into());
        let mut titles = Vec::new();
        let mut diagnostics = Vec::new();
        for locator in locators {
            match probe_title(&ffprobe, format, &paths, locator) {
                Ok(title) => titles.push(title),
                Err(error) => diagnostics.push(error),
            }
        }
        if titles.is_empty()
            && diagnostics
                .iter()
                .any(|error| error.starts_with(PROTECTION_ERROR_PREFIX))
        {
            return Err(format!(
                "{PROTECTION_ERROR_PREFIX}the inserted disc is protected and the configured reader cannot decrypt it"
            ));
        }
        if titles.is_empty() {
            return Err(format!(
                "no title could be probed: {}",
                diagnostics.join("; ")
            ));
        }
        Ok(InspectionResponse {
            schema_version: INSPECTION_SCHEMA_V1,
            expected_generation: args.expected_generation,
            disc: InspectedDisc {
                format,
                // A mountpoint name is node-local configuration, not disc
                // metadata. Preserve unknown until a bounded format parser
                // obtains a label from the media itself.
                volume_label: None,
                protection: ProtectionFacts {
                    detected: None,
                    implementation_available: None,
                    handled: None,
                    scheme: None,
                },
                fingerprint,
                titles,
            },
            diagnostics: bounded_text(&diagnostics.join("; "), MAX_DIAGNOSTIC_BYTES),
        })
    }

    fn reject_symlink(path: &Path) -> Result<(), String> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| format!("cannot inspect optical path: {error}"))?;
        if metadata.file_type().is_symlink() {
            Err("optical navigation paths must not contain symlinks".into())
        } else {
            Ok(())
        }
    }

    fn require_read_only_mount(mount: &Path) -> Result<(), String> {
        let path = std::ffi::CString::new(mount.as_os_str().as_bytes())
            .map_err(|_| "optical mount path contains a NUL byte")?;
        let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        let result = unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) };
        if result != 0 {
            return Err(format!(
                "cannot inspect optical mount flags: {}",
                std::io::Error::last_os_error()
            ));
        }
        let stats = unsafe { stats.assume_init() };
        if stats.f_flag & (libc::ST_RDONLY as libc::c_ulong) == 0 {
            return Err("optical mount must be read-only".into());
        }
        Ok(())
    }

    fn require_mount_matches_device(device: &Path, mount: &Path) -> Result<(), String> {
        let device = fs::metadata(device)
            .map_err(|error| format!("cannot inspect optical device identity: {error}"))?;
        if !device.file_type().is_block_device() {
            return Err("configured device is not a block device".into());
        }
        let mount = fs::metadata(mount)
            .map_err(|error| format!("cannot inspect optical mount identity: {error}"))?;
        if mount.dev() != device.rdev() {
            return Err("configured optical mount does not belong to the configured device".into());
        }
        Ok(())
    }

    fn find_case_path(
        root: &Path,
        wanted: &str,
        directory: bool,
    ) -> Result<Option<PathBuf>, String> {
        let mut found = None;
        for (index, entry) in fs::read_dir(root)
            .map_err(|error| format!("cannot read optical directory: {error}"))?
            .enumerate()
        {
            if index >= MAX_NAVIGATION_ENTRIES {
                return Err("optical directory entry count exceeds its bound".into());
            }
            let entry = entry.map_err(|error| format!("cannot read optical entry: {error}"))?;
            if !entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(wanted)
            {
                continue;
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| format!("cannot inspect optical path: {error}"))?;
            if metadata.file_type().is_symlink() {
                return Err("optical navigation paths must not contain symlinks".into());
            }
            if metadata.is_dir() != directory || (!directory && !metadata.is_file()) {
                return Err(format!(
                    "optical navigation entry {wanted} has the wrong type"
                ));
            }
            if found.replace(path).is_some() {
                return Err(format!("optical navigation entry {wanted} is ambiguous"));
            }
        }
        Ok(found)
    }

    fn dvd_title_count(ifo: &Path) -> Result<u32, String> {
        let mut file =
            File::open(ifo).map_err(|error| format!("cannot read VIDEO_TS.IFO: {error}"))?;
        let mut header = [0_u8; 0xC8];
        file.read_exact(&mut header)
            .map_err(|error| format!("short VIDEO_TS.IFO header: {error}"))?;
        if &header[..12] != b"DVDVIDEO-VMG" {
            return Err("VIDEO_TS.IFO is not a DVD VMG".into());
        }
        let sector = u32::from_be_bytes(header[0xC4..0xC8].try_into().map_err(|_| "invalid VMG")?);
        file.seek(SeekFrom::Start(u64::from(sector) * 2048))
            .map_err(|error| format!("cannot seek DVD title table: {error}"))?;
        let mut count = [0_u8; 2];
        file.read_exact(&mut count)
            .map_err(|error| format!("cannot read DVD title table: {error}"))?;
        let count = u16::from_be_bytes(count) as u32;
        (count > 0)
            .then_some(count)
            .ok_or_else(|| "DVD reports no titles".into())
    }

    fn bluray_playlists(bdmv: &Path) -> Result<Vec<OpticalTitleLocator>, String> {
        let playlist = find_case_path(bdmv, "PLAYLIST", true)?
            .ok_or_else(|| "Blu-ray PLAYLIST directory is missing".to_owned())?;
        let mut numbers = BTreeSet::new();
        for (entry_index, entry) in fs::read_dir(playlist)
            .map_err(|error| error.to_string())?
            .enumerate()
        {
            if entry_index >= MAX_PLAYLIST_ENTRIES {
                return Err("Blu-ray playlist entry count exceeds its bound".into());
            }
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            reject_symlink(&path)?;
            if !path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("mpls"))
            {
                continue;
            }
            if let Some(number) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|stem| stem.parse::<u32>().ok())
                .filter(|number| *number <= MAX_BLURAY_PLAYLIST_NUMBER)
            {
                numbers.insert(number);
            }
            if numbers.len() >= MAX_TITLES {
                break;
            }
        }
        Ok(numbers
            .into_iter()
            .map(|playlist_number| OpticalTitleLocator::Bluray { playlist_number })
            .collect())
    }

    fn navigation_files(format: OpticalFormat, root: &Path) -> Result<Vec<PathBuf>, String> {
        let mut files = Vec::new();
        let mut entries_seen = 0_usize;
        let mut stack = vec![(root.to_owned(), 0_usize)];
        while let Some((directory, depth)) = stack.pop() {
            for entry in fs::read_dir(&directory).map_err(|error| error.to_string())? {
                entries_seen = entries_seen.saturating_add(1);
                if entries_seen > MAX_NAVIGATION_ENTRIES {
                    return Err("optical navigation entry count exceeds its bound".into());
                }
                let entry = entry.map_err(|error| error.to_string())?;
                let path = entry.path();
                let metadata = fs::symlink_metadata(&path)
                    .map_err(|error| format!("cannot inspect optical path: {error}"))?;
                if metadata.file_type().is_symlink() {
                    return Err("optical navigation paths must not contain symlinks".into());
                }
                if metadata.is_dir() {
                    let child_depth = depth.saturating_add(1);
                    if child_depth > MAX_NAVIGATION_DEPTH {
                        return Err("optical navigation directory depth exceeds its bound".into());
                    }
                    stack.push((path, child_depth));
                    continue;
                }
                let keep = match format {
                    OpticalFormat::Dvd => path.extension().is_some_and(|ext| {
                        ext.eq_ignore_ascii_case("ifo") || ext.eq_ignore_ascii_case("bup")
                    }),
                    OpticalFormat::Bluray => path.extension().is_some_and(|ext| {
                        ext.eq_ignore_ascii_case("bdmv")
                            || ext.eq_ignore_ascii_case("mpls")
                            || ext.eq_ignore_ascii_case("clpi")
                    }),
                };
                if keep {
                    files.push(path);
                    if files.len() > MAX_NAVIGATION_FILES {
                        return Err("optical navigation file count exceeds its bound".into());
                    }
                }
            }
        }
        files.sort();
        Ok(files)
    }

    fn fingerprint(
        format: OpticalFormat,
        mount: &Path,
        navigation_root: &Path,
    ) -> Result<FingerprintEvidence, String> {
        let files = navigation_files(format, navigation_root)?;
        if files.is_empty() {
            return Err("no navigation files found for fingerprint".into());
        }
        let mut hash = Sha256::new();
        hash.update(format.as_str().as_bytes());
        let mut navigation_bytes = 0_u64;
        let mut sampled = 0_u64;
        for path in files {
            let relative = path
                .strip_prefix(mount)
                .map_err(|_| "navigation path escaped mount")?;
            let metadata = fs::metadata(&path).map_err(|error| error.to_string())?;
            let relative = relative.as_os_str().as_encoded_bytes();
            hash.update((relative.len() as u64).to_be_bytes());
            hash.update(relative);
            hash.update(metadata.len().to_be_bytes());
            navigation_bytes = navigation_bytes.saturating_add(metadata.len());
            let remaining = MAX_NAVIGATION_BYTES.saturating_sub(sampled);
            if remaining == 0 {
                continue;
            }
            let mut file = File::open(&path).map_err(|error| error.to_string())?;
            let sample = metadata
                .len()
                .min(SAMPLE_BYTES.saturating_mul(2))
                .min(remaining);
            let prefix_len = sample.min(SAMPLE_BYTES);
            let mut prefix = vec![0_u8; prefix_len as usize];
            file.read_exact(&mut prefix)
                .map_err(|error| error.to_string())?;
            hash.update(&prefix);
            sampled = sampled.saturating_add(prefix_len);
            let suffix_len = sample.saturating_sub(prefix_len);
            if suffix_len > 0 {
                file.seek(SeekFrom::End(-(suffix_len as i64)))
                    .map_err(|error| error.to_string())?;
                let mut suffix = vec![0_u8; suffix_len as usize];
                file.read_exact(&mut suffix)
                    .map_err(|error| error.to_string())?;
                hash.update(&suffix);
                sampled = sampled.saturating_add(suffix_len);
            }
        }
        Ok(FingerprintEvidence {
            version: OPTICAL_FINGERPRINT_V1,
            // A digest built from prefixes/suffixes is useful for bounded
            // diagnostics but cannot safely inherit durable progress or
            // matches across reinsertion. Only claim complete identity when
            // every navigation byte contributed to the digest.
            complete: sampled == navigation_bytes,
            digest: hex::encode(hash.finalize()),
            navigation_bytes,
            bounded_sample_bytes: sampled,
        })
    }

    fn probe_title(
        ffprobe: &std::ffi::OsStr,
        format: OpticalFormat,
        paths: &Paths,
        locator: OpticalTitleLocator,
    ) -> Result<InspectedTitle, String> {
        let input = match locator {
            OpticalTitleLocator::Dvd { title_number } => ResolvedInput::Dvd {
                path: if paths.device.exists() {
                    paths.device.clone()
                } else {
                    paths.mount.clone().ok_or("DVD mount is missing")?
                },
                title_number,
                angle: 1,
            },
            OpticalTitleLocator::Bluray { playlist_number } => ResolvedInput::Bluray {
                path: paths.mount.clone().ok_or("Blu-ray mount is missing")?,
                playlist_number,
                angle: 1,
            },
        };
        let mut args = vec!["-v".into(), "error".into()];
        input
            .append_input_args(&mut args)
            .map_err(|error| error.to_string())?;
        args.extend([
            "-show_format".into(),
            "-show_streams".into(),
            "-show_chapters".into(),
            "-of".into(),
            "json".into(),
        ]);
        let mut command = Command::new(ffprobe);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        bind_child_to_helper_lifetime(&mut command);
        let mut child = command
            .spawn()
            .map_err(|error| format!("ffprobe is unavailable: {error}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "ffprobe stdout is unavailable".to_owned())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "ffprobe stderr is unavailable".to_owned())?;
        // Drain both pipes concurrently so neither can block the child, but
        // retain only bounded prefixes. The daemon supplies the outer process
        // timeout and kills this helper (and its ffprobe child) on expiry.
        let stdout = std::thread::spawn(move || read_bounded_and_drain(stdout, MAX_PROBE_BYTES));
        let stderr =
            std::thread::spawn(move || read_bounded_and_drain(stderr, MAX_PROBE_STDERR_BYTES));
        let status = child
            .wait()
            .map_err(|error| format!("waiting for ffprobe failed: {error}"))?;
        let stdout = stdout
            .join()
            .map_err(|_| "ffprobe stdout reader failed".to_owned())??;
        let stderr = stderr
            .join()
            .map_err(|_| "ffprobe stderr reader failed".to_owned())??;
        if !status.success() {
            let stderr = String::from_utf8_lossy(&stderr);
            if probe_reports_unsupported_protection(&stderr) {
                return Err(format!(
                    "{PROTECTION_ERROR_PREFIX}the inserted disc is protected and the configured reader cannot decrypt it"
                ));
            }
            return Err(bounded_text(&stderr, MAX_PROBE_TEXT_BYTES));
        }
        if stdout.len() > MAX_PROBE_BYTES {
            return Err("ffprobe title reply exceeded its byte bound".into());
        }
        let document: Value = serde_json::from_slice(&stdout).map_err(|error| error.to_string())?;
        title_from_probe(format, locator, &document)
    }

    fn read_bounded_and_drain(mut reader: impl Read, limit: usize) -> Result<Vec<u8>, String> {
        let mut retained = Vec::with_capacity(limit.min(64 * 1024).saturating_add(1));
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            let count = reader
                .read(&mut buffer)
                .map_err(|error| format!("reading ffprobe output failed: {error}"))?;
            if count == 0 {
                break;
            }
            let remaining = limit.saturating_add(1).saturating_sub(retained.len());
            retained.extend_from_slice(&buffer[..count.min(remaining)]);
        }
        Ok(retained)
    }

    fn bind_child_to_helper_lifetime(command: &mut Command) {
        #[cfg(target_os = "linux")]
        {
            let helper_pid = unsafe { libc::getpid() };
            unsafe {
                command.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    // The parent can die between fork and prctl. Refuse the
                    // exec in that window rather than leaving an orphaned
                    // reader holding the drive after the daemon's timeout.
                    if libc::getppid() != helper_pid {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "optical helper exited before probe exec",
                        ));
                    }
                    Ok(())
                });
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = command;
    }

    fn probe_reports_unsupported_protection(stderr: &str) -> bool {
        let normalized = stderr.to_ascii_lowercase();
        [
            "can't decrypt this media",
            "cannot decrypt this media",
            "aacs_open() failed",
            "no valid aacs configuration files found",
            "no usable aacs libraries found",
            "bdplus_init() failed",
        ]
        .iter()
        .any(|marker| normalized.contains(marker))
    }

    fn string(value: &Value, key: &str) -> Option<String> {
        value
            .get(key)?
            .as_str()
            .map(|value| bounded_text(value, MAX_PROBE_TEXT_BYTES))
    }

    fn bounded_text(value: &str, max_bytes: usize) -> String {
        let mut result = String::new();
        for character in value.chars().filter(|character| !character.is_control()) {
            if result.len().saturating_add(character.len_utf8()) > max_bytes {
                break;
            }
            result.push(character);
        }
        result
    }

    fn integer(value: &Value, key: &str) -> Option<i64> {
        value
            .get(key)
            .and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()))
    }

    fn seconds_ms(value: Option<&Value>) -> Option<u64> {
        let seconds = value?.as_str()?.parse::<f64>().ok()?;
        (seconds.is_finite() && seconds >= 0.0).then(|| (seconds * 1000.0).round() as u64)
    }

    fn title_from_probe(
        format: OpticalFormat,
        locator: OpticalTitleLocator,
        document: &Value,
    ) -> Result<InspectedTitle, String> {
        let streams = document
            .get("streams")
            .and_then(Value::as_array)
            .ok_or_else(|| "ffprobe title has no streams".to_owned())?;
        if streams.len() > MAX_STREAMS_PER_TITLE {
            return Err("ffprobe title contains too many streams".into());
        }
        let chapter_values = document
            .get("chapters")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if chapter_values.len() > MAX_CHAPTERS_PER_TITLE {
            return Err("ffprobe title contains too many chapters".into());
        }
        let video = streams
            .iter()
            .find(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("video"));
        let duration_ms = seconds_ms(
            document
                .get("format")
                .and_then(|format| format.get("duration")),
        );
        let audio_streams = streams
            .iter()
            .filter(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("audio"))
            .enumerate()
            .map(|(index, stream)| {
                Ok(AudioStream {
                    index: i64::try_from(index)
                        .map_err(|_| "audio stream index exceeds its bound")?,
                    codec: string(stream, "codec_name")
                        .filter(|value| !value.is_empty())
                        .ok_or("audio stream codec is missing")?,
                    channels: integer(stream, "channels"),
                    language: stream.get("tags").and_then(|tags| string(tags, "language")),
                    title: stream.get("tags").and_then(|tags| string(tags, "title")),
                    default: stream
                        .get("disposition")
                        .and_then(|value| integer(value, "default"))
                        == Some(1),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let subtitle_streams = streams
            .iter()
            .filter(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("subtitle"))
            .enumerate()
            .map(|(index, stream)| {
                Ok(SubtitleStream {
                    index: i64::try_from(index)
                        .map_err(|_| "subtitle stream index exceeds its bound")?,
                    codec: string(stream, "codec_name")
                        .filter(|value| !value.is_empty())
                        .ok_or("subtitle stream codec is missing")?,
                    language: stream.get("tags").and_then(|tags| string(tags, "language")),
                    title: stream.get("tags").and_then(|tags| string(tags, "title")),
                    default: stream
                        .get("disposition")
                        .and_then(|value| integer(value, "default"))
                        == Some(1),
                    forced: stream
                        .get("disposition")
                        .and_then(|value| integer(value, "forced"))
                        == Some(1),
                    hearing_impaired: stream
                        .get("disposition")
                        .and_then(|value| integer(value, "hearing_impaired"))
                        == Some(1),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let inspected_streams = streams
            .iter()
            .map(|stream| {
                Ok(InspectedStream {
                    index: integer(stream, "index").ok_or("stream index is missing")?,
                    kind: string(stream, "codec_type")
                        .filter(|value| !value.is_empty())
                        .ok_or("stream kind is missing")?,
                    codec: string(stream, "codec_name"),
                    language: stream.get("tags").and_then(|tags| string(tags, "language")),
                    title: stream.get("tags").and_then(|tags| string(tags, "title")),
                    channels: integer(stream, "channels")
                        .map(u32::try_from)
                        .transpose()
                        .map_err(|_| "stream channel count is invalid")?,
                    default: stream
                        .get("disposition")
                        .and_then(|value| integer(value, "default"))
                        == Some(1),
                    forced: stream
                        .get("disposition")
                        .and_then(|value| integer(value, "forced"))
                        == Some(1),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let chapters = chapter_values
            .iter()
            .enumerate()
            .map(|(index, chapter)| {
                Ok(InspectedChapter {
                    index: u32::try_from(index).map_err(|_| "chapter index exceeds its bound")?,
                    start_ms: seconds_ms(chapter.get("start_time")),
                    // FFprobe's navigation timestamp is useful for a chapter
                    // start, but this path has not built the physical title's
                    // cell/clip index. Do not promise frame-accurate markers.
                    accurate: false,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let locator_json = serde_json::to_vec(&locator).map_err(|error| error.to_string())?;
        let title_id = format!("title-{}", &hex::encode(Sha256::digest(locator_json))[..20]);
        let container = document
            .get("format")
            .and_then(|value| string(value, "format_name"));
        Ok(InspectedTitle {
            title_id,
            locator,
            duration_ms,
            angles: 1,
            facts: PlaybackMediaFacts {
                duration_ms: duration_ms.and_then(|value| i64::try_from(value).ok()),
                container,
                video_codec: video.and_then(|value| string(value, "codec_name")),
                video_codec_tag: video.and_then(|value| string(value, "codec_tag_string")),
                video_profile: video.and_then(|value| string(value, "profile")),
                width: video.and_then(|value| integer(value, "width")),
                height: video.and_then(|value| integer(value, "height")),
                bit_depth: video.and_then(|value| integer(value, "bits_per_raw_sample")),
                hdr: None,
                hdr_format: None,
                dolby_vision: DolbyVisionFacts::default(),
                bitrate: document
                    .get("format")
                    .and_then(|value| integer(value, "bit_rate")),
                audio_streams,
                subtitle_streams,
                audio_offset_ms: 0,
                probed: true,
                source_delivery: SourceDelivery::ManagedOpticalTitle,
                learned_limit_identity: None,
            },
            probe_json: serde_json::to_string(document).map_err(|error| error.to_string())?,
            streams: inspected_streams,
            chapters,
            suggested_feature_score: duration_ms.map(|duration| {
                u16::try_from((duration / 60_000).min(u64::from(u16::MAX))).unwrap_or(u16::MAX)
            }),
            suggestion_reasons: vec![format!(
                "{} title duration is used only as a display heuristic",
                format.as_str()
            )],
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Write;

        #[test]
        fn dvd_title_table_is_bounded_and_big_endian() {
            let directory = tempfile::tempdir().expect("tempdir");
            let path = directory.path().join("VIDEO_TS.IFO");
            let mut bytes = vec![0_u8; 4098];
            bytes[..12].copy_from_slice(b"DVDVIDEO-VMG");
            bytes[0xC4..0xC8].copy_from_slice(&2_u32.to_be_bytes());
            bytes[4096..4098].copy_from_slice(&3_u16.to_be_bytes());
            fs::write(&path, bytes).expect("fixture");
            assert_eq!(dvd_title_count(&path).expect("title table"), 3);
        }

        #[test]
        fn bluray_playlist_enumeration_is_sorted_and_ignores_clip_files() {
            let directory = tempfile::tempdir().expect("tempdir");
            let bdmv = directory.path().join("BDMV");
            let playlist = bdmv.join("PLAYLIST");
            fs::create_dir_all(&playlist).expect("playlist directory");
            fs::write(playlist.join("00000.mpls"), []).expect("playlist");
            fs::write(playlist.join("00800.mpls"), []).expect("playlist");
            fs::write(playlist.join("00001.MPLS"), []).expect("playlist");
            fs::write(playlist.join("00002.m2ts"), []).expect("clip decoy");
            fs::write(playlist.join("100000.mpls"), []).expect("out-of-range playlist");
            assert_eq!(
                bluray_playlists(&bdmv).expect("playlists"),
                vec![
                    OpticalTitleLocator::Bluray { playlist_number: 0 },
                    OpticalTitleLocator::Bluray { playlist_number: 1 },
                    OpticalTitleLocator::Bluray {
                        playlist_number: 800
                    },
                ]
            );
        }

        #[test]
        fn bluray_probe_protection_failures_have_a_stable_classification() {
            assert!(probe_reports_unsupported_protection(
                "[bluray] Your libaacs can't decrypt this media"
            ));
            assert!(probe_reports_unsupported_protection(
                "aacs.c:121: No usable AACS libraries found!"
            ));
            assert!(!probe_reports_unsupported_protection(
                "bluray: Input/output error"
            ));
        }

        #[test]
        fn probe_output_reader_drains_while_retaining_only_the_bound() {
            let input = vec![7_u8; 128];
            let retained =
                read_bounded_and_drain(std::io::Cursor::new(input), 32).expect("bounded reader");
            assert_eq!(retained, vec![7_u8; 33]);
        }

        #[test]
        fn probed_display_text_is_byte_bounded_and_control_free() {
            let text = format!("English\n{}", "é".repeat(MAX_PROBE_TEXT_BYTES));
            let bounded = bounded_text(&text, MAX_PROBE_TEXT_BYTES);
            assert!(!bounded.chars().any(char::is_control));
            assert!(bounded.len() <= MAX_PROBE_TEXT_BYTES);
            assert!(bounded.starts_with("English"));
        }

        #[test]
        fn title_probe_requires_explicit_bounded_stream_identities() {
            let missing_index = serde_json::json!({
                "streams": [{"codec_type": "video", "codec_name": "h264"}],
                "format": {"duration": "60.0"}
            });
            assert!(title_from_probe(
                OpticalFormat::Bluray,
                OpticalTitleLocator::Bluray { playlist_number: 1 },
                &missing_index,
            )
            .is_err());

            let too_many = serde_json::json!({
                "streams": (0..=MAX_STREAMS_PER_TITLE)
                    .map(|index| serde_json::json!({
                        "index": index,
                        "codec_type": "audio",
                        "codec_name": "aac"
                    }))
                    .collect::<Vec<_>>()
            });
            assert!(title_from_probe(
                OpticalFormat::Bluray,
                OpticalTitleLocator::Bluray { playlist_number: 1 },
                &too_many,
            )
            .is_err());
        }

        #[test]
        fn fingerprint_changes_with_navigation_bytes_and_never_reads_video() {
            let directory = tempfile::tempdir().expect("tempdir");
            let bdmv = directory.path().join("BDMV");
            fs::create_dir_all(bdmv.join("PLAYLIST")).expect("playlist directory");
            fs::create_dir_all(bdmv.join("STREAM")).expect("stream directory");
            fs::write(bdmv.join("index.bdmv"), b"index-a").expect("index");
            fs::write(bdmv.join("PLAYLIST/00001.mpls"), b"playlist-a").expect("playlist");
            let mut video = File::create(bdmv.join("STREAM/00001.m2ts")).expect("video");
            video.write_all(&vec![9_u8; 1024 * 1024]).expect("video");
            let first = fingerprint(OpticalFormat::Bluray, directory.path(), &bdmv)
                .expect("first fingerprint");
            fs::write(bdmv.join("STREAM/00001.m2ts"), b"different video").expect("change video");
            let video_only = fingerprint(OpticalFormat::Bluray, directory.path(), &bdmv)
                .expect("video-only fingerprint");
            assert_eq!(first.digest, video_only.digest);
            fs::write(bdmv.join("PLAYLIST/00001.mpls"), b"playlist-b").expect("change playlist");
            let changed = fingerprint(OpticalFormat::Bluray, directory.path(), &bdmv)
                .expect("changed fingerprint");
            assert_ne!(first.digest, changed.digest);
            assert!(first.bounded_sample_bytes <= first.navigation_bytes.saturating_mul(2));
        }

        #[test]
        fn sampled_navigation_fingerprint_is_not_complete_identity() {
            let directory = tempfile::tempdir().expect("tempdir");
            let bdmv = directory.path().join("BDMV");
            fs::create_dir_all(bdmv.join("PLAYLIST")).expect("playlist directory");
            let playlist =
                File::create(bdmv.join("PLAYLIST/00001.mpls")).expect("large sparse playlist");
            playlist
                .set_len(MAX_NAVIGATION_BYTES + 1)
                .expect("sparse playlist length");

            let evidence =
                fingerprint(OpticalFormat::Bluray, directory.path(), &bdmv).expect("fingerprint");
            assert!(!evidence.complete);
            assert!(evidence.bounded_sample_bytes < evidence.navigation_bytes);
            assert!(evidence.bounded_sample_bytes <= MAX_NAVIGATION_BYTES);
        }

        #[test]
        fn navigation_walk_rejects_excessive_directory_depth() {
            let directory = tempfile::tempdir().expect("tempdir");
            let root = directory.path().join("BDMV");
            fs::create_dir(&root).expect("navigation root");
            let mut nested = root.clone();
            for index in 0..=MAX_NAVIGATION_DEPTH {
                nested = nested.join(format!("level-{index}"));
                fs::create_dir(&nested).expect("nested navigation directory");
            }

            assert!(navigation_files(OpticalFormat::Bluray, &root).is_err());
        }

        #[test]
        fn navigation_root_discovery_rejects_ambiguous_case_aliases() {
            let directory = tempfile::tempdir().expect("tempdir");
            fs::create_dir(directory.path().join("BDMV")).expect("first root");
            fs::create_dir(directory.path().join("bdmv")).expect("case alias");
            assert!(find_case_path(directory.path(), "BDMV", true).is_err());
        }

        #[test]
        fn production_mount_binding_rejects_a_regular_device_path() {
            let directory = tempfile::tempdir().expect("tempdir");
            let device = directory.path().join("not-a-device");
            let mount = directory.path().join("mount");
            fs::write(&device, []).expect("regular device decoy");
            fs::create_dir(&mount).expect("mount directory");
            assert!(require_mount_matches_device(&device, &mount).is_err());
        }
    }
}
