use dolby_vision::rpu::{
    dovi_rpu::DoviRpu,
    extension_metadata::blocks::{ExtMetadataBlock, ExtMetadataBlockLevel1},
    generate::{GenerateConfig, GenerateProfile},
    profiles::{profile5::Profile5, DoviProfile},
    rpu_data_mapping::{DoviMMRCurve, DoviMappingMethod},
};
use std::{fs, path::Path};
pub fn generate(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    for name in [
        "p5",
        "p5-mmr1",
        "p5-mmr3",
        "p8-affine",
        "p8-piecewise",
        "p8-eotf",
    ] {
        let dir = root.join(name).join("rpus");
        fs::create_dir_all(&dir)?;
        for frame in 0..18 {
            let mut rpu = DoviRpu::parse_unspec62_nalu(&fs::read(
                root.join("tags").join(format!("p7-frame{}.nal", frame % 6)),
            )?)?;
            rpu.convert_with_mode(2)?;
            rpu.remove_mapping();
            if name.starts_with("p5") {
                let c = GenerateConfig {
                    profile: GenerateProfile::Profile5,
                    ..GenerateConfig::default()
                };
                let generated = DoviRpu::profile5_config(&c)?;
                rpu.header = generated.header;
                rpu.dovi_profile = 5;
                // Existing extension blocks are legal synthetic tags; actual P5 matrices/IPT signal come from the library.
                let dm = Profile5::dm_data();
                let dst = rpu.vdr_dm_data.as_mut().unwrap();
                dst.ycc_to_rgb_coef0 = dm.ycc_to_rgb_coef0;
                dst.ycc_to_rgb_coef1 = dm.ycc_to_rgb_coef1;
                dst.ycc_to_rgb_coef2 = dm.ycc_to_rgb_coef2;
                dst.ycc_to_rgb_coef3 = dm.ycc_to_rgb_coef3;
                dst.ycc_to_rgb_coef4 = dm.ycc_to_rgb_coef4;
                dst.ycc_to_rgb_coef5 = dm.ycc_to_rgb_coef5;
                dst.ycc_to_rgb_coef6 = dm.ycc_to_rgb_coef6;
                dst.ycc_to_rgb_coef7 = dm.ycc_to_rgb_coef7;
                dst.ycc_to_rgb_coef8 = dm.ycc_to_rgb_coef8;
                dst.ycc_to_rgb_offset0 = dm.ycc_to_rgb_offset0;
                dst.ycc_to_rgb_offset1 = dm.ycc_to_rgb_offset1;
                dst.ycc_to_rgb_offset2 = dm.ycc_to_rgb_offset2;
                dst.rgb_to_lms_coef0 = dm.rgb_to_lms_coef0;
                dst.rgb_to_lms_coef1 = dm.rgb_to_lms_coef1;
                dst.rgb_to_lms_coef2 = dm.rgb_to_lms_coef2;
                dst.rgb_to_lms_coef3 = dm.rgb_to_lms_coef3;
                dst.rgb_to_lms_coef4 = dm.rgb_to_lms_coef4;
                dst.rgb_to_lms_coef5 = dm.rgb_to_lms_coef5;
                dst.rgb_to_lms_coef6 = dm.rgb_to_lms_coef6;
                dst.rgb_to_lms_coef7 = dm.rgb_to_lms_coef7;
                dst.rgb_to_lms_coef8 = dm.rgb_to_lms_coef8;
                dst.signal_color_space = 2;
            }
            if name.contains("mmr") {
                for c in 1..3 {
                    let curve = &mut rpu.rpu_data_mapping.as_mut().unwrap().curves[c];
                    curve.mapping_idc = DoviMappingMethod::MMR;
                    curve.polynomial = None;
                    let order = if name.ends_with('3') { 3 } else { 1 };
                    let mut mmr = DoviMMRCurve {
                        mmr_order_minus1: vec![order as u8 - 1],
                        mmr_constant_int: vec![0],
                        mmr_constant: vec![1 << 21],
                        mmr_coef_int: vec![Default::default()],
                        mmr_coef: vec![Default::default()],
                    };
                    for j in 0..order {
                        mmr.mmr_coef_int[0].push([0_i64; 7].into_iter().collect());
                        mmr.mmr_coef[0].push(
                            (0..7)
                                .map(|k| {
                                    if k == c {
                                        if j == 0 {
                                            1_u64 << 22
                                        } else if j == 1 {
                                            1 << 18
                                        } else {
                                            1 << 17
                                        }
                                    } else {
                                        0
                                    }
                                })
                                .collect(),
                        );
                    }
                    curve.mmr = Some(mmr);
                }
            }
            if name == "p8-affine" || name == "p8-piecewise" {
                let curve = &mut rpu.rpu_data_mapping.as_mut().unwrap().curves[0];
                let poly = curve.polynomial.as_mut().unwrap();
                poly.poly_coef_int[0][1] = 0;
                poly.poly_coef[0][0] = 1 << 20;
                poly.poly_coef[0][1] = 3 << 21;
                if name == "p8-piecewise" {
                    curve.num_pivots_minus2 = 1;
                    // Bitstream pivots are deltas; FFmpeg projects [0,512,1023].
                    curve.pivots = vec![0, 512, 511];
                    poly.poly_order_minus1.push(0);
                    poly.linear_interp_flag.push(false);
                    poly.poly_coef_int.push([0_i64, 0].into_iter().collect());
                    poly.poly_coef
                        .push([1_u64 << 21, 1 << 22].into_iter().collect());
                }
            }
            if name == "p8-eotf" {
                rpu.vdr_dm_data.as_mut().unwrap().signal_eotf_param0 = 1;
            }
            rpu.vdr_dm_data
                .as_mut()
                .unwrap()
                .replace_metadata_block(ExtMetadataBlock::Level1(ExtMetadataBlockLevel1 {
                    min_pq: 0,
                    max_pq: 4095 - frame as u16,
                    avg_pq: 2048 - frame as u16,
                }))?;
            rpu.modified = true;
            let bytes = rpu.write_hevc_unspec62_nalu()?;
            let actual = DoviRpu::parse_unspec62_nalu(&bytes)?;
            fs::write(dir.join(format!("p7-frame{frame}.nal")), &bytes)?;
            fs::write(
                dir.join(format!("p7-frame{frame}.json")),
                serde_json::to_vec_pretty(&actual)?,
            )?;
        }
    }
    Ok(())
}
