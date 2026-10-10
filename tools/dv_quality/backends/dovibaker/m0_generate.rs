use std::{fs, path::PathBuf};
use dolby_vision::rpu::{
    dovi_rpu::DoviRpu, generate::GenerateConfig,
    extension_metadata::blocks::{ExtMetadataBlock, ExtMetadataBlockLevel1},
    rpu_data_mapping::DoviNlqMethod,
    rpu_data_nlq::{DoviELType, RpuDataNlq}, ConversionMode,
};
fn write(name: &str, rpu: &DoviRpu, out: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let nal = rpu.write_hevc_unspec62_nalu()?;
    let parsed = DoviRpu::parse_unspec62_nalu(&nal)?;
    fs::write(out.join(format!("{name}.nal")), &nal)?;
    let mut annexb = vec![0, 0, 0, 1];
    annexb.extend_from_slice(&nal[2..]);
    fs::write(out.join(format!("{name}.bin")), annexb)?;
    fs::write(out.join(format!("{name}.json")), serde_json::to_string_pretty(&parsed)?)?;
    println!("{name}: profile={} EL={:?} bytes={}", parsed.dovi_profile, parsed.el_type, nal.len());
    Ok(())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = PathBuf::from(std::env::args().nth(1).unwrap_or("fixtures".into()));
    fs::create_dir_all(&out)?;
    let mut cfg = GenerateConfig::default();
    cfg.source_min_pq = Some(0);
    cfg.source_max_pq = Some(4095);
    cfg.default_metadata_blocks.push(ExtMetadataBlock::Level1(ExtMetadataBlockLevel1::new(0,4095,2048)));
    let baseline = DoviRpu::profile81_config(&cfg)?;
    write("p81_identity", &baseline, &out)?;
    let mut fel = baseline.clone();
    fel.dovi_profile = 7;
    fel.el_type = Some(DoviELType::FEL);
    fel.header.el_spatial_resampling_filter_flag = true;
    fel.header.disable_residual_flag = false;
    let m = fel.rpu_data_mapping.as_mut().unwrap();
    m.nlq_method_idc = Some(DoviNlqMethod::LinearDeadzone);
    m.nlq_num_pivots_minus2 = Some(0);
    m.nlq_pred_pivot_value = Some([0,1023]);
    m.nlq = Some(RpuDataNlq {
        nlq_offset: [512;3], vdr_in_max_int: [1;3], vdr_in_max: [0;3],
        linear_deadzone_slope_int: [0;3], linear_deadzone_slope: [8192;3],
        linear_deadzone_threshold_int: [0;3], linear_deadzone_threshold: [4096;3],
    });
    write("p7_fel_linear", &fel, &out)?;
    let mut matrix = fel.clone();
    matrix.vdr_dm_data.as_mut().unwrap().rgb_to_lms_coef0 /= 2;
    write("p7_fel_nonstandard_linear_matrix", &matrix, &out)?;
    let mut bounded = fel.clone();
    let nlq = bounded.rpu_data_mapping.as_mut().unwrap().nlq.as_mut().unwrap();
    nlq.vdr_in_max_int = [0;3]; nlq.vdr_in_max = [16384;3];
    write("p7_fel_bounded_residual", &bounded, &out)?;
    let mut adapted = fel.clone();
    adapted.convert_with_mode(ConversionMode::To81)?;
    adapted.remove_mapping();
    write("p81_adapted_linear", &adapted, &out)?;
    Ok(())
}
