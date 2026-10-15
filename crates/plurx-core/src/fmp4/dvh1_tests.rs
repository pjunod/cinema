// Synthetic counterpart of FFmpeg 8's extracted dvh1 configuration.
fn dvh1_incomplete_fixture() -> (Init, Fragment) {
    let mut init = minimal_hvcc_dv_init();
    let fragment = fragment_with_first_video_sample(
        init.video().expect("fixture video track").id,
        length_prefixed_hevc_nals(&[
            &[0x40, 1, 0x0c],
            &[0x42, 1, 1],
            &[0x44, 1, 0xc0],
            &[0x7c, 1, 0x19],
            &[0x26, 1, 0x80],
        ]),
    );
    promote_hevc_parameter_sets(&mut init, &fragment).expect("valid DV test fixture operation");
    set_dolby_vision_record(
        &mut init,
        &DolbyVisionRecord::new(5, 6, false, true, true, 0)
            .expect("valid DV test fixture operation"),
    )
    .expect("valid DV test fixture operation");
    let location = locate_hvcc(&init.bytes)
        .expect("valid fixture boxes")
        .expect("fixture hvcC");
    let at = location.ancestors[1].start + 4;
    init.bytes[at..at + 4].copy_from_slice(b"dvh1");
    for at in dvh1_array_offsets(&init) {
        init.bytes[at] &= 0x7f;
    }
    (init, fragment)
}

fn dvh1_array_offsets(init: &Init) -> Vec<usize> {
    let location = locate_hvcc(&init.bytes)
        .expect("valid fixture boxes")
        .expect("fixture hvcC");
    let mut pos = location.payload.start + 23;
    let mut offsets = Vec::new();
    for _ in 0..init.bytes[location.payload.start + 22] {
        if matches!(init.bytes[pos] & 0x3f, 32..=34) {
            offsets.push(pos);
        }
        let count = u16::from_be_bytes([init.bytes[pos + 1], init.bytes[pos + 2]]);
        pos += 3;
        for _ in 0..count {
            let len = u16::from_be_bytes([init.bytes[pos], init.bytes[pos + 1]]) as usize;
            pos += 2 + len;
        }
    }
    offsets
}

#[test]
fn dvh1_completeness_repairs_only_a_matching_authored_tuple() {
    let (original, fragment) = dvh1_incomplete_fixture();
    assert!(validate_hevc_decoder_configuration(&original).is_err());
    for mixed in [false, true] {
        let mut init = original.clone();
        if mixed {
            init.bytes[dvh1_array_offsets(&original)[0]] |= 0x80;
        }
        let before = init.bytes.clone();
        let offsets = dvh1_array_offsets(&init);
        assert!(promote_hevc_parameter_sets(&mut init, &fragment)
            .expect("valid DV test fixture operation"));
        validate_hevc_decoder_configuration(&init).expect("valid DV test fixture operation");
        let mut expected = before;
        for offset in offsets {
            expected[offset] |= 0x80;
        }
        assert_eq!(
            init.bytes, expected,
            "only completeness bits change, including DOVI bytes"
        );
        assert!(!promote_hevc_parameter_sets(&mut init, &fragment)
            .expect("valid DV test fixture operation"));
        let segment = merge(std::slice::from_ref(&fragment), &init, 1)
            .expect("valid DV test fixture operation");
        let mut reader = FragmentReader::new();
        reader.push(&init.bytes);
        reader.push(&segment.bytes);
        reader.next_unit().expect("valid DV test fixture operation");
        let Some(Unit::Fragment(served)) =
            reader.next_unit().expect("valid DV test fixture operation")
        else {
            panic!("fragment")
        };
        assert_eq!(
            first_video_sample(&served, init.video().expect("fixture video track"))
                .expect("valid DV test fixture operation"),
            length_prefixed_hevc_nals(&[&[0x7c, 1, 0x19], &[0x26, 1, 0x80]])
        );
        let kept = merge_retaining(std::slice::from_ref(&fragment), &init, 1, true)
            .expect("valid DV test fixture operation");
        let mut reader = FragmentReader::new();
        reader.push(&init.bytes);
        reader.push(&kept.bytes);
        reader.next_unit().expect("valid DV test fixture operation");
        let Some(Unit::Fragment(kept)) =
            reader.next_unit().expect("valid DV test fixture operation")
        else {
            panic!("fragment")
        };
        assert_eq!(
            first_video_sample(&kept, init.video().expect("fixture video track")),
            first_video_sample(&fragment, init.video().expect("fixture video track"))
        );
    }
}

