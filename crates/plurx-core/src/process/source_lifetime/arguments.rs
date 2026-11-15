//! Audited argument grammars, not source-confinement capabilities.
use super::descriptors::InheritedFd;
#[cfg(unix)]
use std::fs::File;
use std::io;

/// Selects scalar/device operands in a caller-audited command protocol.
/// Every nondefault profile remains ineligible for a clean recovery witness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ArgumentProfile {
    #[default]
    Default,
    /// The twenty-argument bounded segment_decode_render window protocol.
    FelRendererWindowV1,
    /// FFmpeg option/value syntax; DRM access requires one pinned render FD.
    Ffmpeg { drm_device: Option<i32> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ArgumentRole {
    Ordinary,
    Scalar,
    DrmDevice,
}

impl ArgumentProfile {
    pub(super) fn drm_device(self) -> Option<i32> {
        match self {
            Self::Ffmpeg { drm_device } => drm_device,
            _ => None,
        }
    }

    pub(super) fn validate_descriptors(
        self,
        inherited: &[InheritedFd],
        cwd_target: Option<i32>,
    ) -> io::Result<()> {
        let Some(target) = self.drm_device() else {
            return Ok(());
        };
        #[cfg(unix)]
        if target > 2
            && cwd_target != Some(target)
            && inherited
                .iter()
                .any(|entry| entry.target() == target && drm_render_file(entry.file()))
        {
            return Ok(());
        }
        #[cfg(not(unix))]
        let _ = (target, inherited, cwd_target);
        Err(io::Error::other(
            "DRM role requires the exact pinned render-node descriptor",
        ))
    }

    pub(super) fn argument_roles(self, args: &[&str]) -> Result<Vec<ArgumentRole>, &'static str> {
        if self == Self::Default {
            return Ok(Vec::new());
        }
        let mut roles = vec![ArgumentRole::Ordinary; args.len()];
        match self {
            Self::Default => unreachable!(),
            Self::FelRendererWindowV1 => {
                if args.len() != 20
                    || args[9] != "bt2020-pq-master-clip"
                    || !matches!(args[14], "fel" | "base-rpu")
                    || !matches!(
                        args[17],
                        "pq-bilinear-rgb48-v1" | "pq-bilinear-yuv420p10-v1"
                    )
                    || args[18] != "--held-source-origin"
                {
                    return Err("renderer_argument_profile");
                }
                let number = |index: usize, min: u32, max: u32| -> Result<u32, &'static str> {
                    args[index]
                        .parse::<u32>()
                        .ok()
                        .filter(|n| (min..=max).contains(n))
                        .ok_or("renderer_argument_profile")
                };
                number(2, 0, 255)?;
                number(3, 1, 64)?;
                let w = number(4, 64, 4096)?;
                let h = number(5, 64, 2160)?;
                let ew = number(6, 0, 4096)?;
                let eh = number(7, 0, 2160)?;
                number(8, 0, 1)?;
                number(13, 0, 512)?;
                let outw = number(15, 32, w)?;
                let outh = number(16, 32, h)?;
                if [w, h, ew, eh, outw, outh].iter().any(|n| n % 2 != 0)
                    || (ew == 0) != (eh == 0)
                    || (args[14] == "base-rpu" && ew != 0)
                {
                    return Err("renderer_argument_profile");
                }
                let start = window_rational(args[11])?;
                let end = window_rational(args[12])?;
                window_rational(args[19])?;
                let delta = i128::from(end.0) * i128::from(start.1)
                    - i128::from(start.0) * i128::from(end.1);
                if delta <= 0 || delta > 3 * i128::from(start.1) * i128::from(end.1) {
                    return Err("renderer_window_bounds");
                }
                for index in [11, 12, 19] {
                    roles[index] = ArgumentRole::Scalar;
                }
            }
            Self::Ffmpeg { drm_device } => {
                let mut index = 0;
                let mut device_seen = false;
                while index < args.len() {
                    let option = args[index];
                    if option == "--" {
                        break;
                    }
                    if matches!(option, "-r" | "-framerate") {
                        let value = args.get(index + 1).ok_or("missing_rate_operand")?;
                        if !frame_rate(value) {
                            return Err("invalid_rate_operand");
                        }
                        roles[index + 1] = ArgumentRole::Scalar;
                        index += 2;
                    } else if option == "-vaapi_device" {
                        let target = drm_device.ok_or("unpinned_drm_device")?;
                        let value = args.get(index + 1).ok_or("missing_drm_device_operand")?;
                        if device_seen
                            || (*value != format!("/proc/self/fd/{target}")
                                && *value != format!("/dev/fd/{target}"))
                        {
                            return Err("drm_device_operand");
                        }
                        roles[index + 1] = ArgumentRole::DrmDevice;
                        device_seen = true;
                        index += 2;
                    } else if option.starts_with('-') && !ffmpeg_flag(option) {
                        // FFmpeg value options consume one argv element, even
                        // when it resembles another option. Unknown zero-value
                        // options stay conservative rather than invent roles.
                        index += 2;
                    } else {
                        index += 1;
                    }
                }
                if drm_device.is_some() && !device_seen {
                    return Err("unused_drm_device_role");
                }
            }
        }
        Ok(roles)
    }
}

