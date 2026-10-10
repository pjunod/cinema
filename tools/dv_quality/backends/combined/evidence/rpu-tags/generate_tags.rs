use std::{fs, path::PathBuf};
use dolby_vision::rpu::{dovi_rpu::DoviRpu, extension_metadata::blocks::{ExtMetadataBlock, ExtMetadataBlockLevel1}};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(std::env::args().nth(1).ok_or("scratch path required")?);
    let input = fs::read(root.join("p7-identity-input.nal"))?;
    let original = DoviRpu::parse_unspec62_nalu(&input)?;
    for frame in 0..6u16 {
        let mut tagged = original.clone();
        tagged.vdr_dm_data.as_mut().unwrap().replace_metadata_block(
            ExtMetadataBlock::Level1(ExtMetadataBlockLevel1::new(0,4095-frame,2048-frame)),
        )?;
        tagged.modified = true;
        let nal = tagged.write_hevc_unspec62_nalu()?;
        let parsed = DoviRpu::parse_unspec62_nalu(&nal)?;
        fs::write(root.join(format!("p7-frame{frame}.nal")), nal)?;
        fs::write(root.join(format!("p7-frame{frame}.json")), serde_json::to_string_pretty(&parsed)?)?;
    }
    Ok(())
}