#[test]
fn dvh1_completeness_rejects_missing_conflicting_or_malformed_evidence() {
    let (original, fragment) = dvh1_incomplete_fixture();
    let good = PromotionInputs::from_fragment(&fragment, &original);
    assert_eq!(good.dvh1_parameter_sets.len(), 3);
    let mut candidates = vec![Vec::new(), good.dvh1_parameter_sets[..2].to_vec()];
    let mut conflicting = good.dvh1_parameter_sets.clone();
    conflicting[1][2] ^= 1;
    candidates.push(conflicting);
    let mut extra = good.dvh1_parameter_sets.clone();
    extra.push(vec![0x44, 1, 0xff]);
    candidates.push(extra);
    for candidate in candidates {
        let mut init = original.clone();
        let _ = dvh1::complete(&mut init, &candidate);
        assert_eq!(init.bytes, original.bytes);
        assert!(validate_hevc_decoder_configuration(&init).is_err());
    }
    for corruption in [0, 1, 2] {
        let mut init = original.clone();
        let at = dvh1_array_offsets(&init)[0];
        match corruption {
            0 => init.bytes[at] = 33,       // declared type disagrees with NAL
            1 => init.bytes[at + 5] = 0x80, // forbidden NAL header
            _ => init.bytes[at + 3..at + 5].copy_from_slice(&u16::MAX.to_be_bytes()),
        }
        let before = init.bytes.clone();
        assert!(dvh1::complete(&mut init, &good.dvh1_parameter_sets).is_err());
        assert_eq!(init.bytes, before);
    }
    let mut malformed =
        first_video_sample(&fragment, original.video().expect("fixture video track"))
            .expect("valid DV test fixture operation")
            .to_vec();
    malformed.push(0); // a truncated next NAL cannot be ignored
    assert!(dvh1::capture(&original, &malformed, 4).is_err());
    let malformed = fragment_with_first_video_sample(
        original.video().expect("fixture video track").id,
        malformed,
    );
    let mut init = original.clone();
    assert!(promote_hevc_parameter_sets(&mut init, &malformed).is_err());
    let malformed_inputs = PromotionInputs::from_fragment(&malformed, &original);
    assert!(malformed_inputs.dvh1_parameter_sets_malformed);
    assert!(!good.matches_later_parameter_sets(&malformed_inputs));
    assert_eq!(init.bytes, original.bytes);
    for tag in [b"hvc1", b"hev1", b"dvhe"] {
        let mut init = original.clone();
        let at = locate_hvcc(&init.bytes)
            .expect("valid fixture boxes")
            .expect("fixture hvcC")
            .ancestors[1]
            .start
            + 4;
        init.bytes[at..at + 4].copy_from_slice(tag);
        let before = init.bytes.clone();
        assert!(!dvh1::complete(&mut init, &good.dvh1_parameter_sets)
            .expect("valid DV test fixture operation"));
        assert_eq!(init.bytes, before);
    }
}

