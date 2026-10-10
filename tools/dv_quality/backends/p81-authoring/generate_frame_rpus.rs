use std::{fs, path::PathBuf};
use dolby_vision::rpu::{
    dovi_rpu::DoviRpu,
    extension_metadata::blocks::{ExtMetadataBlock, ExtMetadataBlockLevel1},
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(std::env::args().nth(1).ok_or("scratch path required")?);
    let original = fs::read(root.join("adapted-rpu-one.nal"))?;
    let baseline = DoviRpu::parse_unspec62_nalu(&original)?;
    for frame in 0..4u16 {
        let mut rpu = baseline.clone();
        rpu.vdr_dm_data.as_mut().ok_or("DM required")?.replace_metadata_block(
            ExtMetadataBlock::Level1(ExtMetadataBlockLevel1::new(0, 4095-frame, 2048-frame)),
        )?;
        rpu.modified = true;
        let data = rpu.write_hevc_unspec62_nalu()?;
        let parsed = DoviRpu::parse_unspec62_nalu(&data)?;
        fs::write(root.join(format!("adapted-rpu-frame{frame}.json")), serde_json::to_string_pretty(&parsed)?)?;
        if frame == 0 && data != original {
            return Err("frame0 metadata changed".into());
        }
        fs::write(root.join(format!("adapted-rpu-frame{frame}.nal")), data)?;
    }
    Ok(())
}
