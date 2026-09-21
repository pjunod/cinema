//! Bounded Linux helper for observing, inspecting and ejecting optical media.
//!
//! The daemon supplies only trusted startup-config paths. This process keeps
//! filesystem parsing and device ioctls outside the long-lived server and
//! emits one versioned JSON reply on standard output.

#[cfg(any(target_os = "linux", feature = "linux-host-check"))]
fn main() {
    linux::run_main();
}

#[cfg(all(not(target_os = "linux"), not(feature = "linux-host-check")))]
fn main() {
    eprintln!("plurx-optical-helper is supported on Linux only");
    std::process::exit(1);
}

#[cfg(any(target_os = "linux", feature = "linux-host-check"))]
mod linux {

    use std::collections::BTreeSet;
    use std::fs::{self, File};
    use std::io::{Read, Seek, SeekFrom};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    use clap::{Parser, Subcommand};
    use plurx_core::domain::{AudioStream, DolbyVisionFacts, SubtitleStream};
    use plurx_core::optical::{
        FingerprintEvidence, InspectedChapter, InspectedDisc, InspectedStream, InspectedTitle,
        InspectionResponse, OpticalFormat, OpticalTitleLocator, ProtectionFacts, ResolvedInput,
        INSPECTION_SCHEMA_V1,
    };
    use plurx_core::playback::{PlaybackMediaFacts, SourceDelivery};
    use serde::Serialize;
    use serde_json::Value;
    use sha2::{Digest, Sha256};

    const MAX_NAVIGATION_FILES: usize = 4096;
    const MAX_NAVIGATION_BYTES: u64 = 16 * 1024 * 1024;
    const SAMPLE_BYTES: u64 = 64 * 1024;
    const MAX_TITLES: usize = 512;
    const MAX_PROBE_BYTES: usize = 4 * 1024 * 1024;

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

