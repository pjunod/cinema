use std::ffi::OsString;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Backend locator retained only on the trusted drive owner. Public title IDs
/// resolve to one of these after generation and authorization checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "format", rename_all = "snake_case", deny_unknown_fields)]
pub enum OpticalTitleLocator {
    Dvd { title_number: u32 },
    Bluray { playlist_number: u32 },
}

/// A checked local input ready to be lowered into one process invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedInput {
    File {
        path: PathBuf,
    },
    Dvd {
        path: PathBuf,
        title_number: u32,
        angle: u32,
    },
    Bluray {
        path: PathBuf,
        playlist_number: u32,
        angle: u32,
    },
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum InputBuildError {
    #[error("input path is empty")]
    EmptyPath,
    #[error("optical title or playlist number must be at least one")]
    Selection,
    #[error("optical angle must be at least one")]
    Angle,
    #[error("Blu-ray paths must be valid Unicode for the bluray protocol URL")]
    BlurayPathEncoding,
}

impl ResolvedInput {
    pub fn path(&self) -> &std::path::Path {
        match self {
            Self::File { path } | Self::Dvd { path, .. } | Self::Bluray { path, .. } => path,
        }
    }

    pub fn validate(&self) -> Result<(), InputBuildError> {
        let (path, selection, angle) = match self {
            Self::File { path } => {
                if path.as_os_str().is_empty() {
                    return Err(InputBuildError::EmptyPath);
                }
                return Ok(());
            }
            Self::Dvd {
                path,
                title_number,
                angle,
            } => (path, *title_number, *angle),
            Self::Bluray {
                path,
                playlist_number,
                angle,
            } => (path, *playlist_number, *angle),
        };
        if path.as_os_str().is_empty() {
            return Err(InputBuildError::EmptyPath);
        }
        if selection == 0 {
            return Err(InputBuildError::Selection);
        }
        if angle == 0 {
            return Err(InputBuildError::Angle);
        }
        if matches!(self, Self::Bluray { .. }) && path.to_str().is_none() {
            return Err(InputBuildError::BlurayPathEncoding);
        }
        Ok(())
    }

    /// Append this input's complete FFmpeg/FFprobe input clause.
    ///
    /// Input-private options are emitted immediately before their `-i`. This
    /// method is intentionally append-only so a command with a subtitle or
    /// offset input cannot accidentally apply DVD/Blu-ray selection options to
    /// the wrong source.
    pub fn append_input_args(&self, args: &mut Vec<OsString>) -> Result<(), InputBuildError> {
        self.validate()?;
        match self {
            Self::File { path } => {
                args.push("-i".into());
                args.push(path.as_os_str().to_owned());
            }
            Self::Dvd {
                path,
                title_number,
                angle,
            } => {
                args.extend([
                    "-f".into(),
                    "dvdvideo".into(),
                    "-title".into(),
                    title_number.to_string().into(),
                    "-angle".into(),
                    angle.to_string().into(),
                    "-i".into(),
                    path.as_os_str().to_owned(),
                ]);
            }
            Self::Bluray {
                path,
                playlist_number,
                angle,
            } => {
                let path = path.to_str().ok_or(InputBuildError::BlurayPathEncoding)?;
                args.extend([
                    "-playlist".into(),
                    playlist_number.to_string().into(),
                    "-angle".into(),
                    angle.to_string().into(),
                    "-i".into(),
                    format!("bluray:{path}").into(),
                ]);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn optical_input_options_stay_immediately_before_each_input() {
        let mut args = vec!["-hide_banner".into()];
        ResolvedInput::Dvd {
            path: "/media/Disc One".into(),
            title_number: 3,
            angle: 2,
        }
        .append_input_args(&mut args)
        .expect("dvd input");
        ResolvedInput::Bluray {
            path: "/media/Disc Two".into(),
            playlist_number: 800,
            angle: 1,
        }
        .append_input_args(&mut args)
        .expect("bluray input");

        assert_eq!(
            strings(&args),
            vec![
                "-hide_banner",
                "-f",
                "dvdvideo",
                "-title",
                "3",
                "-angle",
                "2",
                "-i",
                "/media/Disc One",
                "-playlist",
                "800",
                "-angle",
                "1",
                "-i",
                "bluray:/media/Disc Two",
            ]
        );
    }

    #[test]
    fn optical_zero_selection_and_angle_are_not_backend_defaults() {
        let invalid = ResolvedInput::Dvd {
            path: "/dev/sr0".into(),
            title_number: 0,
            angle: 1,
        };
        assert_eq!(invalid.validate(), Err(InputBuildError::Selection));

        let invalid = ResolvedInput::Bluray {
            path: "/mnt/disc".into(),
            playlist_number: 1,
            angle: 0,
        };
        assert_eq!(invalid.validate(), Err(InputBuildError::Angle));
    }
}
