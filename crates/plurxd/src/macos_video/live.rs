//! Original, hash-bound software-decode controls for native LiveTV upload graphs.
use super::*;

pub(super) const MANIFEST: &[u8] = include_bytes!("../../fixtures/macos-processing/live.json");
pub(super) const MEDIA: &[(&str, &[u8])] = &[
    (
        "live_h264",
        include_bytes!("../../fixtures/macos-processing/live_h264.mp4"),
    ),
    (
        "live_mpeg2",
        include_bytes!("../../fixtures/macos-processing/live_mpeg2.ts"),
    ),
    (
        "live_mpeg2_tff",
        include_bytes!("../../fixtures/macos-processing/live_mpeg2_tff.ts"),
    ),
    (
        "live_mpeg2_bff",
        include_bytes!("../../fixtures/macos-processing/live_mpeg2_bff.ts"),
    ),
    (
        "live_h264_tff",
        include_bytes!("../../fixtures/macos-processing/live_h264_tff.mp4"),
    ),
    (
        "live_h264_bff",
        include_bytes!("../../fixtures/macos-processing/live_h264_bff.mp4"),
    ),
];

pub(super) fn fixtures() -> Result<Vec<ExtensionFixture>, ProbeReason> {
    let invalid = ProbeReason::InvalidEmbeddedCorpus;
    let corpus: ExtensionCorpus = serde_json::from_slice(MANIFEST).map_err(|_| invalid)?;
    let document: Value = serde_json::from_slice(MANIFEST).map_err(|_| invalid)?;
    if corpus.schema_version != 1
        || corpus.generator_recipe_version != 1
        || corpus.fixtures.len() != MEDIA.len()
        || !corpus.auxiliary.is_empty()
    {
        return Err(invalid);
    }
    for (index, (fixture, (id, bytes))) in corpus.fixtures.iter().zip(MEDIA).enumerate() {
        let interlaced = id.ends_with("tff") || id.ends_with("bff");
        let extension = if id.starts_with("live_h264") {
            "mp4"
        } else {
            "ts"
        };
        if fixture.id != *id
            || fixture.path != format!("{id}.{extension}")
            || fixture.sha256 != digest(bytes)
            || fixture.byte_length != bytes.len()
            || bytes.is_empty()
            || fixture.source_class != "sdr"
            || fixture.operation
                != if interlaced {
                    "live_bwdif"
                } else {
                    "live_scale"
                }
        {
            return Err(invalid);
        }
        let facts = &document["fixtures"][index]["input_observed"];
        for (key, expected) in [
            (
                "codec_name",
                if id.starts_with("live_h264") {
                    "h264"
                } else {
                    "mpeg2video"
                },
            ),
            ("pix_fmt", "yuv420p"),
            ("avg_frame_rate", "12/1"),
            ("sample_aspect_ratio", "1:1"),
            ("color_range", "tv"),
            ("color_primaries", "bt709"),
            ("color_transfer", "bt709"),
            ("color_space", "bt709"),
            (
                "field_order",
                if id.ends_with("tff") {
                    "tt"
                } else if id.ends_with("bff") {
                    "bb"
                } else {
                    "progressive"
                },
            ),
        ] {
            if facts[key].as_str() != Some(expected) {
                return Err(invalid);
            }
        }
        validate_contract(&fixture.output_expectation, false, true)?;
    }
    Ok(corpus.fixtures)
}

