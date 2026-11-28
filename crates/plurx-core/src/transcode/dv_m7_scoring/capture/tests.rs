#![expect(
    clippy::unwrap_used,
    reason = "test fixtures intentionally stop at the first invalid setup or assertion result"
)]

use super::*;
use crate::transcode::dv_refit::{apply_to_rpu, servable, RefitCurves};
use dolby_vision::rpu::{dovi_rpu::DoviRpu, ConversionMode};

const MOCK_NAL: &[u8] = &[0x7c, 1, 0x19];
const POSITIONS: [[u16; 2]; 8] = [
    [3, 5],
    [4, 5],
    [11, 5],
    [12, 5],
    [3, 6],
    [4, 6],
    [11, 6],
    [12, 6],
];

fn child_status(success: bool) -> ExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(if success { 0 } else { 1 << 8 })
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        ExitStatus::from_raw(u32::from(!success))
    }
}

fn layout(role: CaptureRole, denominator: u32) -> CaptureLayout {
    CaptureLayout {
        role,
        video_index: 1,
        time_base: Rational::new(1, denominator).unwrap(),
        raster: [64, 64],
        roi: [3, 5, 13, 7],
    }
}

fn evidence(
    ordinal: u64,
    pts: i64,
    duration: i64,
    time_base: Rational,
    nal: &[u8],
) -> FrameEvidence {
    FrameEvidence {
        ordinal,
        pts,
        duration,
        time_base,
        access_unit_sha256: [0xab; 32],
        rpu_sha256: Sha256::digest(nal).into(),
        bl_vcl_sha256: [0xcd; 32],
    }
}

// Independently spell the C wire format for the small nonzero-origin ROI
// above. The fixture intentionally crosses a stride boundary on each row.
fn wire(role: CaptureRole, denominator: u32, frames: &[(FrameEvidence, &[u8])]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let record_bytes = match role {
        CaptureRole::SourceFel => {
            bytes.extend_from_slice(b"PLXM7F01");
            16
        }
        CaptureRole::AuthoredCandidate => {
            bytes.extend_from_slice(b"PLXM7C01");
            10
        }
    };
    let first = frames.first().unwrap().0;
    let last = frames.last().unwrap().0;
    for field in [
        1,
        1,
        denominator,
        64,
        64,
        3,
        5,
        13,
        7,
        8,
        3,
        8,
        record_bytes,
        frames.len() as u32,
        first.ordinal as u32,
        last.ordinal as u32,
    ] {
        bytes.extend_from_slice(&field.to_le_bytes());
    }
    bytes.extend_from_slice(&first.pts.to_le_bytes());
    bytes.extend_from_slice(&(last.pts + last.duration).to_le_bytes());
    for (frame, rpu) in frames {
        bytes.extend_from_slice(b"FRM1");
        bytes.extend_from_slice(&(224 + rpu.len() as u32 + 8 * record_bytes).to_le_bytes());
        bytes.extend_from_slice(&frame.ordinal.to_le_bytes());
        bytes.extend_from_slice(&frame.pts.to_le_bytes());
        bytes.extend_from_slice(&frame.duration.to_le_bytes());
        bytes.extend_from_slice(&8_u32.to_le_bytes());
        bytes.extend_from_slice(&(rpu.len() as u32).to_le_bytes());
        for digest in [
            frame.access_unit_sha256,
            frame.rpu_sha256,
            frame.bl_vcl_sha256,
        ] {
            bytes.extend_from_slice(hex::encode(digest).as_bytes());
        }
        bytes.extend_from_slice(rpu);
        for [x, y] in POSITIONS {
            for field in [x, y, 128, 500, 540] {
                bytes.extend_from_slice(&field.to_le_bytes());
            }
            if record_bytes == 16 {
                for residual in [511_u16, 513, 510] {
                    bytes.extend_from_slice(&residual.to_le_bytes());
                }
            }
        }
    }
    bytes.extend_from_slice(b"END1");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&(frames.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&(frames.len() as u64 * 8).to_le_bytes());
    bytes
}

