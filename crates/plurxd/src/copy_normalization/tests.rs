use super::{CopyNormalizationContract, CopyNormalizationError};
use plurx_core::{
    domain::MediaFile,
    fmp4::{self, Fragment, FragmentReader, Init, TrackKind, Unit},
    transcode::CopyVideoOptions,
};

fn core_authored_mp4_head() -> (Init, Fragment) {
    let mut reader = FragmentReader::new();
    reader.reject_reinitialization();
    reader.push(include_bytes!(
        "../../../plurx-core/tests/fixtures/dv-runtime/authored.mp4"
    ));
    let mut init = None;
    let mut fragment = None;
    while let Some(unit) = reader.next_unit().expect("core-authored MP4 parses") {
        match unit {
            Unit::Init(value) => init = Some(value),
            Unit::Fragment(value) => fragment = Some(value),
            Unit::Trailer => {}
        }
    }
    (
        init.expect("core-authored MP4 has an init"),
        fragment.expect("core-authored MP4 has a fragment"),
    )
}

fn non_dolby_file() -> MediaFile {
    MediaFile {
        downloaded_subtitles: Vec::new(),
        id: 1,
        item_id: 1,
        path: "/fixture/authored.mp4".into(),
        size: 7_411,
        mtime: 1,
        duration_ms: Some(125),
        container: Some("mp4".into()),
        video_codec: Some("hevc".into()),
        video_codec_tag: None,
        field_order: None,
        video_profile: Some("Main 10".into()),
        width: Some(64),
        height: Some(64),
        bit_depth: Some(10),
        hdr: None,
        hdr_format: None,
        max_cll: None,
        max_fall: None,
        mastering_max_luminance: None,
        luminance_source: None,
        bitrate: Some(1_000_000),
        audio_streams: Vec::new(),
        subtitle_streams: Vec::new(),
        scanned_at: 1,
        audio_offset_ms: 0,
        probed: true,
        dolby_vision: Default::default(),
    }
}

#[test]
fn head_contract_reapplies_to_both_outputs_and_keeps_non_dolby_samples_unchanged() {
    let (head_init, head_fragment) = core_authored_mp4_head();
    let contract = CopyNormalizationContract::from_head(
        &non_dolby_file(),
        CopyVideoOptions::new(false, false),
        &head_init,
        &head_fragment,
        None,
    )
    .expect("head establishes its copy init contract");
    let before = head_fragment.clone();
    let mut canonical_fragment = head_fragment.clone();
    let mut av_fragment = head_fragment;
    let mut outputs = contract
        .begin(head_init.clone(), head_init)
        .expect("both output inits satisfy the head contract");

    assert_eq!(
        fmp4::copy_video_init_contract_sha256(outputs.canonical_init())
            .expect("canonical video init contract"),
        contract.init_contract_sha256()
    );
    assert_eq!(
        fmp4::copy_video_init_contract_sha256(outputs.av_init()).expect("AV video init contract"),
        contract.init_contract_sha256()
    );

    outputs
        .normalize_pair(&mut canonical_fragment, &mut av_fragment, &unused_clock())
        .expect("an ordinary non-Dolby fragment keeps the head parameters");
    // Fragment equality covers the sample payload bytes, track decode times,
    // sample durations and composition offsets, not merely the byte length.
    assert_eq!(canonical_fragment, before);
    assert_eq!(av_fragment, before);
}

#[test]
fn reconstructed_init_with_changed_video_timing_is_rejected_for_each_output() {
    let (head_init, head_fragment) = core_authored_mp4_head();
    let contract = CopyNormalizationContract::from_head(
        &non_dolby_file(),
        CopyVideoOptions::new(false, false),
        &head_init,
        &head_fragment,
        None,
    )
    .expect("head establishes its copy init contract");

    for changed_output in [0, 1] {
        let mut changed_init = head_init.clone();
        let video = changed_init
            .tracks
            .iter_mut()
            .find(|track| track.kind == TrackKind::Video)
            .expect("fixture video track");
        video.timescale = video
            .timescale
            .checked_add(1)
            .expect("small fixture timescale");

        let result = if changed_output == 0 {
            contract.begin(changed_init, head_init.clone())
        } else {
            contract.begin(head_init.clone(), changed_init)
        };
        assert!(
            matches!(result, Err(CopyNormalizationError::ContractChanged)),
            "output side {changed_output} must not publish an init with changed video timing"
        );
    }
}

/// A packet clock for a pair whose conversion writes no F+ curves, which
/// never reads it.
fn unused_clock() -> plurx_core::segplan::packet_clock::SourceFragmentClock {
    plurx_core::segplan::packet_clock::SourceFragmentClock {
        first_packet: plurx_core::segplan::packet_clock::SourcePacketObservation {
            sequence: 0,
            dts: 0,
            pts: 0,
            duration: 1,
            size: 1,
            checksum: 0,
            flags: 1,
            side_data_count: 0,
            time_base_num: 1,
            time_base_den: 1000,
        },
        canonical_dts: 0,
        duration: 1,
        next_packet_sequence: 1,
    }
}
