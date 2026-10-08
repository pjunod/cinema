// SPDX-License-Identifier: AGPL-3.0-only
// Build as a private standalone binary against the repository-vendored
// dolby_vision = 3.4.0. Generated metadata contains no borrowed media bytes.
use dolby_vision::rpu::generate::{GenerateConfig, GenerateProfile, VideoShot};
use std::{error::Error, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let output = PathBuf::from(args.next().ok_or("missing new output directory")?);
    let frames: usize = args
        .next()
        .ok_or("missing frame count")?
        .to_str()
        .ok_or("invalid frame count")?
        .parse()?;
    if args.next().is_some() || !output.is_absolute() || frames == 0 || frames > 1440 {
        return Err("usage: generator ABSOLUTE_NEW_DIRECTORY FRAME_COUNT(1..1440)".into());
    }
    fs::create_dir(&output)?;
    let config = GenerateConfig {
        profile: GenerateProfile::Profile5,
        length: frames,
        shots: vec![VideoShot {
            duration: frames,
            ..Default::default()
        }],
        ..Default::default()
    };
    for (index, rpu) in config.generate_rpu_list()?.iter().enumerate() {
        let bytes = rpu.write_hevc_unspec62_nalu()?;
        fs::write(output.join(format!("frame-{index:04}.bin")), bytes)?;
    }
    Ok(())
}