#[test]
fn wire_requires_exact_lattice_independent_clock_and_digests() {
    let layout = layout(CaptureRole::SourceFel, 1000);
    let expected = [evidence(192, 8133, 42, layout.time_base, MOCK_NAL)];
    let plan = CapturePlan {
        layout,
        frames: &expected,
    };
    let valid = wire(layout.role, 1000, &[(expected[0], MOCK_NAL)]);
    let mut reader = CaptureReader::new(valid.as_slice(), plan, child_status(true)).unwrap();
    assert_eq!(
        reader.next_frame().unwrap().unwrap().evidence(),
        expected[0]
    );
    assert!(reader.next_frame().unwrap().is_none());
    assert_eq!(
        reader.finish().unwrap(),
        CaptureCompletion {
            layout,
            frames: 1,
            samples: 8
        }
    );

    let record = 88 + 8 + 224 + MOCK_NAL.len();
    for (offset, replacement) in [(record, 4), (record + 4, 0xff)] {
        let mut invalid = valid.clone();
        invalid[offset] = replacement;
        if offset == record + 4 {
            invalid[offset + 1] = 4;
        }
        let mut reader = CaptureReader::new(invalid.as_slice(), plan, child_status(true)).unwrap();
        assert!(matches!(reader.next_frame(), Err(CaptureError::Sample)));
        assert!(matches!(reader.finish(), Err(CaptureError::Completion)));
    }

    let digest = 88 + 8 + 32;
    let nal = 88 + 8 + 224;
    for (offset, value) in [(digest, b'A'), (nal + 2, 0xff)] {
        let mut invalid = valid.clone();
        invalid[offset] = value;
        let mut reader = CaptureReader::new(invalid.as_slice(), plan, child_status(true)).unwrap();
        assert!(matches!(reader.next_frame(), Err(CaptureError::Digest)));
    }
    for offset in [88 + 8 + 8, 88 + 8 + 16, digest] {
        let mut invalid = valid.clone();
        if offset == digest {
            invalid[offset] = b'b';
        } else {
            invalid[offset] ^= 1;
        }
        let mut reader = CaptureReader::new(invalid.as_slice(), plan, child_status(true)).unwrap();
        assert!(matches!(reader.next_frame(), Err(CaptureError::Frame)));
    }

    let mut wrong_role = valid.clone();
    wrong_role[5] = b'C';
    assert!(matches!(
        CaptureReader::new(wrong_role.as_slice(), plan, child_status(true)),
        Err(CaptureError::Header)
    ));
    assert!(matches!(
        CaptureReader::new(valid.as_slice(), plan, child_status(false)),
        Err(CaptureError::Child(_))
    ));
}

