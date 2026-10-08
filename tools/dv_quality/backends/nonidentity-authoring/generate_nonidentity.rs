use std::{fs, path::PathBuf};
use dolby_vision::rpu::{dovi_rpu::DoviRpu, ConversionMode, extension_metadata::blocks::{ExtMetadataBlock, ExtMetadataBlockLevel1, ExtMetadataBlockLevel2}};
fn save(root: &PathBuf, name: &str, rpu: &DoviRpu) -> Result<(), Box<dyn std::error::Error>> {
    let nal = rpu.write_hevc_unspec62_nalu()?;
    let parsed = DoviRpu::parse_unspec62_nalu(&nal)?;
    fs::write(root.join(format!("{name}.nal")), nal)?;
    fs::write(root.join(format!("{name}.json")), serde_json::to_string_pretty(&parsed)?)?;
    Ok(())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(std::env::args().nth(1).ok_or("scratch path required")?);
    let mut source = DoviRpu::parse_unspec62_nalu(&fs::read(root.join("p7-identity-input.nal"))?)?;
    let polynomial = source.rpu_data_mapping.as_mut().unwrap().curves[0].polynomial.as_mut().unwrap();
    polynomial.poly_coef_int[0][0] = 0;
    polynomial.poly_coef_int[0][1] = 0;
    polynomial.poly_coef[0][0] = 1 << 19; // 1/16
    polynomial.poly_coef[0][1] = 3 << 21; // 3/4
    source.modified = true;
    save(&root, "p7-affine-fel", &source)?;
    let mut adapted = source.clone();
    adapted.convert_with_mode(ConversionMode::To81)?;
    adapted.remove_mapping();
    save(&root, "p81-adapted-identity", &adapted)?;
    let mut unsupported = adapted.clone();
    unsupported.vdr_dm_data.as_mut().unwrap().rgb_to_lms_coef0 /= 2;
    unsupported.modified = true;
    save(&root, "p81-unsupported-matrix", &unsupported)?;
    let mut trimmed = adapted.clone();
    trimmed.vdr_dm_data.as_mut().unwrap().add_metadata_block(ExtMetadataBlock::Level2(ExtMetadataBlockLevel2::default()))?;
    trimmed.modified = true;
    save(&root, "p81-unsupported-trim", &trimmed)?;
    let mut doublemapped = adapted.clone();
    doublemapped.rpu_data_mapping.as_mut().unwrap().curves[0] = source.rpu_data_mapping.as_ref().unwrap().curves[0].clone();
    doublemapped.modified = true;
    save(&root, "p81-wrong-repeated-affine", &doublemapped)?;
    for frame in 0..4u16 {
        for (prefix, template) in [("adapted-rpu-frame", &adapted), ("wrong-rpu-frame", &doublemapped)] {
            let mut tagged = template.clone();
            tagged.vdr_dm_data.as_mut().unwrap().replace_metadata_block(
                ExtMetadataBlock::Level1(ExtMetadataBlockLevel1::new(0,4095-frame,2048-frame)),
            )?;
            tagged.modified = true;
            save(&root, &format!("{prefix}{frame}"), &tagged)?;
        }
    }
    Ok(())
}