fn window_rational(value: &str) -> Result<(i64, i32), &'static str> {
    let (num, den) = value.split_once('/').ok_or("renderer_window_rational")?;
    let num = num
        .parse::<i64>()
        .ok()
        .filter(|n| *n >= 0)
        .ok_or("renderer_window_rational")?;
    let den = den
        .parse::<i32>()
        .ok()
        .filter(|d| *d > 0)
        .ok_or("renderer_window_rational")?;
    Ok((num, den))
}

fn frame_rate(value: &str) -> bool {
    if matches!(
        value,
        "ntsc" | "pal" | "qntsc" | "qpal" | "sntsc" | "spal" | "film" | "ntsc-film"
    ) {
        return true;
    }
    if let Some((num, den)) = value.split_once('/') {
        return num.parse::<i32>().is_ok_and(|n| n > 0) && den.parse::<i32>().is_ok_and(|d| d > 0);
    }
    value
        .bytes()
        .all(|c| c.is_ascii_digit() || b".+-eE".contains(&c))
        && value.parse::<f64>().is_ok_and(|n| n.is_finite() && n > 0.0)
}

fn ffmpeg_flag(value: &str) -> bool {
    matches!(
        value,
        "-hide_banner"
            | "-nostdin"
            | "-stdin"
            | "-y"
            | "-n"
            | "-an"
            | "-vn"
            | "-sn"
            | "-dn"
            | "-stats"
            | "-nostats"
            | "-copyts"
            | "-start_at_zero"
            | "-shortest"
            | "-re"
            | "-bitexact"
            | "-benchmark"
            | "-benchmark_all"
            | "-accurate_seek"
            | "-noaccurate_seek"
    )
}