#[test]
fn reordered_repeated_or_partial_frames_never_complete() {
    let layout = layout(CaptureRole::SourceFel, 1000);
    let frames = [
        evidence(17, 742, 41, layout.time_base, MOCK_NAL),
        evidence(18, 783, 42, layout.time_base, MOCK_NAL),
    ];
    let plan = CapturePlan {
        layout,
        frames: &frames,
    };
    let valid = wire(
        layout.role,
        1000,
        &[(frames[0], MOCK_NAL), (frames[1], MOCK_NAL)],
    );
    let first = 88;
    let frame_bytes = 8 + 224 + MOCK_NAL.len() + 8 * 16;
    let second = first + frame_bytes;

    let mut bad_census = frames;
    bad_census[1].pts = bad_census[0].pts;
    let duplicate = CapturePlan {
        layout,
        frames: &bad_census,
    };
    assert!(matches!(
        CaptureReader::new(valid.as_slice(), duplicate, child_status(true)),
        Err(CaptureError::Census)
    ));

    let mut swapped = valid.clone();
    swapped[first..second].copy_from_slice(&valid[second..second + frame_bytes]);
    let mut reader = CaptureReader::new(swapped.as_slice(), plan, child_status(true)).unwrap();
    assert!(matches!(reader.next_frame(), Err(CaptureError::Frame)));

    let mut doubled = valid.clone();
    doubled[second..second + frame_bytes].copy_from_slice(&valid[first..second]);
    let mut reader = CaptureReader::new(doubled.as_slice(), plan, child_status(true)).unwrap();
    assert!(reader.next_frame().unwrap().is_some());
    assert!(matches!(reader.next_frame(), Err(CaptureError::Frame)));

    let reader = CaptureReader::new(valid.as_slice(), plan, child_status(true)).unwrap();
    assert!(matches!(reader.finish(), Err(CaptureError::Completion)));

    let mut bad_total = valid.clone();
    let last = bad_total.len() - 8;
    bad_total[last] += 1;
    let mut extended = valid.clone();
    extended.push(0);
    for invalid in [
        bad_total.as_slice(),
        extended.as_slice(),
        &valid[..valid.len() - 1],
    ] {
        let mut reader = CaptureReader::new(invalid, plan, child_status(true)).unwrap();
        assert!(reader.next_frame().unwrap().is_some());
        assert!(reader.next_frame().unwrap().is_some());
        assert!(matches!(
            reader.finish(),
            Err(CaptureError::Completion | CaptureError::Truncated)
        ));
    }

    let mut reader = CaptureReader::new(&valid[..second - 1], plan, child_status(true)).unwrap();
    assert!(matches!(reader.next_frame(), Err(CaptureError::Truncated)));
}

fn calibrated_rpus() -> (Vec<u8>, DoviRpu, Vec<u8>, ServableCurves) {
    let mut source = DoviRpu::parse_unspec62_nalu(include_bytes!(
        "../../../../tests/fixtures/dv-runtime/frame-000.nal"
    ))
    .unwrap();
    let mapping = source.rpu_data_mapping.as_mut().unwrap();
    mapping.set_empty_p81_mapping();
    let nlq = mapping.nlq.as_mut().unwrap();
    nlq.nlq_offset = [512; 3];
    nlq.vdr_in_max_int = [0; 3];
    nlq.vdr_in_max = [1_048_576; 3];
    nlq.linear_deadzone_slope_int = [0; 3];
    nlq.linear_deadzone_slope = [2048; 3];
    nlq.linear_deadzone_threshold_int = [0; 3];
    nlq.linear_deadzone_threshold = [0; 3];
    source.vdr_dm_data.as_mut().unwrap().set_p81_coeffs();
    source.modified = true;
    let source_nal = source.write_hevc_unspec62_nalu().unwrap();

    source.convert_with_mode(ConversionMode::To81).unwrap();
    let pinned = servable(&RefitCurves::identity()).unwrap();
    let mut served = source.clone();
    apply_to_rpu(&mut served, &pinned).unwrap();
    (
        source_nal,
        source,
        served.write_hevc_unspec62_nalu().unwrap(),
        pinned,
    )
}

fn one_frame(
    layout: CaptureLayout,
    observed: FrameEvidence,
    expected: FrameEvidence,
    rpu: &[u8],
) -> CapturedFrame {
    let census = [expected];
    let plan = CapturePlan {
        layout,
        frames: &census,
    };
    let encoded = wire(
        layout.role,
        layout.time_base.denominator(),
        &[(observed, rpu)],
    );
    let mut reader = CaptureReader::new(encoded.as_slice(), plan, child_status(true)).unwrap();
    let frame = reader.next_frame().unwrap().unwrap();
    assert_eq!(reader.finish().unwrap().samples, 8);
    frame
}