#[test]
fn dvh1_completeness_survives_equivalent_descriptions_and_persisted_replay() {
    let (mut original, fragment) = dvh1_incomplete_fixture();
    duplicate_hevc_sample_entry(&mut original);
    append_trailing_zero_to_second_entry_pps(&mut original);
    let inputs = PromotionInputs::from_fragment(&fragment, &original);
    assert_eq!(inputs.dvh1_parameter_sets.len(), 3);
    let restored: PromotionInputs = serde_json::from_str(
        &serde_json::to_string(&inputs).expect("valid DV test fixture operation"),
    )
    .expect("valid DV test fixture operation");
    let mut live = original.clone();
    promote_hevc_parameter_sets(&mut live, &fragment).expect("valid DV test fixture operation");
    let mut planned = original.clone();
    promote_from(&mut planned, &restored).expect("valid DV test fixture operation");
    assert_eq!(planned.bytes, live.bytes);
    validate_hevc_decoder_configuration(&planned).expect("valid DV test fixture operation");
    assert_eq!(
        validate_hevc_sample_entries(&planned).expect("valid DV test fixture operation"),
        HevcSampleEntryLayout::Single
    );
    let mut legacy = serde_json::to_value(&inputs).expect("valid DV test fixture operation");
    legacy
        .as_object_mut()
        .expect("valid DV test fixture operation")
        .remove("dvh1_parameter_sets");
    let legacy: PromotionInputs =
        serde_json::from_value(legacy).expect("valid DV test fixture operation");
    assert!(legacy.dvh1_parameter_sets.is_empty());
    let mut unproved = original.clone();
    promote_from(&mut unproved, &legacy).expect("valid DV test fixture operation");
    assert!(validate_hevc_decoder_configuration(&unproved).is_err());
    let bare = fragment_with_first_video_sample(
        original.video().expect("fixture video track").id,
        length_prefixed_hevc_nals(&[&[0x26, 1, 0x80]]),
    );
    let absent = PromotionInputs::from_fragment(&bare, &original);
    assert!(inputs.matches_later_parameter_sets(&absent));
    assert!(
        !absent.matches_later_parameter_sets(&inputs),
        "later evidence cannot bless unproved opening"
    );
    let mut conflict = inputs.clone();
    conflict.dvh1_parameter_sets[0][2] ^= 1;
    assert!(!inputs.matches_later_parameter_sets(&conflict));
    let sample = length_prefixed_hevc_nals(&[
        &[0x44, 1, 0xc0, 0],
        &[0x40, 1, 0x0c],
        &[0x42, 1, 1],
        &[0x40, 1, 0x0c],
    ]);
    assert_eq!(
        dvh1::capture(&original, &sample, 4).expect("valid DV test fixture operation"),
        inputs.dvh1_parameter_sets
    );
}

#[test]
#[ignore = "needs a private FFmpeg dvh1 capture; PLURX_DVH1_CAPTURE"]
fn captured_dvh1_completeness_preserves_dolby_vision_and_publishes() {
    let feed = std::fs::read(std::env::var("PLURX_DVH1_CAPTURE").expect("private capture path"))
        .expect("valid DV test fixture operation");
    let (original, fragments, _) = read_all(&feed);
    assert!(validate_hevc_decoder_configuration(&original).is_err());
    let dovi = dolby_vision_record(&original)
        .expect("valid DV test fixture operation")
        .expect("valid DV test fixture operation");
    assert_eq!(dovi.profile, 5);
    let inputs = PromotionInputs::from_fragment(&fragments[0], &original);
    assert_eq!(inputs.dvh1_parameter_sets.len(), 3);
    let mut init = original.clone();
    promote_from(&mut init, &inputs).expect("valid DV test fixture operation");
    validate_hevc_decoder_configuration(&init).expect("valid DV test fixture operation");
    assert_eq!(
        dolby_vision_record(&init).expect("valid DV test fixture operation"),
        Some(dovi)
    );
    let mut output = init.bytes.clone();
    for (n, fragment) in fragments.iter().enumerate() {
        let segment = merge(std::slice::from_ref(fragment), &init, n as u32 + 1)
            .expect("valid DV test fixture operation");
        output.extend(segment.bytes);
    }
    if let Ok(path) = std::env::var("PLURX_DVH1_REPAIRED") {
        std::fs::write(path, output).expect("valid DV test fixture operation");
    }
}

