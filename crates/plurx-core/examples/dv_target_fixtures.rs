//! Synthetic control tags: the reconstruction metadata is identical; only L1
//! differs between high/alternate/low. No proprietary metadata is generated.
use dolby_vision::rpu::{
    dovi_rpu::DoviRpu,
    extension_metadata::blocks::{
        ExtMetadataBlock, ExtMetadataBlockLevel1, ExtMetadataBlockLevel6,
    },
};
use std::{fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(std::env::args().nth(1).ok_or("output directory")?);
    fs::create_dir(&root)?;
    let template = include_bytes!("../../../tools/dv_processing/controls/tags/p7-frame0.nal");
    for profile in [7, 8] {
        for family in [
            "high",
            "alternate",
            "low",
            "missing",
            "missing-rpu",
            "conflict",
            "corrupt",
        ] {
            let directory = root.join(format!("p{profile}-{family}"));
            fs::create_dir(&directory)?;
            let mut manifest = Vec::new();
            for frame in 0..18 {
                let mut rpu = DoviRpu::parse_unspec62_nalu(template)?;
                if profile == 8 {
                    rpu.convert_with_mode(2)?;
                    rpu.remove_mapping();
                }
                let dm = rpu.vdr_dm_data.as_mut().ok_or("DM")?;
                let (max_pq, avg_pq) = if family == "low" {
                    (1000, 500)
                } else if family == "alternate" {
                    (4095, 2500 + (frame % 3) * 100)
                } else {
                    (4095, 1800 + (frame % 3) * 100)
                };
                dm.replace_metadata_block(ExtMetadataBlock::Level1(ExtMetadataBlockLevel1 {
                    min_pq: 0,
                    max_pq,
                    avg_pq,
                }))?;
                dm.replace_metadata_block(ExtMetadataBlock::Level6(ExtMetadataBlockLevel6 {
                    min_display_mastering_luminance: 50,
                    max_display_mastering_luminance: if family == "conflict" && frame == 17 {
                        1000
                    } else {
                        4000
                    },
                    // Deliberately below the target: this must NOT elide the high title.
                    max_content_light_level: 1,
                    max_frame_average_light_level: 0,
                }))?;
                if family == "missing" && frame == 17 {
                    dm.remove_metadata_level(1);
                }
                rpu.modified = true;
                let mut bytes = rpu.write_hevc_unspec62_nalu()?;
                DoviRpu::parse_unspec62_nalu(&bytes)?;
                if family == "corrupt" && frame == 17 {
                    // Deliberately damage the parsed payload; the scanner must
                    // refuse or mark missing/invalid, never all-valid.
                    bytes[3] ^= 0x80;
                }
                fs::write(directory.join(format!("p7-frame{frame}.nal")), bytes)?;
                manifest.push(serde_json::json!({"frame":frame, "max_pq":max_pq, "avg_pq":avg_pq}));
            }
            fs::write(
                directory.join("l1.json"),
                serde_json::to_vec_pretty(&manifest)?,
            )?;
        }
    }
    Ok(())
}
