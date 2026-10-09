use dolby_vision::rpu::{
    dovi_rpu::DoviRpu,
    extension_metadata::blocks::{
        ExtMetadataBlock, ExtMetadataBlockLevel2, ExtMetadataBlockLevel3, ExtMetadataBlockLevel4,
        ExtMetadataBlockLevel5, ExtMetadataBlockLevel8,
    },
};
use std::{fs, path::PathBuf};
mod base_profiles;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(std::env::args().nth(1).ok_or("root")?);
    if std::env::args().nth(2).as_deref() == Some("base") {
        return base_profiles::generate(&root);
    }
    if std::env::args().nth(2).as_deref() == Some("parse") {
        let dir = root.join("adapted");
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) == Some("nal") {
                let r = DoviRpu::parse_unspec62_nalu(&fs::read(&path)?)?;
                fs::write(path.with_extension("json"), serde_json::to_vec_pretty(&r)?)?;
            }
        }
        return Ok(());
    }
    for name in [
        "valid",
        "invalid-area",
        "nonzero-eotf",
        "binding-max",
        "long-l8",
        "plain",
    ] {
        let dir = root.join(name).join("rpus");
        fs::create_dir_all(&dir)?;
        for frame in 0..18 {
            let mut rpu = DoviRpu::parse_unspec62_nalu(&fs::read(
                root.join("tags").join(format!("p7-frame{}.nal", frame % 6)),
            )?)?;
            let nlq = rpu.rpu_data_mapping.as_mut().unwrap().nlq.as_mut().unwrap();
            nlq.nlq_offset = [512; 3];
            nlq.linear_deadzone_slope_int = [0; 3];
            nlq.linear_deadzone_slope = [2048; 3];
            nlq.linear_deadzone_threshold_int = [0; 3];
            nlq.linear_deadzone_threshold = [0; 3];
            nlq.vdr_in_max_int = [0; 3];
            nlq.vdr_in_max = [if name == "binding-max" {
                524288
            } else {
                1048576
            }; 3];
            let dm = rpu.vdr_dm_data.as_mut().unwrap();
            if name != "plain" {
                dm.replace_metadata_block(ExtMetadataBlock::Level2(ExtMetadataBlockLevel2 {
                    target_max_pq: 2081,
                    trim_slope: 2101,
                    trim_offset: 2002,
                    trim_power: 1999,
                    trim_chroma_weight: 2055,
                    trim_saturation_gain: 2077,
                    ms_weight: -1,
                }))?;
                dm.replace_metadata_block(ExtMetadataBlock::Level2(ExtMetadataBlockLevel2 {
                    target_max_pq: 3079,
                    trim_slope: 2155,
                    trim_offset: 2040,
                    trim_power: 2080,
                    trim_chroma_weight: 1990,
                    trim_saturation_gain: 2090,
                    ms_weight: 2048,
                }))?;
                dm.replace_metadata_block(ExtMetadataBlock::Level3(ExtMetadataBlockLevel3 {
                    min_pq_offset: 2048,
                    max_pq_offset: 2048,
                    avg_pq_offset: 1571,
                }))?;
                dm.replace_metadata_block(ExtMetadataBlock::Level8(ExtMetadataBlockLevel8 {
                    length: if name == "long-l8" { 12 } else { 10 },
                    target_display_index: 1,
                    trim_slope: 2100,
                    trim_offset: 2020,
                    trim_power: 2080,
                    trim_chroma_weight: 1980,
                    trim_saturation_gain: 2170,
                    ms_weight: 2000,
                    ..Default::default()
                }))?;
                dm.replace_metadata_block(ExtMetadataBlock::Level4(ExtMetadataBlockLevel4 {
                    anchor_pq: 100 + frame as u16,
                    anchor_power: 500 + frame as u16,
                }))?;
                dm.replace_metadata_block(ExtMetadataBlock::Level5(
                    ExtMetadataBlockLevel5::from_offsets(
                        0,
                        0,
                        if name == "invalid-area" { 32 } else { 4 },
                        if name == "invalid-area" { 32 } else { 4 },
                    ),
                ))?;
            }
            if name == "nonzero-eotf" {
                rpu.vdr_dm_data.as_mut().unwrap().signal_eotf_param0 = 1;
            }
            rpu.modified = true;
            let bytes = rpu.write_hevc_unspec62_nalu()?;
            let parsed = DoviRpu::parse_unspec62_nalu(&bytes)?;
            fs::write(dir.join(format!("p7-frame{frame}.nal")), &bytes)?;
            fs::write(
                dir.join(format!("p7-frame{frame}.json")),
                serde_json::to_vec_pretty(&parsed)?,
            )?;
        }
    }
    Ok(())
}
