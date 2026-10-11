use dolby_vision::rpu::{
    dovi_rpu::DoviRpu,
    extension_metadata::blocks::{ExtMetadataBlock, ExtMetadataBlockLevel1},
    generate::GenerateConfig,
};
use std::{fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(std::env::args().nth(1).ok_or("output directory required")?);
    fs::create_dir(&root)?;
    for index in 0..6u16 {
        let mut config = GenerateConfig::default();
        config.source_min_pq = Some(index);
        config.source_max_pq = Some(4095);
        config.default_metadata_blocks.push(ExtMetadataBlock::Level1(
            ExtMetadataBlockLevel1::new(0, 4095 - index, 2048 - index),
        ));
        let mut rpu = DoviRpu::profile81_config(&config)?;
        if index >= 3 {
            let curve = rpu.rpu_data_mapping.as_mut().ok_or("missing mapping")?
                .curves[0].polynomial.as_mut().ok_or("missing polynomial")?;
            curve.poly_coef_int[0][0] = 0;
            curve.poly_coef_int[0][1] = 0;
            curve.poly_coef[0][0] = 1 << 19;
            curve.poly_coef[0][1] = 3 << 21;
            rpu.modified = true;
        }
        let nal = rpu.write_hevc_unspec62_nalu()?;
        let parsed = DoviRpu::parse_unspec62_nalu(&nal)?;
        fs::write(root.join(format!("p8-frame{index}.nal")), &nal)?;
        fs::write(root.join(format!("p8-frame{index}.json")),
                  serde_json::to_string_pretty(&parsed)?)?;
    }
    Ok(())
}