#[test]
fn rolling_dvh1_guard_checks_every_sample_and_strict_nal_framing() {
    let (mut init, opening) = dvh1_incomplete_fixture();
    promote_hevc_parameter_sets(&mut init, &opening).expect("repair fixture");
    let video = init.video().expect("video").id;
    let (_, fragments, _) = read_all(&pipe("open-gop"));
    let base = fragments[0].clone();
    let mut ambiguous = init.clone();
    let mut extra = ambiguous.video().expect("video").clone(); extra.id += 100;
    ambiguous.tracks.push(extra);
    assert!(validate_rolling_dvh1_fragment(&ambiguous, &base).is_err());
    let mut outside_mdat = base.clone();
    outside_mdat.tracks.iter_mut().find(|t| t.track_id == video).expect("video").runs[0].data_offset = 0;
    assert!(validate_rolling_dvh1_fragment(&init, &outside_mdat).is_err());
    assert!(base.track(video).expect("video").sample_count() > 1);
    for invalid in [false, true] {
        for nal in [vec![0x44, 1, 0xc1], vec![0x44], vec![0xc4, 1, 0xc0], vec![0x44, 0, 0xc0]] {
            let mut fragment = base.clone();
            let mut count = 0;
            rewrite_video_samples(&mut fragment, &init.tracks, video, |_| {
                count += 1;
                Ok(if invalid && count == 2 { length_prefixed_hevc_nals(&[&nal]) }
                   else { length_prefixed_hevc_nals(&[&[0x44, 1, 0xc0, 0], &[0x26, 1, 0x80]]) })
            }).expect("rewrite fixture");
            assert_eq!(validate_rolling_dvh1_fragment(&init, &fragment).is_err(), invalid);
        }
    }
    for bad in [vec![], vec![0], vec![0, 0, 0, 0], vec![0, 0, 0, 3, 0x26, 1]] {
        let mut fragment = base.clone();
        let mut count = 0;
        rewrite_video_samples(&mut fragment, &init.tracks, video, |_| {
            count += 1;
            Ok(if count == 2 { bad.clone() } else { length_prefixed_hevc_nals(&[&[0x26, 1, 0x80]]) })
        }).expect("rewrite fixture");
        assert!(validate_rolling_dvh1_fragment(&init, &fragment).is_err());
    }
    let mut absent = base;
    rewrite_video_samples(&mut absent, &init.tracks, video, |_| Ok(length_prefixed_hevc_nals(&[&[0x26, 1, 0x80]]))).expect("absent repeats");
    validate_rolling_dvh1_fragment(&init, &absent).expect("absent is safe");
    let moof = peek_box(&absent.bytes, 0).expect("moof").expect("moof");
    for (at, traf) in find_children(&absent.bytes, moof.header_len..moof.size, b"traf").expect("trafs") {
        let (tfhd_at, tfhd) = find_child(&absent.bytes, at.start+traf.header_len..at.start+traf.size, b"tfhd").expect("tfhd").expect("tfhd");
        let payload = tfhd_at.start+tfhd.header_len;
        if be_u32(&absent.bytes,payload+4)==video {
            // A description override is four bytes longer: reuse the default
            // duration slot in this synthetic tfhd to supply an invalid index.
            let flags=be_u32(&absent.bytes,payload);
            assert_eq!(flags & 1,0);
            assert!(tfhd.size >= tfhd.header_len+12);
            absent.bytes[payload..payload+4].copy_from_slice(&(flags|2).to_be_bytes());
            absent.bytes[payload+8..payload+12].copy_from_slice(&99u32.to_be_bytes());
            break;
        }
    }
    assert!(validate_rolling_dvh1_fragment(&init, &absent).is_err());
}

#[test]
fn rolling_dvh1_reader_rejects_replacement_init_after_arming() {
    let (init, fragments, _) = read_all(&pipe("open-gop"));
    let mut reader = FragmentReader::new();
    reader.push(&init.bytes);
    assert!(matches!(reader.next_unit().expect("first init"),Some(Unit::Init(_))));
    reader.reject_reinitialization();
    reader.push(&fragments[0].bytes);
    assert!(matches!(reader.next_unit().expect("first fragment"),Some(Unit::Fragment(_))));
    reader.push(&init.bytes);
    assert!(reader.next_unit().is_err());
}