pub(super) fn add_work(
    corpus: &Corpus,
    work: &mut Vec<(Fixture, SmokeOperation, MacosProcessingGraph)>,
) -> Result<(), ProbeReason> {
    for fixture in corpus
        .fixtures
        .iter()
        .filter(|fixture| fixture.class == "sdr")
    {
        work.push((
            fixture.clone(),
            SmokeOperation::LiveUpload(MacosProcessingGraph::LiveSdrUploadScale),
            MacosProcessingGraph::LiveSdrUploadScale,
        ));
    }
    let live = fixtures()?;
    for extension in &live {
        let mut fixture = corpus.fixtures[0].clone();
        fixture.class = "sdr".into();
        fixture.sha256 = extension.sha256.clone();
        fixture.byte_length = extension.byte_length;
        fixture.output_expectations = vec![extension.output_expectation.clone()];
        let interlaced = extension.operation == "live_bwdif" || extension.operation == "bwdif";
        for graph in if interlaced {
            &[
                MacosProcessingGraph::LiveSdrUploadBwdifFrame,
                MacosProcessingGraph::LiveSdrUploadBwdifField,
            ][..]
        } else {
            &[MacosProcessingGraph::LiveSdrUploadScale][..]
        } {
            let mut output = fixture.clone();
            if *graph == MacosProcessingGraph::LiveSdrUploadBwdifField {
                let contract = &mut output.output_expectations[0];
                contract.expected.frame_count = 24;
                contract.expected.avg_frame_rate = "24/1".into();
                contract.timestamps.step_seconds = 1.0 / 24.0;
            }
            if extension.id.starts_with("live_h264") && interlaced {
                let (operation, ordinary) =
                    if *graph == MacosProcessingGraph::LiveSdrUploadBwdifField {
                        (
                            SmokeOperation::BwdifField,
                            MacosProcessingGraph::SdrBwdifField,
                        )
                    } else {
                        (
                            SmokeOperation::BwdifFrame,
                            MacosProcessingGraph::SdrBwdifFrame,
                        )
                    };
                work.push((output.clone(), operation, ordinary));
            }
            work.push((output, SmokeOperation::LiveUpload(*graph), *graph));
        }
    }
    Ok(())
}

/// The original source moves a white 16x16 marker by 24 pixels per field.
/// Test its interior after native half-size scaling, so a woven/copied wrong
/// field cannot pass merely by retaining gray patches and doubled timestamps.
pub(super) fn observe_motion(raw: &[u8], field: bool) -> Result<(), ProbeReason> {
    let count = if field { 24 } else { 12 };
    let stride = 160 * 90 * 3 / 2;
    if raw.len() != stride * count {
        return Err(ProbeReason::OutputContractFailed);
    }
    for frame in 0..count {
        let source_frame = (if field { frame } else { frame * 2 }) % 12;
        // Exclude source columns whose stationary yellow/cyan/white background
        // is itself bright enough to conceal an absent white marker.
        if !matches!(source_frame, 0..=4 | 8..=10) {
            continue;
        }
        let x = 8 + source_frame * 12;
        let mean = (81..=83)
            .flat_map(|y| (x - 1..=x + 1).map(move |x| (y, x)))
            .map(|(y, x)| f64::from(raw[frame * stride + y * 160 + x]))
            .sum::<f64>()
            / 9.0;
        if mean < 210.0 {
            return Err(ProbeReason::OutputContractFailed);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_wrong_field_motion_cannot_pass_static_gray_and_cadence() {
        let stride = 160 * 90 * 3 / 2;
        let mut raw = vec![16u8; stride * 24];
        for frame in 0..24 {
            let x = 8 + (frame % 12) * 12;
            for y in 81..=83 {
                raw[frame * stride + y * 160 + x - 1..frame * stride + y * 160 + x + 2].fill(235);
            }
        }
        assert_eq!(observe_motion(&raw, true), Ok(()));
        raw[stride + 82 * 160 + 19..stride + 82 * 160 + 22].fill(16);
        assert_eq!(
            observe_motion(&raw, true),
            Err(ProbeReason::OutputContractFailed)
        );
    }

    #[test]
    fn upload_readiness_requires_every_decode_depth_and_interlace_parity_control() {
        let corpus = embedded_corpus(super::super::MANIFEST, SDR8, SDR10, HDR10)
            .expect("valid embedded live controls");
        let mut work = Vec::new();
        add_work(&corpus, &mut work).expect("valid embedded live controls");
        for graph in [
            MacosProcessingGraph::LiveSdrUploadScale,
            MacosProcessingGraph::LiveSdrUploadBwdifFrame,
            MacosProcessingGraph::LiveSdrUploadBwdifField,
        ] {
            let controls = work
                .iter()
                .filter(|(_, _, selected)| *selected == graph)
                .collect::<Vec<_>>();
            assert_eq!(
                controls.len(),
                4,
                "every eligible codec/depth or codec/parity tuple"
            );
            for (fixture, operation, _) in controls {
                assert_eq!(*operation, SmokeOperation::LiveUpload(graph));
                assert_eq!(fixture.class, "sdr");
                let output = &fixture.output_expectations[0];
                let field = graph == MacosProcessingGraph::LiveSdrUploadBwdifField;
                assert_eq!(output.expected.frame_count, if field { 24 } else { 12 });
                assert_eq!(
                    output.expected.avg_frame_rate,
                    if field { "24/1" } else { "12/1" }
                );
            }
        }
    }
}