#[test]
fn scorer_requires_exact_authoring_crop_two_clocks_and_readback() {
    let (source_nal, mut candidate_rpu, served, pinned) = calibrated_rpus();
    let source_layout = layout(CaptureRole::SourceFel, 1000);
    let candidate_layout = layout(CaptureRole::AuthoredCandidate, 48000);
    let source_evidence = evidence(192, 8133, 42, source_layout.time_base, &source_nal);
    let source = one_frame(source_layout, source_evidence, source_evidence, &source_nal);

    let candidate_nal = candidate_rpu.write_hevc_unspec62_nalu().unwrap();
    let observed_candidate = evidence(0, 0, 2002, candidate_layout.time_base, &candidate_nal);
    let candidate_evidence = FrameEvidence {
        duration: 1001,
        time_base: Rational::new(1, 24000).unwrap(),
        ..observed_candidate
    };
    let candidate = one_frame(
        candidate_layout,
        observed_candidate,
        candidate_evidence,
        &candidate_nal,
    );
    let mut authoring = AuthoringFrameBinding {
        source: source_evidence,
        candidate: candidate_evidence,
        crop_first_source_ordinal: source_evidence.ordinal,
        crop_frames: 1,
    };
    let mut readback = FplusFrameEvidence {
        source: source_evidence,
        bl_vcl_sha256: source_evidence.bl_vcl_sha256,
        rpu_sha256: Sha256::digest(&served).into(),
        rpu_nal: &served,
    };
    let scores = score_paired_frame(
        &source,
        &candidate,
        &authoring,
        &readback,
        &readback,
        &pinned,
        || false,
    )
    .unwrap();
    assert_eq!(scores.len(), 8);
    assert!(scores
        .iter()
        .flatten()
        .all(|s| s.candidate_delta_e_itp.is_finite()));

    authoring.candidate.duration += 1;
    assert!(matches!(
        score_paired_frame(
            &source,
            &candidate,
            &authoring,
            &readback,
            &readback,
            &pinned,
            || false
        ),
        Err(CaptureError::Authoring)
    ));
    authoring.candidate = candidate_evidence;
    authoring.crop_first_source_ordinal -= 1;
    assert!(matches!(
        score_paired_frame(
            &source,
            &candidate,
            &authoring,
            &readback,
            &readback,
            &pinned,
            || false
        ),
        Err(CaptureError::Authoring)
    ));
    authoring.crop_first_source_ordinal = source_evidence.ordinal;

    readback.bl_vcl_sha256[0] ^= 1;
    assert!(matches!(
        score_paired_frame(
            &source,
            &candidate,
            &authoring,
            &readback,
            &readback,
            &pinned,
            || false
        ),
        Err(CaptureError::Readback)
    ));
    readback.bl_vcl_sha256 = source_evidence.bl_vcl_sha256;

    candidate_rpu.vdr_dm_data.as_mut().unwrap().rgb_to_lms_coef8 += 1;
    let wrong_nal = candidate_rpu.write_hevc_unspec62_nalu().unwrap();
    let wrong_evidence = evidence(0, 0, 2002, candidate_layout.time_base, &wrong_nal);
    let wrong_candidate = one_frame(candidate_layout, wrong_evidence, wrong_evidence, &wrong_nal);
    authoring.candidate = wrong_evidence;
    assert!(matches!(
        score_paired_frame(
            &source,
            &wrong_candidate,
            &authoring,
            &readback,
            &readback,
            &pinned,
            || false
        ),
        Err(CaptureError::Score(ScoreError::UnsupportedCandidateRpu))
    ));
}

#[test]
fn layout_admits_the_full_supported_raster_and_preserves_the_frozen_roi() {
    let frozen = CaptureLayout {
        raster: [3840, 2160],
        roi: [0, 32, 3840, 2128],
        ..layout(CaptureRole::SourceFel, 1000)
    };
    assert_eq!(frozen.sites_per_frame().unwrap(), 503_040);
    let full = CaptureLayout {
        roi: [0, 0, 3840, 2160],
        ..frozen
    };
    assert_eq!(full.sites_per_frame().unwrap(), 518_400);
    let oversized = CaptureLayout {
        raster: [3842, 2160],
        ..full
    };
    assert!(matches!(
        oversized.sites_per_frame(),
        Err(CaptureError::Layout)
    ));
    let reversed = CaptureLayout {
        roi: [20, 32, 10, 2128],
        ..full
    };
    assert!(matches!(
        reversed.sites_per_frame(),
        Err(CaptureError::Layout)
    ));
}
