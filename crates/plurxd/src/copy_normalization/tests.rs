use super::{CopyNormalizationContract, CopyNormalizationError, CopyNormalizers, PipeNormalizer};
use plurx_core::{
    domain::MediaFile,
    fmp4::{self, Fragment, FragmentReader, Init, PromotionInputs, TrackKind, Unit},
    segplan::{
        companion::CopyCompanionVerifier,
        packet_clock::{SourceFragmentClock, SourcePacketObservation},
        IndexRow,
    },
    testfixtures,
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

pub(crate) fn non_dolby_file() -> MediaFile {
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
    )
    .expect("head establishes its copy init contract");
    let before = head_fragment.clone();
    let mut canonical_fragment = testfixtures::trimmed(&head_init, &head_fragment, usize::MAX);
    let kept_before = canonical_fragment.clone();
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
        .normalize_pair(&mut canonical_fragment, &mut av_fragment)
        .expect("an ordinary non-Dolby fragment keeps the head parameters");
    // Fragment equality covers the sample payload bytes, track decode times,
    // sample durations and composition offsets, not merely the byte length.
    assert_eq!(canonical_fragment, kept_before);
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

fn read(stream: &[u8]) -> (Init, Vec<Fragment>) {
    let mut reader = FragmentReader::new();
    reader.push(stream);
    let (mut init, mut fragments) = (None, Vec::new());
    while let Some(unit) = reader.next_unit().expect("the fixture parses") {
        match unit {
            Unit::Init(value) => init = Some(value),
            Unit::Fragment(value) => fragments.push(value),
            Unit::Trailer => {}
        }
    }
    (init.expect("an init"), fragments)
}

/// The normalizers a copy of `init` gets, converting Dolby Vision or not.
fn normalizers(init: &Init, head: &Fragment, convert: bool) -> CopyNormalizers {
    let pipe = || PipeNormalizer {
        raw_init: init.clone(),
        served_init: init.clone(),
        converter: convert
            .then(|| crate::dvpipe::Converter::for_init(init).expect("a describable init")),
    };
    let mut raw_parameters = PromotionInputs::from_fragment(head, init);
    raw_parameters.hdr10_sei.clear();
    CopyNormalizers {
        canonical: pipe(),
        av: pipe(),
        raw_parameters,
        failed: false,
    }
}

/// The row the legacy index pass gives `fragment`: converted in full, then
/// measured.
fn legacy_row(init: &Init, fragment: &Fragment, convert: bool) -> (Fragment, IndexRow) {
    let mut whole = fragment.clone();
    if convert {
        crate::dvpipe::Converter::for_init(init)
            .expect("a describable init")
            .convert(&mut whole)
            .expect("the whole fragment converts");
    }
    let row = IndexRow::from_fragment(&whole, init).expect("a row");
    (whole, row)
}

/// A canonical fragment kept without its slice data, through the real
/// normalization and verification, gives the row the legacy index pass gives
/// the whole fragment converted in full, and every sample its converted size:
/// on Dolby Vision copies of each GOP shape, on a single sample sized by its
/// `tfhd` (whose conversion grows the `moof`), and on copies that convert
/// nothing.
#[test]
fn a_trimmed_canonical_gives_the_row_of_the_fully_converted_one() {
    let mut cases = Vec::new();
    for kind in ["closed-gop", "open-gop", "clean-cra"] {
        let plain = testfixtures::video_only_pipe(kind);
        let (init, fragments) = read(&testfixtures::with_dolby_vision_rpus(&plain));
        if kind == "closed-gop" {
            let single = testfixtures::tfhd_sized_fragment(&init, &fragments[0], 1);
            cases.push((
                "a tfhd-sized sample".to_owned(),
                init.clone(),
                vec![single],
                true,
            ));
        }
        cases.push((format!("{kind} with RPUs"), init, fragments, true));
        let (init, fragments) = read(&plain);
        cases.push((kind.to_owned(), init, fragments, false));
    }
    let (init, fragments) = read(&testfixtures::video_only_pipe("h264"));
    cases.push(("h264".to_owned(), init, fragments, false));

    for (name, init, fragments, convert) in cases {
        let video = init.video().expect("video").id;
        let mut outputs = normalizers(&init, &fragments[0], convert);
        for fragment in &fragments {
            let (whole, row) = legacy_row(&init, fragment, convert);
            assert_eq!(
                convert,
                whole != *fragment,
                "{name}: conversion rewrites RPUs"
            );
            let mut canonical = testfixtures::trimmed(&init, fragment, 4096);
            assert!(canonical.held_len() < fragment.len(), "{name}: trimmed");
            let mut av = fragment.clone();
            outputs
                .normalize_pair(&mut canonical, &mut av)
                .expect("the pair normalizes");
            assert_eq!(av, whole, "{name}: the AV twin is converted in full");
            assert_eq!(
                canonical.kept().mdat_payload.start,
                whole.mdat_payload.start,
                "{name}: the kept moof is the whole one's size"
            );
            assert_eq!(
                IndexRow::from_fragment(&canonical, &init),
                Ok(row.clone()),
                "{name}"
            );
            let view = fmp4::VideoOnlyFragment::from(&canonical);
            let kept_sizes: Vec<u64> = canonical
                .kept()
                .track(video)
                .expect("video")
                .samples()
                .enumerate()
                .map(|(index, sample)| view.sample_size(index, sample.size))
                .collect();
            let whole_sizes: Vec<u64> = whole
                .track(video)
                .expect("video")
                .samples()
                .map(|sample| u64::from(sample.size))
                .collect();
            assert_eq!(kept_sizes, whole_sizes, "{name}: converted sample sizes");

            let clock = SourceFragmentClock {
                first_packet: SourcePacketObservation {
                    sequence: 0,
                    dts: 0,
                    pts: 0,
                    duration: 1,
                    size: 1,
                    checksum: 0,
                    flags: 1,
                    side_data_count: 0,
                    time_base_num: 1,
                    time_base_den: 1,
                },
                canonical_dts: row.dts,
                duration: row.duration,
                next_packet_sequence: 1,
                last_muxed_dts: 0,
            };
            let mut verifier =
                CopyCompanionVerifier::new(init.clone(), init.clone()).expect("matching inits");
            assert_eq!(verifier.verify(&canonical, clock, &av), Ok(row), "{name}");
        }
    }
}

/// The tfhd-sized case above is the one whose conversion grows the `moof`.
#[test]
fn converting_a_tfhd_sized_sample_grows_its_moof() {
    let stream = testfixtures::with_dolby_vision_rpus(&testfixtures::video_only_pipe("closed-gop"));
    let (init, fragments) = read(&stream);
    let single = testfixtures::tfhd_sized_fragment(&init, &fragments[0], 1);
    let (whole, _) = legacy_row(&init, &single, true);
    assert_eq!(whole.mdat_payload.start, single.mdat_payload.start + 4);
}

/// A canonical fragment whose conversion is refused fails its pair, and the
/// copy with it, though only what is kept of it was converted: with a sound
/// AV twin, and with a twin refused for the same RPU, whose refusal (the
/// whole fragment's, at the sample's real byte offset) is the one reported.
#[test]
fn a_canonical_conversion_refusal_fails_the_pair() {
    let sound = testfixtures::with_dolby_vision_rpus(&testfixtures::video_only_pipe("closed-gop"));
    let (init, fragments) = read(&sound);
    let (_, broken) = read(&testfixtures::with_one_unconvertible_rpu(&sound, 0));
    let refused = crate::dvpipe::Converter::for_init(&init)
        .expect("a describable init")
        .convert(&mut broken[0].clone())
        .expect_err("the whole fragment is refused")
        .to_string();

    let mut outputs = normalizers(&init, &fragments[0], true);
    let mut canonical = testfixtures::trimmed(&init, &broken[0], 4096);
    assert!(canonical.held_len() < broken[0].len());
    assert!(matches!(
        outputs.normalize_pair(&mut canonical, &mut fragments[0].clone()),
        Err(CopyNormalizationError::Conversion(_))
    ));
    assert!(matches!(
        outputs.normalize_pair(
            &mut testfixtures::trimmed(&init, &fragments[1], 4096),
            &mut fragments[1].clone()
        ),
        Err(CopyNormalizationError::Failed)
    ));

    let mut outputs = normalizers(&init, &fragments[0], true);
    let mut canonical = testfixtures::trimmed(&init, &broken[0], 4096);
    match outputs.normalize_pair(&mut canonical, &mut broken[0].clone()) {
        Err(CopyNormalizationError::Conversion(reason)) => assert_eq!(reason, refused),
        other => panic!("expected a conversion refusal, got {other:?}"),
    }
}