    pub(super) fn run_main() {
        if let Err(error) = run(Args::parse()) {
            eprintln!("{error}");
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
        if mount_has_media(paths.mount.as_deref()) {
            return Ok("present");
        }
        if !paths.device.exists() {
            return Ok("unknown");
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&paths.device)
            .map_err(|error| format!("cannot open optical device: {error}"))?;
        const CDROM_DRIVE_STATUS: libc::c_ulong = 0x5326;
        const CDSL_CURRENT: libc::c_int = i32::MAX;
        let status = unsafe {
            libc::ioctl(
                std::os::fd::AsRawFd::as_raw_fd(&file),
                CDROM_DRIVE_STATUS,
                CDSL_CURRENT,
            )
        };
        Ok(match status {
            4 => "present",
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
        let mut paths = args.paths;
        paths.mount = Some(mount.clone());
        let (format, navigation_root, locators) =
            if let Some(root) = find_case_dir(&mount, "VIDEO_TS") {
                reject_symlink(&root)?;
                let ifo = find_case_file(&root, "VIDEO_TS.IFO")
                    .ok_or_else(|| "DVD VIDEO_TS.IFO is missing".to_owned())?;
                reject_symlink(&ifo)?;
                let count = dvd_title_count(&ifo)?;
                let locators = (1..=count.min(MAX_TITLES as u32))
                    .map(|title_number| OpticalTitleLocator::Dvd { title_number })
                    .collect();
                (OpticalFormat::Dvd, root, locators)
            } else if let Some(root) = find_case_dir(&mount, "BDMV") {
                reject_symlink(&root)?;
                let locators = bluray_playlists(&root)?;
                (OpticalFormat::Bluray, root, locators)
            } else {
                return Err("mounted media is not DVD-Video or Blu-ray".into());
            };
        if locators.is_empty() {
            return Err("no playable optical titles were found".into());
        }
        let fingerprint = fingerprint(format, &mount, &navigation_root)?;
        let ffprobe = std::env::var_os("PLURX_FFPROBE").unwrap_or_else(|| "ffprobe".into());
        let mut titles = Vec::new();
        let mut diagnostics = Vec::new();
        for locator in locators {
            match probe_title(&ffprobe, format, &paths, locator) {
                Ok(title) => titles.push(title),
                Err(error) => diagnostics.push(error),
            }
        }
        if titles.is_empty() {
            return Err(format!(
                "no title could be probed: {}",
                diagnostics.join("; ")
            ));
        }
        let volume_label = mount
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        Ok(InspectionResponse {
            schema_version: INSPECTION_SCHEMA_V1,
            expected_generation: args.expected_generation,
            disc: InspectedDisc {
                format,
                volume_label,
                protection: ProtectionFacts {
                    detected: None,
                    implementation_available: None,
                    handled: None,
                    scheme: None,
                },
                fingerprint,
                titles,
            },
            diagnostics: diagnostics.join("; ").chars().take(16 * 1024).collect(),
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

    fn find_case_dir(root: &Path, wanted: &str) -> Option<PathBuf> {
        fs::read_dir(root).ok()?.flatten().find_map(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(wanted)
                .then(|| entry.path())
        })
    }

    fn find_case_file(root: &Path, wanted: &str) -> Option<PathBuf> {
        fs::read_dir(root).ok()?.flatten().find_map(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(wanted)
                .then(|| entry.path())
        })
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
        let playlist = find_case_dir(bdmv, "PLAYLIST")
            .ok_or_else(|| "Blu-ray PLAYLIST directory is missing".to_owned())?;
        reject_symlink(&playlist)?;
        let mut numbers = BTreeSet::new();
        for entry in fs::read_dir(playlist)
            .map_err(|error| error.to_string())?
            .flatten()
        {
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
                .filter(|number| *number > 0)
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
        let mut stack = vec![root.to_owned()];
        while let Some(directory) = stack.pop() {
            for entry in fs::read_dir(&directory)
                .map_err(|error| error.to_string())?
                .flatten()
            {
                let path = entry.path();
                let metadata = fs::symlink_metadata(&path)
                    .map_err(|error| format!("cannot inspect optical path: {error}"))?;
                if metadata.file_type().is_symlink() {
                    return Err("optical navigation paths must not contain symlinks".into());
                }
                if metadata.is_dir() {
                    stack.push(path);
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
            hash.update(relative.as_os_str().as_encoded_bytes());
            hash.update(metadata.len().to_be_bytes());
            navigation_bytes = navigation_bytes.saturating_add(metadata.len());
            if navigation_bytes > MAX_NAVIGATION_BYTES {
                return Err("optical navigation bytes exceed their bound".into());
            }
            let mut file = File::open(&path).map_err(|error| error.to_string())?;
            let take = metadata.len().min(SAMPLE_BYTES);
            let mut prefix = vec![0_u8; take as usize];
            file.read_exact(&mut prefix)
                .map_err(|error| error.to_string())?;
            hash.update(&prefix);
            sampled = sampled.saturating_add(take);
            if metadata.len() > take {
                let suffix_len = (metadata.len() - take).min(SAMPLE_BYTES);
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
            version: 1,
            complete: true,
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
        let output = Command::new(ffprobe)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| format!("ffprobe is unavailable: {error}"))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(512)
                .collect());
        }
        if output.stdout.len() > MAX_PROBE_BYTES {
            return Err("ffprobe title reply exceeded its byte bound".into());
        }
        let document: Value =
            serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
        title_from_probe(format, locator, &document)
    }

    fn string(value: &Value, key: &str) -> Option<String> {
        value.get(key)?.as_str().map(ToOwned::to_owned)
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
            .map(|(index, stream)| AudioStream {
                index: i64::try_from(index).unwrap_or(i64::MAX),
                codec: string(stream, "codec_name").unwrap_or_default(),
                channels: integer(stream, "channels"),
                language: stream.get("tags").and_then(|tags| string(tags, "language")),
                title: stream.get("tags").and_then(|tags| string(tags, "title")),
                default: stream
                    .get("disposition")
                    .and_then(|value| integer(value, "default"))
                    == Some(1),
            })
            .collect::<Vec<_>>();
        let subtitle_streams = streams
            .iter()
            .filter(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("subtitle"))
            .enumerate()
            .map(|(index, stream)| SubtitleStream {
                index: i64::try_from(index).unwrap_or(i64::MAX),
                codec: string(stream, "codec_name").unwrap_or_default(),
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
            .collect::<Vec<_>>();
        let inspected_streams = streams
            .iter()
            .map(|stream| InspectedStream {
                index: integer(stream, "index").unwrap_or_default(),
                kind: string(stream, "codec_type").unwrap_or_else(|| "unknown".into()),
                codec: string(stream, "codec_name"),
                language: stream.get("tags").and_then(|tags| string(tags, "language")),
                title: stream.get("tags").and_then(|tags| string(tags, "title")),
                channels: integer(stream, "channels").and_then(|value| u32::try_from(value).ok()),
                default: stream
                    .get("disposition")
                    .and_then(|value| integer(value, "default"))
                    == Some(1),
                forced: stream
                    .get("disposition")
                    .and_then(|value| integer(value, "forced"))
                    == Some(1),
            })
            .collect();
        let chapters = document
            .get("chapters")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(index, chapter)| InspectedChapter {
                index: u32::try_from(index).unwrap_or(u32::MAX),
                start_ms: seconds_ms(chapter.get("start_time")),
                accurate: true,
            })
            .collect();
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
            fs::write(playlist.join("00800.mpls"), []).expect("playlist");
            fs::write(playlist.join("00001.MPLS"), []).expect("playlist");
            fs::write(playlist.join("00002.m2ts"), []).expect("clip decoy");
            assert_eq!(
                bluray_playlists(&bdmv).expect("playlists"),
                vec![
                    OpticalTitleLocator::Bluray { playlist_number: 1 },
                    OpticalTitleLocator::Bluray {
                        playlist_number: 800
                    },
                ]
            );
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
    }
}
