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
    let mode = match args.next() {
        None => "fresh",
        Some(value) if value == "reuse" => "reuse",
        Some(value) if value == "frame-bound" => "frame-bound",
        Some(value) if value == "missing-color" => "missing-color",
        Some(value) if value == "reuse-missing-color" => "reuse-missing-color",
        _ => return Err("optional third argument must be reuse, frame-bound, missing-color or reuse-missing-color".into()),
    };
    if args.next().is_some() || !output.is_absolute() || frames == 0 || frames > 1440 {
        return Err("usage: generator ABSOLUTE_NEW_DIRECTORY FRAME_COUNT(1..1440)".into());
    }
    if mode == "frame-bound" && frames > 24 {
        return Err("frame-bound experiment is limited to 24 frames".into());
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
        let mut effective = rpu.clone();
        if mode == "frame-bound" {
            let dm = effective
                .vdr_dm_data
                .as_mut()
                .ok_or("missing generated color metadata")?;
            // A unique coded-AU signature makes B-frame metadata association
            // observable without borrowing any mastered Dolby source.
            dm.affected_dm_metadata_id = (index % 16) as u64;
            dm.current_dm_metadata_id = (index % 16) as u64;
            dm.source_max_pq = 3079 + index as u16;
        }
        if (mode == "reuse" || mode == "reuse-missing-color") && index > 0 {
            effective.header.use_prev_vdr_rpu_flag = true;
            effective.header.prev_vdr_rpu_id = 0;
            effective.rpu_data_mapping = None;
        }
        if mode == "missing-color" || (mode == "reuse-missing-color" && index > 0) {
            effective.header.vdr_dm_metadata_present_flag = false;
            effective.vdr_dm_data = None;
        }
        let bytes = effective.write_hevc_unspec62_nalu()?;
        fs::write(output.join(format!("frame-{index:04}.bin")), bytes)?;
    }
    Ok(())
}