#[cfg(target_os = "linux")]
fn drm_render_file(file: &File) -> bool {
    use std::{
        io::Read,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::fs::{FileTypeExt, MetadataExt},
        },
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if !metadata.file_type().is_char_device() || libc::major(metadata.rdev()) != 226 {
        return false;
    }
    let minor = libc::minor(metadata.rdev());
    let name = format!("renderD{minor}");
    let Ok(named) = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())) else {
        return false;
    };
    if named.file_name().is_none_or(|n| n != name.as_str()) {
        return false;
    }
    let Ok(current) = std::fs::symlink_metadata(&named) else {
        return false;
    };
    if !current.file_type().is_char_device()
        || current.dev() != metadata.dev()
        || current.ino() != metadata.ino()
        || current.rdev() != metadata.rdev()
    {
        return false;
    }
    // Extended DRM minor numbers can identify primary or render nodes. Check
    // the actual kernel class and pinned device inode, not a numeric range.
    let Ok(class) = std::fs::canonicalize(format!("/sys/dev/char/226:{minor}")) else {
        return false;
    };
    if class.file_name().is_none_or(|n| n != name.as_str()) {
        return false;
    }
    let Ok(directory) = File::open(class) else {
        return false;
    };
    // SAFETY: statfs is a plain C output buffer; it is read only after success.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: directory is a live descriptor and stat is writable storage.
    if unsafe { libc::fstatfs(directory.as_raw_fd(), &mut stat) } != 0 || stat.f_type != 0x6265_6572
    {
        return false;
    }
    // SAFETY: the kernel-owned sysfs directory is pinned; the literal name is
    // NUL-terminated and the returned descriptor is immediately owned.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c"dev".as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return false;
    }
    let mut text = String::new();
    // SAFETY: openat succeeded and this is the sole owner of its new descriptor.
    let attribute = unsafe { File::from_raw_fd(fd) };
    attribute.take(65).read_to_string(&mut text).is_ok() && text.trim() == format!("226:{minor}")
}
#[cfg(all(unix, not(target_os = "linux")))]
fn drm_render_file(_file: &File) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> Vec<&'static str> {
        vec![
            "http://127.0.0.1:12345/source",
            "/proc/self/fd/4",
            "0",
            "3",
            "1920",
            "1080",
            "0",
            "0",
            "0",
            "bt2020-pq-master-clip",
            "/proc/self/fd/5",
            "0/1",
            "1/1",
            "512",
            "fel",
            "1920",
            "1080",
            "pq-bilinear-rgb48-v1",
            "--held-source-origin",
            "0/1",
        ]
    }

    #[test]
    fn renderer_scalars_match_the_bounded_native_protocol() {
        let profile = ArgumentProfile::FelRendererWindowV1;
        let args = window();
        let roles = profile
            .argument_roles(&args)
            .expect("valid native protocol");
        assert_eq!(
            roles.iter().filter(|r| **r == ArgumentRole::Scalar).count(),
            3
        );
        for (index, value) in [
            (11, "0/0"),
            (11, "-1/1"),
            (12, "4/1"),
            (19, "0/2147483648"),
            (11, "0/../../media"),
            (3, "65"),
            (15, "1921"),
            (14, "unknown"),
            (17, "movie=source.mkv"),
        ] {
            let mut bad = args.clone();
            bad[index] = value;
            assert!(
                profile.argument_roles(&bad).is_err(),
                "slot {index} accepted invalid operand"
            );
        }
        assert!(ArgumentProfile::Default
            .argument_roles(&args)
            .expect("generic")
            .is_empty());
    }

    #[test]
    fn ffmpeg_rate_roles_do_not_reclassify_input_output_or_other_option_values() {
        let profile = ArgumentProfile::Ffmpeg { drm_device: None };
        let args = [
            "-hide_banner",
            "-r",
            "24000/1001",
            "-i",
            "0/1",
            "-metadata",
            "-r",
            "0/1",
        ];
        let roles = profile.argument_roles(&args).expect("rate protocol");
        assert_eq!(roles[2], ArgumentRole::Scalar);
        assert_eq!(roles[4], ArgumentRole::Ordinary);
        assert_eq!(roles[7], ArgumentRole::Ordinary);
        assert_eq!(
            profile
                .argument_roles(&["-i", "-r", "0/1"])
                .expect("input named -r")[2],
            ArgumentRole::Ordinary
        );
        assert_eq!(
            profile
                .argument_roles(&["--", "-r", "0/1"])
                .expect("end of options")[2],
            ArgumentRole::Ordinary
        );
        for rate in ["film", "23.976", "30000/1001"] {
            assert_eq!(
                profile.argument_roles(&["-framerate", rate]).expect("rate")[1],
                ArgumentRole::Scalar
            );
        }
        for rate in [
            "0/1",
            "1/0",
            "1/../../media",
            "http://127.0.0.1/unregistered",
        ] {
            assert!(profile.argument_roles(&["-r", rate]).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn drm_role_rejects_regular_files_generic_characters_and_wrong_operands() {
        use super::super::descriptors::{DescriptorManifest, StdioFacts};
        let regular = tempfile::tempfile().expect("regular file");
        for file in [
            &regular,
            &File::open("/dev/null").expect("ordinary character device"),
        ] {
            let entry = InheritedFd::duplicate(file, 7).expect("pin");
            assert!(DescriptorManifest::new_with_profile(
                vec![entry],
                StdioFacts::CAPTURED,
                None,
                ArgumentProfile::Ffmpeg {
                    drm_device: Some(7)
                }
            )
            .is_err());
        }
        let profile = ArgumentProfile::Ffmpeg {
            drm_device: Some(7),
        };
        assert_eq!(
            profile
                .argument_roles(&["-vaapi_device", "/proc/self/fd/7"])
                .expect("typed operand")[1],
            ArgumentRole::DrmDevice
        );
        assert!(profile
            .argument_roles(&["-vaapi_device", "/dev/dri/renderD128"])
            .is_err());
        assert!(profile
            .argument_roles(&["-vaapi_device", "/proc/self/fd/8"])
            .is_err());
        assert!(profile.argument_roles(&["-i", "/proc/self/fd/7"]).is_err());
        assert!(ArgumentProfile::Ffmpeg { drm_device: None }
            .argument_roles(&["-vaapi_device", "/proc/self/fd/7"])
            .is_err());
    }
}
