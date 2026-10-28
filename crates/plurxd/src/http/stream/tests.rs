use super::*;

#[test]
fn a05_candidate_catalog_never_promotes_legacy_prior_provenance() {
    let prior = plurx_core::domain::NetworkPrior {
        credential_generation: Default::default(),
        client_class: "web".to_owned(),
        network_fingerprint: "candidate-isolation".to_owned(),
        sustained_kbps: Some(1),
        worst_rung_height: Some(2160),
        starved_at_ms: Some(i64::MAX),
        sample_count: 99,
        updated_at_ms: i64::MAX,
        link_worst_rung_height: Some(2160),
        link_starved_at_ms: Some(i64::MAX),
    };
    assert!(prior_for_candidate_policy(Some(&prior), true).is_none());
    let legacy = prior_for_candidate_policy(Some(&prior), false)
        .expect("legacy consumers retain their unchanged prior");
    assert!(std::ptr::eq(legacy, &prior));
    assert_eq!(legacy.sustained_kbps, Some(1));
    assert!(prior_for_candidate_policy(None, false).is_none());
}

#[test]
fn flat_audio_channel_claim_is_bounded_at_the_request_boundary() {
    for value in ["0", "17", "255"] {
        assert!(
            serde_urlencoded::from_str::<Caps>(&format!("achannels={value}")).is_err(),
            "achannels={value} must not reach capability translation"
        );
    }
    assert_eq!(
        serde_urlencoded::from_str::<Caps>("achannels=16")
            .expect("the documented ceiling is valid")
            .achannels,
        Some(16)
    );
}

#[test]
fn progressive_aac_conversion_is_pinned_to_forty_eight_khz() {
    assert_eq!(
        progressive_audio_args(true),
        ["-c:a", "aac", "-ac", "2", "-b:a", "256k", "-ar", "48000"]
    );
    assert_eq!(progressive_audio_args(false), ["-c:a", "copy"]);
}

#[test]
fn progressive_query_preserves_the_existing_flat_audio_channel_claim() {
    let query: StreamQuery =
        serde_urlencoded::from_str("acodec=aac&achannels=2").expect("bounded flat claim");
    let caps = query.caps();
    assert_eq!(caps.achannels, Some(2));
    let source = plurx_core::domain::AudioStream {
        codec: "aac".into(),
        channels: Some(6),
        sample_rate: Some(48_000),
        ..Default::default()
    };
    let audio = plurx_core::playback::audio::resolve_audio(
        Some(&source),
        &caps.profile(NOW_MS),
        plurx_core::playback::audio::AudioRoute::Progressive,
        0,
    );
    assert!(matches!(
        audio.action,
        plurx_core::playback::audio::AudioAction::Encode { channels: 2, .. }
    ));
    let mut argv = Vec::new();
    plurx_core::transcode::push_audio_delivery_args(&mut argv, &audio, false);
    assert!(argv.windows(2).any(|pair| pair == ["-ac", "2"]));
    for value in [0, 17] {
        assert!(serde_urlencoded::from_str::<StreamQuery>(&format!("achannels={value}")).is_err());
    }
    assert!(serde_urlencoded::from_str::<StreamQuery>("")
        .expect("legacy query")
        .caps()
        .achannels
        .is_none());
}

/// A fixed clock for every test that builds a device profile.
///
/// `Caps::profile` takes the time because it decides which of a client's
/// learned limits still apply. A test that read the real clock would pass
/// today and fail whenever a fixture's timestamp aged past the browser's
/// re-test window.
const NOW_MS: i64 = 1_756_400_000_000;

#[test]
fn decision_source_frame_rate_preserves_probe_rational_and_absence() {
    let average = r#"{"streams":[
            {"codec_type":"audio","avg_frame_rate":"0/0"},
            {"codec_type":"video","avg_frame_rate":"24000/1001","r_frame_rate":"24/1"}
        ]}"#;
    let fallback = r#"{"streams":[
            {"codec_type":"video","avg_frame_rate":"0/0","r_frame_rate":"30000/1001"}
        ]}"#;
    let absent = r#"{"streams":[{"codec_type":"video"}]}"#;

    assert_eq!(
        source_frame_rate(Some(average)).as_deref(),
        Some("24000/1001")
    );
    assert_eq!(
        source_frame_rate(Some(fallback)).as_deref(),
        Some("30000/1001")
    );
    assert_eq!(source_frame_rate(Some(absent)), None);
    assert_eq!(source_frame_rate(None), None);
}

fn hevc_file(hdr: Option<&str>) -> MediaFile {
    MediaFile {
        downloaded_subtitles: Vec::new(),
        id: 42,
        item_id: 1,
        path: "/movies/hevc.mp4".into(),
        size: 1,
        mtime: 1,
        duration_ms: Some(1_000),
        container: Some("mp4".into()),
        video_codec: Some("hevc".into()),
        video_codec_tag: Some("hvc1".into()),
        field_order: None,
        video_profile: Some("Main 10".into()),
        width: Some(3840),
        height: Some(2160),
        bit_depth: Some(10),
        hdr: hdr.map(str::to_owned),
        hdr_format: None,
        max_cll: None,
        max_fall: None,
        mastering_max_luminance: None,
        luminance_source: None,
        bitrate: Some(20_000_000),
        audio_streams: Vec::new(),
        subtitle_streams: Vec::new(),
        scanned_at: 1,
        audio_offset_ms: 0,
        probed: true,
        dolby_vision: Default::default(),
    }
}

/// PLAYBACK-CAPS-V2-PLAN §4.2: two wire shapes, one translation, one
/// profile.
///
/// The migration's whole risk is here. Every client will send both shapes
/// for at least one release — the v2 body to `/decision`, the legacy
/// `CAPS_Q` on the progressive `play_url` the web still builds — and a
/// device that gets Dolby Vision from one and a tone-mapped SDR ladder
/// from the other is a bug that looks like flaky hardware. So the two
/// shapes are not merely *expected* to agree: they are compared field for
/// field, from a real query string decoded by the same decoder axum's
/// `Query` extractor uses.
#[test]
fn the_legacy_query_and_the_v2_document_build_the_same_profile() {
    const CAPS_Q: &str = "vcodec=hevc,h264&acodec=aac,eac3&achannels=6\
                              &hdr=1&dv=1&dvprofile=5,8&dvhls=1\
                              &hdr10t=1&maxheight=2160";

    let legacy: Caps = serde_urlencoded::from_str(CAPS_Q).expect("CAPS_Q decodes as Caps");
    let from_query = legacy.profile(NOW_MS);

    // The same claims, said the v2 way. `hdr=1` is a *display* fact and
    // `hdr10t=1` is a per-codec presentation fact about HEVC — the legacy
    // query could not tell those apart, which is why the document does.
    let document: playback::DeviceCaps = serde_json::from_str(
        r#"{
                "v": 2,
                "video": [
                  { "codec": "hevc", "present": ["sdr", "pq"], "dv_profiles": [5, 8] },
                  { "codec": "h264", "present": ["sdr"] }
                ],
                "containers": [],
                "audio": ["aac", "eac3"],
                "audio_sinks": [
                  { "codec": "aac", "max_channels": 6 },
                  { "codec": "eac3", "max_channels": 6 }
                ],
                "dv_transport": "hls",
                "display": { "hdr": true, "dolby_vision": true },
                "max_height": 2160
            }"#,
    )
    .expect("the v2 document parses");
    let from_document = playback::DeviceProfile::from_caps_v2(&document);

    assert_eq!(
        from_query, from_document,
        "the legacy query and the v2 document must produce byte-identical \
             profiles; a difference here is a client that gets two different \
             verdicts for one device"
    );

    // Equality alone is `f(g(A)) == f(B)`: both sides end in
    // `from_caps_v2`, so a bug inside that function is invisible to it.
    // Pin the values themselves, from the side that has to be right.
    assert_eq!(from_query.video_codecs, vec!["hevc", "h264"]);
    assert!(from_query.supports_hdr, "hdr=1 is the display bit");
    assert!(
        from_query.supports_hdr10_transcode,
        "hdr10t=1 on a client that lists hevc"
    );
    assert_eq!(from_query.dolby_vision_profiles, vec![5, 8]);
    assert!(
        !from_query.supports_dolby_vision,
        "enumerating profiles retires the blanket claim"
    );
    assert!(from_query.remux_dolby_vision, "dvhls=1");
    assert_eq!(from_query.max_height, Some(2160));
    assert_eq!(from_query.max_audio_channels.get("aac"), Some(&6));
    assert_eq!(from_query.max_audio_channels.get("eac3"), Some(&6));
    assert_eq!(
        from_query
            .presents
            .get("hevc")
            .map(|set| set.iter().copied().collect::<Vec<_>>()),
        Some(vec![playback::Transfer::Sdr, playback::Transfer::Pq]),
        "hdr10t is a per-codec presentation claim, and only hevc made it"
    );
    assert_eq!(
        from_query
            .presents
            .get("h264")
            .map(|set| set.iter().copied().collect::<Vec<_>>()),
        Some(vec![playback::Transfer::Sdr])
    );
}

/// `hdr10t=1` must not become an HDR *display* claim.
///
/// The two bits come from different sources on the out-of-tree clients —
/// Android reads `hdr` off the raw MediaCodec registry and `hdr10t` off
/// `MediaCapabilities` — so `hdr10t=1&hdr=0` is a shape a real device
/// sends. Reading it as "shows HDR" direct-plays a PQ stream into an SDR
/// panel, which renders grey and *plays*, so nobody reports it.
#[test]
fn a_presentation_claim_is_not_a_display_claim() {
    let caps: Caps = serde_urlencoded::from_str("vcodec=hevc&hdr10t=1&maxheight=2160")
        .expect("the query decodes");
    let profile = caps.profile(NOW_MS);
    assert!(
        !profile.supports_hdr,
        "no hdr=1, so this client has not said it can show HDR"
    );
    assert!(
        profile.supports_hdr10_transcode,
        "…but it did prove the Main10 PQ decode, which is a different claim"
    );

    // A v2 document says the same thing the same way: a display block that
    // denies HDR is the authority over what its codecs can emit.
    let document: playback::DeviceCaps = serde_json::from_str(
        r#"{"v":2,"video":[{"codec":"hevc","present":["sdr","pq"]}],
                "audio":[],"containers":[],"display":{"hdr":false}}"#,
    )
    .expect("the v2 document parses");
    let from_document = playback::DeviceProfile::from_caps_v2(&document);
    assert!(!from_document.supports_hdr);
    assert!(from_document.supports_hdr10_transcode);

    // With no display block at all there is nothing to overrule the
    // codecs, and a client that presents PQ can show HDR.
    let no_display: playback::DeviceCaps = serde_json::from_str(
        r#"{"v":2,"video":[{"codec":"hevc","present":["sdr","pq"]}],
                "audio":[],"containers":[]}"#,
    )
    .expect("the v2 document parses");
    assert!(playback::DeviceProfile::from_caps_v2(&no_display).supports_hdr);
}

/// A document is diagnostic data before it is a contract.
///
/// Every field a client might get wrong has to degrade to "not claimed",
/// never to a refused create. The three shapes here are the ones the plan
/// itself makes likely: `learned_limits` serialized from a map whose KEY
/// was the identity, a transfer curve this build has not heard of, and a
/// timestamp spelled the way section 4.6 spells it.
#[test]
fn an_imperfect_capabilities_document_degrades_instead_of_refusing() {
    let caps: playback::DeviceCaps = serde_json::from_str(
        r#"{"v":2,
                "video":[{"codec":"hevc","present":["sdr","pq","hlg10"]}],
                "audio":["aac"],"containers":["mp4"],
                "learned_limits":[{"lost":41,"secs":60,"rate":41,"at":1756400000000}]}"#,
    )
    .expect("an unknown curve and a keyless learned limit must not fail the document");

    let profile = playback::DeviceProfile::from_caps_v2(&caps);
    assert!(
        profile.supports_hdr10_transcode,
        "the recognised curves still count"
    );
    assert_eq!(profile.learned_limits.len(), 1);
    assert_eq!(profile.learned_limits[0].identity, "");
    assert_eq!(
        profile.learned_limits[0].at_ms, 1_756_400_000_000,
        "section 4.6 spells the timestamp `at`"
    );
    assert!(
        profile.presents["hevc"].contains(&playback::Transfer::Unknown),
        "the unknown curve is carried and grades nothing"
    );
}

#[test]
fn decoder_bound_is_negotiated_and_legacy_documents_remain_admissible() {
    for count in [63, 64, 65] {
        let mut caps: playback::DeviceCaps = serde_json::from_value(serde_json::json!({
                "v": 2, "video": (0..count).map(|_| serde_json::json!({"codec": "h264", "present": ["sdr"]})).collect::<Vec<_>>()
            })).expect("decoder document");
        assert!(validate_device_caps(&caps).is_ok(), "legacy count {count}");
        caps.decoder_compaction = Some(playback::DECODER_COMPACTION_CONTRACT.to_owned());
        assert_eq!(validate_device_caps(&caps).is_ok(), count <= 64);
        assert_eq!(
            crate::playback_control::DecoderCapsSnapshot::from_device_caps(&caps, 1).is_ok(),
            count <= 64
        );
    }
}

#[test]
fn ambiguous_audio_sink_claims_are_refused_before_they_can_steer_playback() {
    let caps = playback::DeviceCaps {
        audio_sinks: vec![
            playback::AudioSink {
                codec: "eac3".into(),
                max_channels: 6,
                passthrough: true,
                sample_rates_hz: vec![48_000],
            },
            playback::AudioSink {
                codec: "EAC3".into(),
                max_channels: 2,
                passthrough: false,
                sample_rates_hz: vec![48_000],
            },
        ],
        ..Default::default()
    };
    assert!(validate_device_caps(&caps).is_err());
}

/// The one legacy claim that is deliberately unspellable in v2.
///
/// `dv=1` with no `dvprofile` meant "every profile, including the
/// dual-layer ones no consumer decoder takes". Safari sent exactly that
/// and got Profile 7 preserved — which changed the badge and never
/// rendered. It has to keep working byte-for-byte for one release, and it
/// has to be impossible to say in the new shape, or the deprecation never
/// lands.
#[test]
fn the_blanket_dolby_vision_claim_survives_translation_and_has_no_v2_spelling() {
    let blanket: Caps = serde_urlencoded::from_str("vcodec=hevc&dv=1").expect("the query decodes");
    let profile = blanket.profile(NOW_MS);
    assert!(
        profile.supports_dolby_vision,
        "a client that sent the blanket claim yesterday gets the same answer today"
    );
    assert!(
        profile.dolby_vision_profiles.is_empty(),
        "the blanket claim enumerates nothing; it is a separate bit"
    );

    // Enumerating profiles retires the blanket claim, exactly as the
    // legacy rule at the old `stream.rs:286` did.
    let enumerated: Caps =
        serde_urlencoded::from_str("vcodec=hevc&dv=1&dvprofile=5").expect("the query decodes");
    let enumerated = enumerated.profile(NOW_MS);
    assert!(!enumerated.supports_dolby_vision);
    assert_eq!(enumerated.dolby_vision_profiles, vec![5]);

    // And there is no JSON that sets it: the field is `serde(skip)`.
    let claimed: playback::DeviceCaps = serde_json::from_str(
        r#"{"v":2,"legacy_blanket_dolby_vision":true,
                "video":[{"codec":"hevc"}],"containers":[],"audio":[]}"#,
    )
    .expect("unknown-to-v2 keys are ignored, not refused");
    assert!(
        !playback::DeviceProfile::from_caps_v2(&claimed).supports_dolby_vision,
        "a v2 client cannot claim every Dolby Vision profile"
    );
}

#[test]
fn progressive_minimal_hevc_uses_an_in_band_sample_entry() {
    let file = hevc_file(None);
    let args = progressive_hevc_copy_args(&file, false, false, true).join(" ");
    assert!(args.contains("-tag:v hev1"), "{args}");
    assert!(!args.contains("-strict"), "{args}");
    assert!(!args.contains("remove_types=32-34"), "{args}");
}

#[test]
fn progressive_minimal_dolby_vision_keeps_its_in_band_identity() {
    let file = hevc_file(Some("dolby_vision"));
    let args = progressive_hevc_copy_args(&file, true, true, true).join(" ");
    assert!(args.contains("-tag:v dvhe"), "{args}");
    assert!(args.contains("-strict unofficial"), "{args}");
    assert!(!args.contains("-bsf:v"), "{args}");

    let stripped = progressive_hevc_copy_args(&file, true, false, true).join(" ");
    assert!(stripped.contains("-tag:v hev1"), "{stripped}");
    assert!(
        stripped.contains("dovi_rpu=strip=1,filter_units=remove_types=62-63"),
        "{stripped}"
    );
    assert!(!stripped.contains("-strict"), "{stripped}");
}

#[test]
fn minimal_hvcc_hvc1_only_requires_hls() {
    let caps = playback::DeviceCaps {
        v: playback::DeviceCaps::VERSION,
        transports: vec!["hls".into()],
        progressive_hevc_sample_entries: Some(vec!["hvc1".into()]),
        ..Default::default()
    };
    assert!(hevc_copy_requires_hls(&hevc_file(None), &caps, false, true)
        .expect("HLS is an admitted fallback"));
}

#[test]
fn progressive_dolby_vision_output_tag_matches_the_builder() {
    let file = hevc_file(Some("dolby_vision"));
    assert_eq!(
        progressive_hevc_output_tag(&file, true, false),
        Some("dvh1"),
        "complete hvcC follows the progressive builder's preserved-DV branch"
    );
    assert_eq!(
        progressive_hevc_output_tag(&file, true, true),
        Some("dvhe"),
        "minimal hvcC follows the builder's in-band preserved-DV branch"
    );
    assert_eq!(
        progressive_hevc_output_tag(&file, false, true),
        Some("hev1"),
        "stripping DV leaves the in-band ordinary HEVC sample entry"
    );
}

#[test]
fn android_dv_delivery_native_progressive_packaging_uses_structured_source_facts() {
    let mut p8 = hevc_file(Some("dolby_vision"));
    p8.dolby_vision.profile = Some(8);
    p8.dolby_vision.bl_compat_id = Some(1);
    p8.dolby_vision.rpu_present = Some(true);
    let p8_args = progressive_hevc_copy_args(&p8, true, true, false).join(" ");
    assert!(p8_args.contains("-tag:v hvc1"), "{p8_args}");
    assert!(p8_args.contains("-strict unofficial"), "{p8_args}");

    let mut p5 = p8.clone();
    p5.dolby_vision.profile = Some(5);
    p5.dolby_vision.bl_compat_id = Some(0);
    let p5_args = progressive_hevc_copy_args(&p5, true, true, false).join(" ");
    assert!(p5_args.contains("-tag:v dvh1"), "{p5_args}");
    assert!(p5_args.contains("-strict unofficial"), "{p5_args}");

    let stripped = progressive_hevc_copy_args(&p8, true, false, false).join(" ");
    assert!(stripped.contains("-tag:v hvc1"), "{stripped}");
    assert!(!stripped.contains("-strict"), "{stripped}");
}

#[test]
fn android_dv_delivery_native_progressive_mux_retains_record_and_rpus() {
    use plurx_core::fmp4::{self, FragmentReader, Unit};
    use std::process::Command;

    plurx_core::testfixtures::require_ffmpeg();
    for (profile, compatibility, expected_tag, expected_box) in
        [(8u8, 1u8, "hvc1", b"dvvC"), (5u8, 0u8, "dvh1", b"dvcC")]
    {
        // A DV record must describe the actual base layer: Jellyfin's
        // muxer rejects an 8-bit SDR source labelled P8.1 or P5. These
        // committed CC0 controls carry HDR10 Main10 and P5 IPT-PQ-C2
        // respectively; the P5 control already has matching P5 RPUs.
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/macos-processing")
            .join(if profile == 8 {
                "hdr10.mp4"
            } else {
                "strict_p5_fresh.mp4"
            });
        let mut fixture_command = Command::new(crate::ffmpeg::ffmpeg_bin());
        fixture_command
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(fixture)
            .args([
                "-map",
                "0:v:0",
                "-an",
                "-sn",
                "-c:v",
                "copy",
                "-tag:v",
                expected_tag,
                "-strict",
                "unofficial",
                "-bsf:v",
                "filter_units=remove_types=32-34",
                "-movflags",
                "frag_keyframe+empty_moov+default_base_moof+delay_moov",
                "-f",
                "mp4",
                "pipe:1",
            ]);
        let feed = plurx_core::testfixtures::run(&mut fixture_command);
        let feed = if profile == 8 {
            plurx_core::testfixtures::with_dolby_vision_rpus(&feed)
        } else {
            feed
        };
        let mut reader = FragmentReader::new();
        reader.push(&feed);
        let Some(Unit::Init(mut source_init)) = reader.next_unit().expect("source init") else {
            panic!("fixture must begin with an init");
        };
        let video = source_init.video().expect("source video track");
        let video_id = video.id;
        let nal_length_size = video.nal_length_size;
        let record = fmp4::DolbyVisionRecord::new(profile, 6, false, true, true, compatibility)
            .expect("native DV record");
        fmp4::set_dolby_vision_record(&mut source_init, &record).expect("write source DV record");
        let mut source_bytes = source_init.bytes.clone();
        while let Some(unit) = reader.next_unit().expect("source media") {
            if let Unit::Fragment(mut fragment) = unit {
                if profile == 8 {
                    // Match the injected RPUs to the single HDR10 layer
                    // rather than merely relabelling Profile 7 metadata.
                    fmp4::rewrite_video_samples(
                        &mut fragment,
                        &source_init.tracks,
                        video_id,
                        |sample| {
                            let mut converted = Vec::new();
                            plurx_core::transcode::dvconvert::convert_length_prefixed(
                                sample,
                                nal_length_size,
                                &mut converted,
                            )
                            .expect("convert fixture RPUs to P8.1");
                            Ok(converted)
                        },
                    )
                    .expect("rewrite P8.1 fixture samples");
                }
                source_bytes.extend_from_slice(&fragment.bytes);
            }
        }

        let base = crate::test_tempdir().expect("native DV progressive fixture");
        let input = base.path().join(format!("p{profile}-source.mp4"));
        let output = base.path().join(format!("p{profile}-progressive.mp4"));
        std::fs::write(&input, source_bytes).expect("write native DV source");

        let mut media = hevc_file(Some("dolby_vision"));
        media.path = input.clone();
        media.dolby_vision.profile = Some(i64::from(profile));
        media.dolby_vision.level = Some(6);
        media.dolby_vision.bl_compat_id = Some(i64::from(compatibility));
        media.dolby_vision.el_present = Some(false);
        media.dolby_vision.rpu_present = Some(true);

        let mut command = Command::new(crate::ffmpeg::ffmpeg_bin());
        command
            .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
            .arg(&input)
            .args(["-map", "0:v:0", "-an", "-sn", "-c:v", "copy"])
            .args(progressive_hevc_copy_args(&media, false, true, false))
            .args([
                "-avoid_negative_ts",
                "make_zero",
                "-movflags",
                "frag_keyframe+empty_moov+default_base_moof+delay_moov",
                "-f",
                "mp4",
            ])
            .arg(&output);
        let result = command.output().expect("run progressive mux");
        assert!(
            result.status.success(),
            "P{profile} mux failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );

        let output_bytes = std::fs::read(&output).expect("read progressive output");
        let mut output_reader = FragmentReader::new();
        output_reader.push(&output_bytes);
        let Some(Unit::Init(output_init)) = output_reader.next_unit().expect("output init") else {
            panic!("P{profile} output must begin with a complete init");
        };
        assert!(
            output_init
                .bytes
                .windows(4)
                .any(|window| window == expected_tag.as_bytes()),
            "P{profile} output must use {expected_tag}"
        );
        assert!(
            output_init
                .bytes
                .windows(4)
                .any(|window| window == expected_box),
            "P{profile} output must carry {}",
            String::from_utf8_lossy(expected_box)
        );
        let output_record = fmp4::dolby_vision_record(&output_init)
            .expect("parse output record")
            .expect("output record present");
        assert_eq!(output_record.profile, profile);
        assert_eq!(output_record.bl_signal_compatibility_id, compatibility);
        assert!(!output_record.el_present);
        assert!(output_record.rpu_present);

        let Some(Unit::Fragment(mut fragment)) =
            output_reader.next_unit().expect("output fragment")
        else {
            panic!("P{profile} output must carry a complete media fragment");
        };
        let video = output_init.video().expect("video track");
        let width = usize::from(video.nal_length_size);
        let mut rpus = 0usize;
        fmp4::rewrite_video_samples(&mut fragment, &output_init.tracks, video.id, |sample| {
            let mut at = 0usize;
            while at + width <= sample.len() {
                let mut declared = 0usize;
                for byte in &sample[at..at + width] {
                    declared = (declared << 8) | usize::from(*byte);
                }
                at += width;
                if declared == 0 || at + declared > sample.len() {
                    break;
                }
                if (sample[at] >> 1) & 0x3f == 62 {
                    rpus += 1;
                }
                at += declared;
            }
            Ok(sample.to_vec())
        })
        .expect("inspect output samples");
        assert!(rpus > 0, "P{profile} progressive output must retain RPUs");
    }
}

fn android_dv_p7_source() -> MediaFile {
    let mut file = hevc_file(Some("dolby_vision"));
    file.hdr_format = Some("Dolby Vision · Profile 7 (HDR10-compatible)".into());
    file.dolby_vision.profile = Some(7);
    file.dolby_vision.level = Some(6);
    file.dolby_vision.bl_compat_id = Some(1);
    file.dolby_vision.el_present = Some(true);
    file.dolby_vision.rpu_present = Some(true);
    file
}

fn android_dv_caps(transports: &[&str]) -> playback::DeviceCaps {
    playback::DeviceCaps {
        v: playback::DeviceCaps::VERSION,
        video: vec![playback::VideoCaps {
            codec: "hevc".into(),
            profiles: vec!["main10".into()],
            max_height: Some(2160),
            present: vec![playback::Transfer::Sdr, playback::Transfer::Pq],
            dv_profiles: vec![5, 8],
            ..Default::default()
        }],
        containers: vec!["mp4".into()],
        transports: transports.iter().map(|value| (*value).to_owned()).collect(),
        display: Some(playback::caps::DisplayCaps {
            hdr: true,
            dolby_vision: true,
            max_nits: None,
            peak_source: None,
            presentation_target: None,
        }),
        ..Default::default()
    }
}

#[test]
fn android_dv_delivery_conversion_requires_an_available_hls_producer() {
    let source = android_dv_p7_source();
    let hls = android_dv_caps(&["hls", "progressive"]);
    let progressive = android_dv_caps(&["progressive"]);

    let converted = Caps {
        caps_v2: Some(hls.clone()),
        ..Default::default()
    }
    .decide(
        &source,
        &decision_render_caps(playback::RenderCaps::proven(true), Some(&hls)),
        NOW_MS,
    );
    assert!(converted.convert_dolby_vision);
    assert!(converted.preserve_dolby_vision);
    assert!(
        remux_requires_hls(&source, &converted, Some(&hls), false).expect("copy HLS is admitted")
    );

    let stripped = Caps {
        caps_v2: Some(progressive.clone()),
        ..Default::default()
    }
    .decide(
        &source,
        &decision_render_caps(playback::RenderCaps::proven(true), Some(&progressive)),
        NOW_MS,
    );
    assert!(!stripped.convert_dolby_vision);
    assert!(!stripped.preserve_dolby_vision);
    assert_eq!(stripped.delivered_dynamic_range, "hdr10");
    assert_eq!(stripped.delivered_dolby_vision_profile, None);

    let legacy = Caps {
        vcodec: Some("hevc".into()),
        container: Some("mp4".into()),
        hdr: Some(1),
        dvprofile: Some("5,8".into()),
        hdr10t: Some(1),
        ..Default::default()
    }
    .decide(
        &source,
        &decision_render_caps(playback::RenderCaps::proven(true), None),
        NOW_MS,
    );
    assert!(legacy.convert_dolby_vision);
    assert!(remux_requires_hls(&source, &legacy, None, false)
        .expect("legacy transport is not an explicit refusal"));
}

#[test]
fn android_dv_delivery_converted_vod_identity_differs_from_preserved_profile_seven() {
    let source = android_dv_p7_source();
    let preserved = plurx_core::transcode::CopyVideoOptions::new(true, true);
    let converted = preserved.with_dolby_vision_conversion(true);

    assert_ne!(
        crate::fragindex::identity_for(&source, preserved),
        crate::fragindex::identity_for(&source, converted),
        "a converted decision must not report the preserved Profile 7 index as ready"
    );
}

#[test]
fn progressive_only_incompatible_hevc_returns_409() {
    let caps = playback::DeviceCaps {
        v: playback::DeviceCaps::VERSION,
        transports: vec!["progressive".into()],
        progressive_hevc_sample_entries: Some(vec!["hvc1".into()]),
        ..Default::default()
    };
    let error = hevc_copy_requires_hls(&hevc_file(None), &caps, false, true)
        .expect_err("hev1 is not admitted and HLS was not claimed");
    assert!(matches!(
        error,
        ApiError::Typed {
            status: StatusCode::CONFLICT,
            code: "unsupported_hevc_delivery",
            ..
        }
    ));
}

#[test]
fn compatible_progressive_hevc_does_not_require_hls() {
    let caps = playback::DeviceCaps {
        v: playback::DeviceCaps::VERSION,
        progressive_hevc_sample_entries: Some(vec!["hev1".into()]),
        ..Default::default()
    };
    assert!(
        !hevc_copy_requires_hls(&hevc_file(None), &caps, false, true)
            .expect("the actual progressive tag is admitted")
    );

    let hvc1_caps = playback::DeviceCaps {
        v: playback::DeviceCaps::VERSION,
        progressive_hevc_sample_entries: Some(vec!["hvc1".into()]),
        ..Default::default()
    };
    assert!(
        !hevc_copy_requires_hls(&hevc_file(None), &hvc1_caps, false, false)
            .expect("the complete-hvcC progressive output is admitted")
    );
}

fn planned(method: playback::PlaybackMethod) -> Decision {
    Decision {
        convert_dolby_vision: false,
        method,
        reasons: Vec::new(),
        transcode_audio: true,
        delivered_audio: playback::resolve_audio(
            None,
            playback::default_profile(),
            playback::AudioRoute::RollingHls,
            0,
        ),
        preserve_dolby_vision: true,
        container: "mp4",
        delivered_dynamic_range: "dolby_vision",
        delivered_dolby_vision_profile: Some(8),
        transcode_grade: plurx_core::transcode::OutputGrade::Sdr,
    }
}

/// `/decision` must put the delivered Dolby Vision profile at the *top*
/// level of its body, because that is where both mobile clients read it
/// (`decision.delivered_dolby_vision_profile` in Android's `DecisionDto`
/// and Apple's `Decision`). It gets there only through the
/// `#[serde(flatten)]` on `DecisionResponse::decision`, and nothing else
/// in the tree asserts that flatten holds: the core crate proves the
/// field is *computed* and the client suites prove a body carrying it is
/// *decoded*, with the server's own serialization the untested seam
/// between them. Drop the flatten — or mark the field `#[serde(skip)]`,
/// as its neighbor `transcode_grade` already is — and every badge goes
/// quietly inert with no test to say so.
#[test]
fn decision_body_carries_the_delivered_dolby_vision_profile_at_top_level() {
    fn body(decision: Decision) -> serde_json::Value {
        let (play_url, delivery) = delivery_plan(42, &decision, None, false);
        serde_json::to_value(DecisionResponse {
            display_aware_auto_protocol: Some("route-v1".to_owned()),
            quality_candidate_id: None,
            quality_candidates: None,
            measured_candidate_outputs: None,
            file_id: 42,
            vod_indexed: false,
            decision,
            play_url,
            delivery,
            source: SourceSummary {
                container: None,
                video_codec: None,
                video_profile: None,
                width: None,
                height: None,
                bit_depth: None,
                hdr: None,
                hdr_format: None,
                dv_profile: None,
                dv_el_present: None,
                bitrate: None,
                duration_ms: None,
                frame_rate: None,
            },
            audio: Vec::new(),
            subtitles: Vec::new(),
            selection: None,
            markers: Vec::new(),
            audio_offset_ms: 0,
            declared_offset_ms: None,
            ladder: Vec::new(),
            prior_kbps: None,
            prefer_segmented: None,
        })
        .expect("serialize decision body")
    }

    let converted = body(planned(playback::PlaybackMethod::Remux));
    assert_eq!(
        converted.pointer("/delivered_audio/action/kind"),
        Some(&serde_json::json!("none")),
        "the independently resolved audio contract is flattened beside the verdict: {converted}"
    );
    assert_eq!(
        converted.get("delivered_dolby_vision_profile"),
        Some(&serde_json::json!(8)),
        "clients read this key off the body root: {converted}"
    );
    assert!(
        converted.get("decision").is_none(),
        "the verdict is flattened, not nested — a nested `decision` object \
             means every client stopped seeing all of it: {converted}"
    );

    // Absent means "this delivery carries no Dolby Vision", which is not
    // the same as a serializer that dropped the field: the sibling range
    // readout still has to be there to prove the difference.
    let mut no_dv = planned(playback::PlaybackMethod::Remux);
    no_dv.delivered_dolby_vision_profile = None;
    no_dv.delivered_dynamic_range = "hdr10";
    let stripped = body(no_dv);
    assert!(
        stripped.get("delivered_dolby_vision_profile").is_none(),
        "{stripped}"
    );
    assert_eq!(
        stripped.get("delivered_dynamic_range"),
        Some(&serde_json::json!("hdr10")),
        "{stripped}"
    );
}

#[test]
fn every_verdict_has_one_server_owned_execution_plan() {
    let (legacy, direct) = delivery_plan(
        42,
        &planned(playback::PlaybackMethod::DirectPlay),
        None,
        false,
    );
    assert_eq!(legacy, "/api/v1/files/42/direct");
    assert!(matches!(
        direct,
        DeliveryPlan::Direct { url } if url == "/api/v1/files/42/direct"
    ));

    let (legacy, remux) = delivery_plan(42, &planned(playback::PlaybackMethod::Remux), None, false);
    assert_eq!(legacy, "/api/v1/files/42/stream.mp4");
    assert!(matches!(
        remux,
        DeliveryPlan::Remux {
            url,
            sessions_url,
            aac: true,
            requires_hls: false,
            preserve_dolby_vision: true,
            audio: None,
        } if url == "/api/v1/files/42/stream.mp4"
            && sessions_url == "/api/v1/files/42/hls/sessions"
    ));

    let (legacy, transcode) = delivery_plan(
        42,
        &planned(playback::PlaybackMethod::Transcode),
        None,
        false,
    );
    assert_eq!(legacy, "/api/v1/files/42/stream.mp4");
    assert!(matches!(
        transcode,
        DeliveryPlan::Transcode { sessions_url, audio: None }
            if sessions_url == "/api/v1/files/42/hls/sessions"
    ));
}

#[test]
fn remux_plan_serializes_the_hls_requirement_only_when_true() {
    let (_, ordinary) = delivery_plan(42, &planned(playback::PlaybackMethod::Remux), None, false);
    let ordinary = serde_json::to_value(ordinary).expect("serialize ordinary remux");
    assert!(ordinary.get("requires_hls").is_none(), "{ordinary}");

    let (_, constrained) = delivery_plan(42, &planned(playback::PlaybackMethod::Remux), None, true);
    let constrained = serde_json::to_value(constrained).expect("serialize constrained remux");
    assert_eq!(
        constrained.get("requires_hls"),
        Some(&serde_json::json!(true)),
        "{constrained}"
    );
}

/// The loop review finding 1 named: a plan is only executable if following
/// it delivers the track the caller selected. Feed the plan's own URL back
/// through the remux endpoint's extractor and track rule, and the answer
/// must be the selection — not the language policy the bare URL re-derives.
#[test]
fn a_selection_aware_plan_url_delivers_the_selected_track() {
    use plurx_core::domain::AudioStream;

    let tracks = vec![
        AudioStream {
            index: 0,
            codec: "aac".into(),
            language: Some("eng".into()),
            default: true,
            ..Default::default()
        },
        AudioStream {
            index: 1,
            codec: "aac".into(),
            language: Some("fre".into()),
            default: false,
            ..Default::default()
        },
    ];
    let prefs = plurx_core::tracks::LangPrefs {
        audio_lang: "eng".into(),
        ..Default::default()
    };
    // The policy default is track 0, so a bare plan URL would deliver it.
    assert_eq!(remux_audio_index(&tracks, None, &prefs), 0);

    let (play_url, remux) = delivery_plan(
        42,
        &planned(playback::PlaybackMethod::Remux),
        Some(1),
        false,
    );
    assert_eq!(play_url, "/api/v1/files/42/stream.mp4?audio=1");
    let DeliveryPlan::Remux { url, audio, .. } = remux else {
        panic!("a remux verdict must plan a remux");
    };
    assert_eq!(audio, Some(1));

    let uri: axum::http::Uri = url.parse().expect("the plan URL is a valid URI");
    let Query(followed) = Query::<StreamQuery>::try_from_uri(&uri)
        .expect("the remux endpoint's own extractor accepts the plan URL it was handed");
    assert_eq!(
        remux_audio_index(&tracks, followed.audio, &prefs),
        1,
        "following the plan must deliver the selected French track, not the policy default"
    );

    let (_, transcode) = delivery_plan(
        42,
        &planned(playback::PlaybackMethod::Transcode),
        Some(1),
        false,
    );
    assert!(
        matches!(transcode, DeliveryPlan::Transcode { audio: Some(1), .. }),
        "an HLS session create takes the selection in its body, so the plan carries it"
    );
}

#[test]
fn remux_omission_uses_the_preferred_audio_instead_of_stream_zero() {
    use plurx_core::domain::AudioStream;

    let track = |index: i64, language: &str, default: bool| AudioStream {
        index,
        codec: "eac3".into(),
        channels: Some(6),
        sample_rate: Some(48_000),
        channel_layout: None,
        language: Some(language.into()),
        title: None,
        default,
    };
    // Scary Movie's shape: the mux calls Italian default, while the server
    // preference and `/decision` select English stream 2.
    let audio = vec![
        track(0, "ita", true),
        track(1, "ita", true),
        track(2, "eng", false),
        track(3, "eng", false),
    ];
    let prefs = plurx_core::tracks::LangPrefs::default();

    assert_eq!(remux_audio_index(&audio, None, &prefs), 2);
    assert_eq!(remux_audio_index(&audio, Some(3), &prefs), 3);
}

#[test]
fn preferred_truehd_audio_is_transcoded_for_android_hdr_remux() {
    use plurx_core::domain::AudioStream;

    let mut file = MediaFile {
        downloaded_subtitles: Vec::new(),
        id: 5698,
        item_id: 1,
        path: "/movies/Michael (2026).mkv".into(),
        size: 1,
        mtime: 1,
        duration_ms: Some(120_000),
        container: Some("mkv".into()),
        video_codec: Some("hevc".into()),
        video_codec_tag: None,
        field_order: None,
        video_profile: Some("Main 10".into()),
        width: Some(3840),
        height: Some(2160),
        bit_depth: Some(10),
        hdr: Some("dolby_vision".into()),
        hdr_format: Some("Dolby Vision · Profile 7 (HDR10-compatible)".into()),
        max_cll: None,
        max_fall: None,
        mastering_max_luminance: None,
        luminance_source: None,
        bitrate: Some(90_892_368),
        // The columns, not only the label. The conversion needs the level
        // and the compatibility id as numbers to build the configuration
        // record its output declares, so a label-only row is deliberately
        // not converted — see `file_can_convert_to_p81`.
        audio_streams: vec![
            AudioStream {
                index: 0,
                codec: "eac3".into(),
                channels: Some(8),
                sample_rate: Some(48_000),
                channel_layout: None,
                language: Some("fra".into()),
                title: Some("French E-AC-3".into()),
                default: true,
            },
            AudioStream {
                index: 3,
                codec: "truehd".into(),
                channels: Some(8),
                sample_rate: Some(48_000),
                channel_layout: None,
                language: Some("eng".into()),
                title: Some("English TrueHD Atmos".into()),
                default: false,
            },
        ],
        subtitle_streams: vec![],
        scanned_at: 1,
        audio_offset_ms: 0,
        probed: true,
        dolby_vision: plurx_core::domain::DolbyVisionFacts {
            profile: Some(7),
            level: Some(6),
            bl_compat_id: Some(1),
            ..Default::default()
        },
    };
    let caps = Caps {
        vcodec: Some("h264,hevc,av1,vp9".into()),
        acodec: Some("aac,mp3,opus,flac,ac3,eac3".into()),
        container: Some("mp4,webm".into()),
        hdr: Some(1),
        dvprofile: Some("5,8".into()),
        ..Default::default()
    };
    let prefs = plurx_core::tracks::LangPrefs::default();

    let selected = remux_audio_index(&file.audio_streams, None, &prefs);
    assert_eq!(selected, 3, "English preference selects the TrueHD track");
    set_selected_audio_default(&mut file.audio_streams, Some(selected));
    let english = caps.decide(&file, &playback::RenderCaps::proven(true), NOW_MS);
    assert_eq!(english.method, playback::PlaybackMethod::Remux);
    assert!(english.transcode_audio, "TrueHD must become AAC in MP4");
    // Since M5a a Profile 7 source over an HDR10 base reaches a client
    // that decodes profile 8 as a conversion rather than a strip. This
    // test is about the audio; the range is asserted because the audio
    // decision must not disturb the video one — a track swap re-encoding
    // the picture's grade would be the bug worth catching here.
    assert!(english.convert_dolby_vision);
    assert_eq!(english.delivered_dynamic_range, "dolby_vision");
    assert!(english
        .reasons
        .iter()
        .any(|reason| reason.contains("audio codec truehd unsupported")));

    set_selected_audio_default(&mut file.audio_streams, Some(0));
    let french = caps.decide(&file, &playback::RenderCaps::proven(true), NOW_MS);
    assert!(!french.transcode_audio, "the E-AC-3 alternative can copy");
}

#[test]
fn explicit_dolby_vision_profiles_override_the_legacy_all_profiles_bit() {
    let caps = Caps {
        vcodec: Some("hevc".into()),
        acodec: Some("aac".into()),
        container: Some("mp4".into()),
        hdr: Some(1),
        // Kept for compatibility with an older server, which ignores the
        // new profile list. A new server must prefer the specific list.
        dv: Some(1),
        dvprofile: Some("5,8".into()),
        dvhls: Some(1),
        ..Default::default()
    };

    let profile = caps.profile(NOW_MS);
    assert!(!profile.supports_dolby_vision);
    assert_eq!(profile.dolby_vision_profiles, vec![5, 8]);
    assert!(profile.remux_dolby_vision);
}

#[test]
fn runtime_caps_parse_per_codec_height_limits() {
    let caps = Caps {
        vcodec: Some("h264,hevc,av1".into()),
        acodec: Some("aac".into()),
        container: Some("mp4".into()),
        maxheight: Some(2160),
        vmaxheight: Some(" H264:1080,hevc:2160,av1:bad,empty:0,broken ".into()),
        ..Default::default()
    };

    let profile = caps.profile(NOW_MS);
    assert_eq!(profile.max_height, Some(2160));
    assert_eq!(profile.video_max_heights.get("h264"), Some(&1080));
    assert_eq!(profile.video_max_heights.get("hevc"), Some(&2160));
    assert!(!profile.video_max_heights.contains_key("av1"));
    assert!(!profile.video_max_heights.contains_key("empty"));
}

/// ffmpeg's `-progress` shares stderr with its diagnostics here, because
/// stdout is the MP4. Telling them apart is the whole trick, and getting it
/// wrong in either direction is silent: a misread error line vanishes from
/// the log, a misread progress line does nothing at all.
#[test]
fn progress_blocks_are_distinguishable_from_ffmpegs_prose() {
    for line in [
        "out_time_us=5960000",
        "speed=4.02x",
        "frame=142",
        "progress=continue",
        "bitrate=N/A",
    ] {
        assert!(
            plurx_core::transcode::progress::is_progress_line(line),
            "progress: {line}"
        );
    }
    for line in [
        "[matroska @ 0x55f4] Could not find codec parameters",
        "Error opening input file /media/x.mkv.",
        "Conversion failed!",
        "",
        // An ffmpeg message that happens to contain '=' is still prose:
        // the key side has spaces and capitals, which progress keys never do.
        "[out#0/mp4 @ 0x1] Output file is empty, nothing was encoded",
        "filter_units=remove_types=32-34",
        "some_future_key=1",
    ] {
        assert!(
            !plurx_core::transcode::progress::is_progress_line(line),
            "prose: {line}"
        );
    }
}

#[tokio::test]
async fn tracked_remux_logs_diagnostics_without_advancing_progress() {
    use tokio::io::AsyncWriteExt as _;
    use tracing_subscriber::prelude::*;

    let logs = std::sync::Arc::new(crate::logbuf::LogBuffer::new(8));
    let subscriber = tracing_subscriber::registry()
        .with(crate::logbuf::BufferLayer(std::sync::Arc::clone(&logs)));
    let _guard = tracing::subscriber::set_default(subscriber);
    let progress = std::sync::Arc::new(crate::transcode::Progress::new());
    let generation = progress.begin_attempt();

    let (mut diagnostic_writer, diagnostic_reader) = tokio::io::duplex(256);
    diagnostic_writer
        .write_all(b"filter_units=remove_types=32-34\n")
        .await
        .expect("write diagnostic transcript");
    diagnostic_writer
        .shutdown()
        .await
        .expect("finish diagnostic transcript");
    consume_remux_stderr(
        diagnostic_reader,
        Some((std::sync::Arc::clone(&progress), generation)),
    )
    .await;

    assert_eq!(progress.speed(), None);
    assert_eq!(progress.out_time_ms(), None);
    let captured = logs.tail("trace", 8);
    assert_eq!(captured.len(), 1, "{captured:?}");
    assert!(
        captured[0]
            .message
            .contains("remux ffmpeg: filter_units=remove_types=32-34"),
        "{}",
        captured[0].message
    );

    let (mut progress_writer, progress_reader) = tokio::io::duplex(256);
    progress_writer
        .write_all(b"speed=2.50x\nout_time_us=1250000\n")
        .await
        .expect("write progress transcript");
    progress_writer
        .shutdown()
        .await
        .expect("finish progress transcript");
    consume_remux_stderr(progress_reader, Some((progress.clone(), generation))).await;

    assert_eq!(progress.speed(), Some(2.5));
    assert_eq!(progress.out_time_ms(), Some(1_250));
    assert_eq!(
        logs.tail("trace", 8).len(),
        1,
        "progress lines stay out of logs"
    );
}

/// This is the response-start budget in isolation: an artificially cold
/// origin probe cannot add its full delay before the progressive body is
/// returned. The production call uses the pinned one-second budget; the
/// short test budget keeps the suite fast while exercising the same race.
#[tokio::test]
async fn delayed_origin_probe_cannot_hold_progressive_first_byte_past_budget() {
    assert_eq!(
        PROGRESSIVE_MEDIA_ORIGIN_PROBE_TIMEOUT,
        std::time::Duration::from_secs(1)
    );
    let budget = std::time::Duration::from_millis(20);
    let began = std::time::Instant::now();
    let origin = bounded_progressive_media_origin(42.0, budget, async {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        40.0
    })
    .await;
    assert_eq!(origin, 42.0, "timeout falls back to the requested seek");
    assert!(
        began.elapsed() < std::time::Duration::from_millis(100),
        "the delayed probe held response creation for {:?}",
        began.elapsed()
    );

    assert_eq!(
        bounded_progressive_media_origin(42.0, budget, async { 40.0 }).await,
        40.0,
        "a prompt keyframe answer still reaches the response header"
    );
}

/// A progressive remux owner around a stand-in child, with the serving
/// watch under the test's control and no HTTP body: the owner, not body
/// polling, holds and reaps the child.
fn remux_owner_stand_in() -> (
    tokio::sync::watch::Sender<crate::serving_fence::ServingState>,
    RemuxProcessGuard,
    tokio_util::sync::CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    remux_owner_around("sleep", &["60"])
}

fn remux_owner_around(
    program: &str,
    args: &[&str],
) -> (
    tokio::sync::watch::Sender<crate::serving_fence::ServingState>,
    RemuxProcessGuard,
    tokio_util::sync::CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    let mut command = tokio::process::Command::new(program);
    command.args(args).kill_on_drop(true);
    let child = command.spawn().expect("spawn remux stand-in");
    let child_job = crate::process_control::ChildJob::attach(&child).expect("attach child job");
    let (serving_tx, serving_rx) =
        tokio::sync::watch::channel(crate::serving_fence::ServingState {
            ready: true,
            loss_generation: 0,
        });
    let (guard, fenced, owner) = spawn_remux_process_owner(child, child_job, serving_rx, 0, None);
    (serving_tx, guard, fenced, owner)
}

fn publish_serving(
    serving: &tokio::sync::watch::Sender<crate::serving_fence::ServingState>,
    ready: bool,
    loss_generation: u64,
) {
    // send_replace, because an owner that already ended has dropped its
    // receiver, and a test publishing past that point is not an error.
    serving.send_replace(crate::serving_fence::ServingState {
        ready,
        loss_generation,
    });
}

/// The 2026-10-04 shape on the progressive path: a leader restart costs
/// every voter its serving authority for a second or two. A loss that
/// recovers inside the grace must keep ffmpeg and the response.
#[tokio::test]
async fn remux_owner_keeps_the_child_through_a_brief_loss() {
    let grace = crate::serving_fence::SERVING_FENCE_SESSION_GRACE;
    let (serving, guard, fenced, owner) = remux_owner_stand_in();
    publish_serving(&serving, false, 1);
    tokio::time::sleep(grace / 4).await;
    publish_serving(&serving, true, 1);
    // Well past the point at which a sustained loss would have ended it.
    tokio::time::sleep(grace + grace / 2).await;
    assert!(
        !owner.is_finished(),
        "a brief authority loss must not reap the progressive child"
    );
    assert!(
        !fenced.is_cancelled(),
        "a brief authority loss must not end the response body"
    );
    drop(guard);
    tokio::time::timeout(std::time::Duration::from_secs(10), owner)
        .await
        .expect("dropping the body still ends the owner")
        .expect("owner task must not panic");
}

/// The fence's safety property still holds: a node that has truly lost
/// authority stops serving within the grace, and not before it.
#[tokio::test]
async fn remux_owner_reaps_the_child_after_a_sustained_loss() {
    let grace = crate::serving_fence::SERVING_FENCE_SESSION_GRACE;
    let (serving, _guard, fenced, owner) = remux_owner_stand_in();
    let lost_at = std::time::Instant::now();
    publish_serving(&serving, false, 1);
    tokio::time::timeout(grace + std::time::Duration::from_secs(10), owner)
        .await
        .expect("a sustained loss must reap the child once the grace is spent")
        .expect("owner task must not panic");
    assert!(
        fenced.is_cancelled(),
        "the body learns the owner ended the stream"
    );
    assert!(
        lost_at.elapsed() >= grace,
        "reaped after {:?}, before the {grace:?} grace",
        lost_at.elapsed()
    );
}

/// Recovery, then more losses before the quorum has been stable for a
/// grace: they spend one budget, so a flapping quorum still ends the
/// stream, even though every single loss is shorter than the grace.
#[tokio::test]
async fn remux_owner_losses_after_a_recovery_share_one_grace() {
    let slice = crate::serving_fence::SERVING_FENCE_SESSION_GRACE / 3;
    let (serving, _guard, fenced, owner) = remux_owner_stand_in();
    for generation in 1..=4 {
        publish_serving(&serving, false, generation);
        tokio::time::sleep(slice).await;
        publish_serving(&serving, true, generation);
        tokio::task::yield_now().await;
    }
    // Authority is back now; only the shared budget can end the stream.
    tokio::time::timeout(
        crate::serving_fence::SERVING_FENCE_SESSION_GRACE + std::time::Duration::from_secs(10),
        owner,
    )
    .await
    .expect("losses summing past the grace must reap the child")
    .expect("owner task must not panic");
    assert!(fenced.is_cancelled(), "the body is ended with the child");
}

/// Recovery published before the owner gets a scheduling point leaves it
/// a new generation with authority already restored. Until 2026-10-04
/// this alone reaped the child; the owner now adopts the new generation.
#[tokio::test]
async fn remux_owner_adopts_a_new_generation_when_authority_is_already_back() {
    let grace = crate::serving_fence::SERVING_FENCE_SESSION_GRACE;
    let (serving, guard, fenced, owner) = remux_owner_stand_in();
    publish_serving(&serving, false, 1);
    publish_serving(&serving, true, 1);
    tokio::time::sleep(grace + grace / 2).await;
    assert!(
        !owner.is_finished(),
        "a generation bump with authority restored must not reap the child"
    );
    assert!(!fenced.is_cancelled(), "nor end the response body");
    // The adopted generation is the one later losses are judged against:
    // a sustained loss after it still ends the stream.
    publish_serving(&serving, false, 2);
    tokio::time::timeout(grace + std::time::Duration::from_secs(10), owner)
        .await
        .expect("a sustained loss after recovery still reaps the child")
        .expect("owner task must not panic");
    assert!(fenced.is_cancelled());
    drop(guard);
}

/// Shutdown while authority is lost does not wait out the grace.
#[tokio::test]
async fn remux_owner_reaps_the_child_when_the_fence_closes_during_a_loss() {
    let grace = crate::serving_fence::SERVING_FENCE_SESSION_GRACE;
    let (serving, _guard, fenced, owner) = remux_owner_stand_in();
    publish_serving(&serving, false, 1);
    tokio::task::yield_now().await;
    drop(serving);
    tokio::time::timeout(grace / 2, owner)
        .await
        .expect("a closed fence reaps the child without waiting for the grace")
        .expect("owner task must not panic");
    assert!(fenced.is_cancelled(), "the body is ended with the child");
}

/// ffmpeg finished on its own, and the body may still hold its tail. The
/// owner keeps the authority decision until the body is dropped, so a
/// loss after the exit still ends the body within the grace instead of
/// leaving it waiting for authority forever.
#[tokio::test]
async fn remux_owner_still_decides_for_the_body_after_a_natural_exit() {
    let grace = crate::serving_fence::SERVING_FENCE_SESSION_GRACE;
    let (serving, guard, fenced, owner) = remux_owner_around("true", &[]);
    tokio::time::sleep(grace / 4).await;
    assert!(
        !owner.is_finished(),
        "a natural exit leaves the owner deciding for the body"
    );
    publish_serving(&serving, false, 1);
    tokio::time::timeout(grace + std::time::Duration::from_secs(10), owner)
        .await
        .expect("a sustained loss after the exit still ends the owner")
        .expect("owner task must not panic");
    assert!(fenced.is_cancelled(), "and tells the body to stop waiting");
    drop(guard);

    let (_serving, guard, fenced, owner) = remux_owner_around("true", &[]);
    tokio::time::sleep(grace / 4).await;
    drop(guard);
    tokio::time::timeout(std::time::Duration::from_secs(10), owner)
        .await
        .expect("dropping the body ends an owner whose child already exited")
        .expect("owner task must not panic");
    assert!(!fenced.is_cancelled(), "an ordinary end is not a fence");
}

/// The body publishes only under authority: bytes produced during a loss
/// are held until authority returns, and dropped when the owner gives up.
#[tokio::test]
async fn remux_body_publishes_nothing_while_authority_is_lost() {
    use futures_util::StreamExt as _;
    use tokio::io::AsyncWriteExt as _;

    let (mut writer, reader) = tokio::io::duplex(1024);
    let (serving, serving_rx) = tokio::sync::watch::channel(crate::serving_fence::ServingState {
        ready: true,
        loss_generation: 0,
    });
    let fenced = tokio_util::sync::CancellationToken::new();
    let guard = RemuxProcessGuard {
        cancel: tokio_util::sync::CancellationToken::new(),
    };
    let body = remux_body(reader, None, guard, None, serving_rx, fenced.clone());
    tokio::pin!(body);

    writer.write_all(b"one").await.expect("write");
    let chunk = body.next().await.expect("chunk").expect("bytes");
    assert_eq!(&chunk[..], b"one");

    publish_serving(&serving, false, 1);
    writer.write_all(b"two").await.expect("write");
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(200), body.next())
            .await
            .is_err(),
        "nothing is published while authority is lost"
    );
    publish_serving(&serving, true, 1);
    let chunk = body.next().await.expect("chunk").expect("bytes");
    assert_eq!(
        &chunk[..],
        b"two",
        "held bytes follow once authority returns"
    );

    publish_serving(&serving, false, 2);
    writer.write_all(b"three").await.expect("write");
    fenced.cancel();
    assert!(
        body.next().await.is_none(),
        "an owner that gave up ends the body without publishing what it held"
    );
}

fn headers_with_range(value: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(header::RANGE, value.parse().expect("valid header"));
    h
}

#[test]
fn range_parsing() {
    let len = 1000;
    assert_eq!(
        parse_range(&headers_with_range("bytes=0-99"), len),
        Ok(Some((0, 99)))
    );
    assert_eq!(
        parse_range(&headers_with_range("bytes=100-"), len),
        Ok(Some((100, 999)))
    );
    assert_eq!(
        parse_range(&headers_with_range("bytes=-100"), len),
        Ok(Some((900, 999)))
    );
    // Open end clamps to len-1.
    assert_eq!(
        parse_range(&headers_with_range("bytes=0-99999"), len),
        Ok(Some((0, 999)))
    );
    // Invalid / out of range.
    assert_eq!(
        parse_range(&headers_with_range("bytes=2000-3000"), len),
        Err(())
    );
    assert_eq!(
        parse_range(&headers_with_range("bytes=500-100"), len),
        Err(())
    );
    assert_eq!(parse_range(&HeaderMap::new(), len), Ok(None));
}

#[test]
fn direct_range_handles_empty_files_huge_integers_and_complete_lists() {
    for range in ["bytes=0-", "bytes=0-0", "bytes=-0"] {
        assert_eq!(
            parse_range(&headers_with_range(range), 0),
            Err(()),
            "{range}"
        );
    }
    for range in [
        "bytes=",
        "bytes=, ,",
        "bytes=+1-2",
        "bytes=1-+2",
        "bytes=1 -2",
        "bytes=0-1,bad",
        "bytes=0-1,8-7",
        "bytes=-0",
        "bytes=99-",
        "bytes=0-1,18446744073709551617-18446744073709551616",
    ] {
        assert_eq!(
            parse_range(&headers_with_range(range), 10),
            Err(()),
            "{range}"
        );
    }
    for (range, expected) in [
        ("bytes=99-,2-3", (2, 3)),
        ("bytes=0-1,8-9", (0, 1)),
        ("bytes=, 2-3,", (2, 3)),
        ("Bytes=1-2", (1, 2)),
        ("bytes=00001-00002", (1, 2)),
        ("bytes=0-99999999999999999999999999999999999", (0, 9)),
        ("bytes=-99999999999999999999999999999999999", (0, 9)),
    ] {
        assert_eq!(
            parse_range(&headers_with_range(range), 10),
            Ok(Some(expected)),
            "{range}"
        );
    }
    assert_eq!(
        parse_range(&headers_with_range("bytes=999999999999999999999999-"), 10),
        Err(())
    );
    assert_eq!(parse_range(&headers_with_range("items=0-1"), 10), Ok(None));
    assert_eq!(parse_range(&headers_with_range("bytes=-1"), 0), Ok(None));
    assert_eq!(
        parse_range(&headers_with_range("bytes=-1,broken"), 0),
        Err(())
    );
    assert_eq!(
        parse_range(&headers_with_range("bytes=0-"), u64::MAX),
        Ok(Some((0, u64::MAX - 1)))
    );
    let excessive = format!("bytes={}", vec!["0-1"; 17].join(","));
    assert_eq!(parse_range(&headers_with_range(&excessive), 10), Err(()));
    let mut repeated = headers_with_range("bytes=0-1");
    repeated.append(header::RANGE, HeaderValue::from_static("bytes=2-3"));
    assert_eq!(parse_range(&repeated, 10), Err(()));
}

#[tokio::test]
async fn direct_range_response_has_exact_status_headers_and_bytes() {
    let directory = crate::test_tempdir().expect("temporary raw file");
    let path = directory.path().join("movie.mp4");
    tokio::fs::write(&path, b"0123456789")
        .await
        .expect("write raw fixture");
    for (range, status, content_range, body) in [
        (None, StatusCode::OK, None, "0123456789"),
        (
            Some("bytes=0-1,18446744073709551617-18446744073709551616"),
            StatusCode::RANGE_NOT_SATISFIABLE,
            Some("bytes */10"),
            "",
        ),
        (
            Some("bytes=2-4"),
            StatusCode::PARTIAL_CONTENT,
            Some("bytes 2-4/10"),
            "234",
        ),
        (
            Some("bytes=-3"),
            StatusCode::PARTIAL_CONTENT,
            Some("bytes 7-9/10"),
            "789",
        ),
        (
            Some("bytes=99-,3-4"),
            StatusCode::PARTIAL_CONTENT,
            Some("bytes 3-4/10"),
            "34",
        ),
        (
            Some("bytes=10-"),
            StatusCode::RANGE_NOT_SATISFIABLE,
            Some("bytes */10"),
            "",
        ),
        (
            Some("bytes=bad"),
            StatusCode::RANGE_NOT_SATISFIABLE,
            Some("bytes */10"),
            "",
        ),
        (
            Some("bytes=0-1,bad"),
            StatusCode::RANGE_NOT_SATISFIABLE,
            Some("bytes */10"),
            "",
        ),
        (Some("items=0-1"), StatusCode::OK, None, "0123456789"),
    ] {
        let headers = range.map(headers_with_range).unwrap_or_default();
        let response = serve_file_range(&path, &headers, &Method::GET, None)
            .await
            .expect("range response");
        assert_eq!(response.status(), status, "{range:?}");
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_RANGE)
                .map(|value| value.to_str().expect("ASCII Content-Range")),
            content_range
        );
        assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
        assert_eq!(
            response.headers()[header::CONTENT_LENGTH],
            body.len().to_string()
        );
        let received = axum::body::to_bytes(response.into_body(), 100)
            .await
            .expect("read response");
        assert_eq!(received.as_ref(), body.as_bytes(), "{range:?}");
    }
}

#[tokio::test]
async fn direct_range_empty_head_and_unvalidated_if_range_cannot_underflow_or_send_partial_media() {
    let directory = crate::test_tempdir().expect("temporary raw files");
    let path = directory.path().join("empty.mp4");
    tokio::fs::write(&path, b"").await.expect("empty fixture");
    for (headers, status) in [
        (HeaderMap::new(), StatusCode::OK),
        (headers_with_range("bytes=-1"), StatusCode::OK),
        (headers_with_range("bytes=-1,0-"), StatusCode::OK),
        (
            headers_with_range("bytes=0-"),
            StatusCode::RANGE_NOT_SATISFIABLE,
        ),
        (
            headers_with_range("bytes=-1,broken"),
            StatusCode::RANGE_NOT_SATISFIABLE,
        ),
    ] {
        let response = serve_file_range(&path, &headers, &Method::GET, None)
            .await
            .expect("empty response");
        assert_eq!(response.status(), status);
        assert_eq!(response.headers()[header::CONTENT_LENGTH], "0");
        assert!(axum::body::to_bytes(response.into_body(), 1)
            .await
            .expect("read empty-file response")
            .is_empty());
    }
    tokio::fs::write(&path, b"0123456789")
        .await
        .expect("populated fixture");
    let response = serve_file_range(&path, &headers_with_range("bytes=99-"), &Method::HEAD, None)
        .await
        .expect("HEAD");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_LENGTH], "10");
    assert!(!response.headers().contains_key(header::CONTENT_RANGE));
    assert!(axum::body::to_bytes(response.into_body(), 1)
        .await
        .expect("read HEAD response")
        .is_empty());
    for validator in ["\"unknown\"", "W/\"weak\"", "Fri, 04 Sep 2026 20:00:00 GMT"] {
        let mut headers = headers_with_range("bytes=2-3");
        headers.insert(
            header::IF_RANGE,
            validator.parse().expect("If-Range fixture"),
        );
        let response = serve_file_range(&path, &headers, &Method::GET, None)
            .await
            .expect("If-Range response");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!response.headers().contains_key(header::CONTENT_RANGE));
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 10)
                .await
                .expect("read complete If-Range response")
                .as_ref(),
            b"0123456789"
        );
    }
}

#[test]
fn remux_readrate_accepts_only_finite_nonnegative_values() {
    for value in [
        "NaN", "inf", "+inf", "-inf", "Infinity", "1e999", "-1", "bad", "",
    ] {
        assert_eq!(parse_readrate(value), None, "{value}");
    }
    for (value, expected) in [("0", 0.0), (" 4.0 ", 4.0), ("1.25", 1.25)] {
        assert_eq!(parse_readrate(value), Some(expected));
    }
}

#[test]
fn content_types() {
    assert_eq!(content_type(Path::new("a.mp4")), "video/mp4");
    assert_eq!(content_type(Path::new("a.mkv")), "video/x-matroska");
    assert_eq!(content_type(Path::new("a.webm")), "video/webm");
    assert_eq!(content_type(Path::new("a.m4b")), "audio/mp4");
    assert_eq!(content_type(Path::new("a.aac")), "audio/aac");
    assert_eq!(content_type(Path::new("a.mp3")), "audio/mpeg");
    assert_eq!(content_type(Path::new("a.flac")), "audio/flac");
    assert_eq!(content_type(Path::new("a.epub")), "application/epub+zip");
    assert_eq!(content_type(Path::new("a.pdf")), "application/pdf");
}

#[test]
fn chapter_classification() {
    assert_eq!(classify_chapter("Intro").map(|m| m.0), Some("intro"));
    assert_eq!(classify_chapter("Opening").map(|m| m.0), Some("intro"));
    assert_eq!(classify_chapter("OP").map(|m| m.0), Some("intro"));
    assert_eq!(
        classify_chapter("Previously On").map(|m| m.0),
        Some("intro")
    );
    assert_eq!(
        classify_chapter("End Credits").map(|m| m.0),
        Some("credits")
    );
    assert_eq!(classify_chapter("Ending").map(|m| m.0), Some("credits"));
    assert_eq!(classify_chapter("ED").map(|m| m.0), Some("credits"));
    // "Opening Credits" is the intro, not the end credits.
    assert_eq!(
        classify_chapter("Opening Credits").map(|m| m.0),
        Some("intro")
    );
    // Ordinary content chapters are not markers.
    assert_eq!(classify_chapter("Chapter 1"), None);
    assert_eq!(classify_chapter("The Heist"), None);
}

/// A trailing preview is its own kind, and used to be the credits.
///
/// Both regions sit in the tail, so the old single keyword list collapsed
/// them and put a `Skip Credits` button over new footage. They want
/// opposite treatment: credits are identical every week and skipping them
/// means the viewer is done, while a preview is different each time and a
/// viewer skipping it is going on to the next episode.
#[test]
fn a_trailing_preview_is_not_the_credits() {
    for title in [
        "Next Episode Preview",
        "Preview",
        "Previews",
        "Next Time On",
        "On the Next",
        "Next Week On",
        "next episode",
    ] {
        assert_eq!(
            classify_chapter(title).map(|m| m.0),
            Some("preview"),
            "{title}"
        );
    }
    // Titles that must NOT become a preview button. "The Next One" is the
    // trap a bare `next on` substring would spring — it contains those
    // characters and is story. "Coming Up" is unscripted television's
    // act-break tease before an ad break, mid-programme by definition.
    // "Previously On" is a recap and belongs to intro, which is checked
    // first; assert that ordering rather than trusting it.
    //
    // "Until Next Time" and "Till Next Time" are the expensive ones. They
    // are ordinary tail story titles carrying the identical words as a
    // sign-off, they sit past 70% of the runtime where the position bound
    // imposes nothing, and matching them deleted the file's real Skip
    // Credits marker and offered a button that finished the episode early.
    // "Next Weekend" is why the leading match needs a word boundary.
    for title in [
        "The Next One",
        "Coming Up",
        "Trailers",
        "Until Next Time",
        "Till Next Time",
        "Next Weekend",
        "Epilogue",
    ] {
        assert_eq!(classify_chapter(title).map(|m| m.0), None, "{title}");
    }
    // The teaser spellings that announce themselves in the lead, including
    // the bare "Next On" that a substring list had to withdraw for
    // matching "The Next One".
    for title in ["Next Time", "Next On", "Next On: Better Call Saul"] {
        assert_eq!(
            classify_chapter(title).map(|m| m.0),
            Some("preview"),
            "{title}"
        );
    }
    assert_eq!(
        classify_chapter("Previously On").map(|m| m.0),
        Some("intro"),
        "a recap is not a preview"
    );
    assert_eq!(
        classify_chapter("Next Episode Preview").map(|m| m.1),
        Some("Skip Preview"),
        "the button says what the region is"
    );

    // The credits stay the credits, including the anime spellings that sit
    // right next to a preview chapter on the same disc.
    for title in ["End Credits", "Ending", "ED", "Outro", "Closing"] {
        assert_eq!(
            classify_chapter(title).map(|m| m.0),
            Some("credits"),
            "{title}"
        );
    }

    // A title naming both starts in the credits, so that is the honest
    // button — and pressing it skips the whole span either way.
    assert_eq!(
        classify_chapter("Credits & Next Episode").map(|m| m.0),
        Some("credits")
    );

    // A disc's bonus reel is not a region inside the episode. Offering to
    // seek into it is worse than offering nothing.
    assert_eq!(classify_chapter("Trailers"), None);
}

#[test]
fn stored_chapter_bound_ignores_unrelated_probe_metadata() {
    let raw = serde_json::json!({
        "format": {"tags": {"padding": "x".repeat(MAX_CHAPTER_PROBE_BYTES + 1)}},
        "streams": [],
        "chapters": [chapter("Intro", "0", "60")],
    })
    .to_string();
    assert!(raw.len() > MAX_CHAPTER_PROBE_BYTES);
    let chapters = bounded_stored_chapters(&raw)
        .expect("only the extracted chapter payload is bounded")
        .expect("stored chapter array");
    assert_eq!(chapters.len(), 1);

    let oversized = serde_json::json!({
        "chapters": [{"tags": {"title": "x".repeat(MAX_CHAPTER_PROBE_BYTES + 1)}}],
    })
    .to_string();
    assert_eq!(
        bounded_stored_chapters(&oversized),
        Err(ChapterProbeFailure::Failed)
    );
}

fn chapter(title: &str, start: &str, end: &str) -> serde_json::Value {
    serde_json::json!({ "start_time": start, "end_time": end, "tags": { "title": title } })
}

/// The preview and the credits are separate markers on the same file, and
/// neither swallows the other.
///
/// The layout an anime or TV rip actually has: story, ED, next-episode
/// teaser. Before the split both tail chapters produced `credits`, so the
/// viewer got two identical buttons and the second one lied about what it
/// was skipping.
#[test]
fn the_credits_and_the_trailing_preview_are_separate_markers() {
    let chapters = vec![
        chapter("Opening", "0.000", "90.000"),
        chapter("Episode", "90.000", "1290.000"),
        chapter("Ending", "1290.000", "1380.000"),
        chapter("Next Episode Preview", "1380.000", "1410.000"),
    ];
    let markers = markers_from_chapters(&chapters, Some(1_410_000));
    let kinds: Vec<&str> = markers.iter().map(|m| m.kind.as_str()).collect();
    assert_eq!(kinds, vec!["intro", "credits", "preview"]);

    let credits = markers
        .iter()
        .find(|m| m.kind == "credits")
        .expect("credits");
    let preview = markers
        .iter()
        .find(|m| m.kind == "preview")
        .expect("preview");
    assert_eq!(credits.start_ms, 1_290_000);
    assert_eq!(preview.start_ms, 1_380_000);
    assert!(
        credits.end_ms <= preview.start_ms,
        "the credits must not run into the preview"
    );
    assert_eq!(preview.label, "Skip Preview");
    assert!(
        preview.chapter,
        "the file said so; this is not an inference"
    );
}

/// A file whose only tail chapter is the preview does not get an invented
/// credits marker laid over it.
///
/// This is the regression the split creates, and it is worse than the bug
/// it fixes. That preview chapter used to satisfy "some chapter said
/// credits" and stop the inference. Now it does not — so the guess would
/// run, take the preview's own boundary as the exact start of the credits,
/// and stretch to the end of the file: the same mislabelling, rebuilt by a
/// different path. The inferred credits are bounded by where the preview
/// begins instead.
#[test]
fn an_inferred_credits_marker_stops_where_a_labelled_preview_begins() {
    // A 24-minute episode: the plausible credits tail is 36 s, and the
    // boundary before the preview sits 40 s ahead of it — inside the
    // window, so the exact boundary is used rather than a proportion.
    let chapters = vec![
        chapter("Cold Open", "0.000", "60.000"),
        chapter("Act One", "60.000", "600.000"),
        chapter("Act Two", "600.000", "1340.000"),
        // The unlabelled tail chapter. Its boundary is the only positional
        // evidence for where the credits start, and taking it is what
        // makes the estimate exact.
        chapter("Tag", "1340.000", "1380.000"),
        chapter("Next Episode Preview", "1380.000", "1440.000"),
    ];
    let markers = markers_from_chapters(&chapters, Some(1_440_000));
    let preview = markers
        .iter()
        .find(|m| m.kind == "preview")
        .expect("preview");
    assert_eq!(preview.start_ms, 1_380_000);

    let credits = markers
        .iter()
        .find(|m| m.kind == "credits")
        .expect("the tail before the preview is still inferred as credits");
    assert!(
        !credits.chapter,
        "no chapter said credits, so it stays a guess"
    );
    assert_eq!(
        credits.end_ms, 1_380_000,
        "the inference ends where the file's own label begins, not at the \
             end of the file — which is the whole point"
    );
    assert_eq!(
        credits.start_ms, 1_340_000,
        "and starts at the last real boundary before the preview, not at \
             the preview's own"
    );
}

/// With no credits chapter at all, the guess still runs — bounded.
///
/// Inferring credits inside unlabelled story is the fallback's whole job
/// and predates this change; a file with no chapters gets exactly that. The
/// invariant the preview adds is narrower and it is the one asserted here:
/// the guess never runs past where the file said the preview begins, so a
/// viewer pressing an estimated "Skip Credits" can never be seeking into
/// footage they have not seen.
#[test]
fn a_preview_with_no_credits_boundary_invents_no_credits_marker() {
    // One story chapter and a preview. There is no boundary inside a
    // plausible credits tail before the preview, so there is no evidence
    // for where credits would start — and the proportional estimate would
    // put a "Skip Credits" button over the last 35 s of *story*, seeking a
    // viewer out of the episode.
    //
    // The earlier guard for this could never fire: it compared the
    // computed start against `credits_end`, and a preview clears 70% of
    // the runtime while the tail is at most a fortieth of it. Every such
    // file got the invented marker. Assert the absence, not the overlap.
    let chapters = vec![
        chapter("Episode", "0.000", "1200.000"),
        chapter("Preview", "1200.000", "1410.000"),
    ];
    let markers = markers_from_chapters(&chapters, Some(1_410_000));
    assert!(markers.iter().any(|m| m.kind == "preview"));
    assert!(
        !markers.iter().any(|m| m.kind == "credits"),
        "invented credits over story: {:?}",
        markers
            .iter()
            .filter(|m| m.kind == "credits")
            .map(|m| (m.start_ms, m.end_ms))
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_preview_with_a_real_boundary_infers_credits_up_to_it() {
    // The other half of the rule. Here the muxer did leave a boundary
    // inside a plausible tail — the story chapter ends 40 s before the
    // preview — so the credits are inferred exactly, from evidence, and
    // stop where the preview starts rather than running to the file end.
    let chapters = vec![
        chapter("Episode", "0.000", "1160.000"),
        chapter("End", "1160.000", "1200.000"),
        chapter("Preview", "1200.000", "1410.000"),
    ];
    let markers = markers_from_chapters(&chapters, Some(1_410_000));
    let preview = markers
        .iter()
        .find(|m| m.kind == "preview")
        .expect("preview");
    let credits = markers
        .iter()
        .find(|m| m.kind == "credits")
        .expect("inferred credits");
    assert!(!credits.chapter);
    assert_eq!(credits.start_ms, 1_160_000);
    assert_eq!(credits.end_ms, preview.start_ms);
    assert!(credits.start_ms < credits.end_ms);
    // Ticks and milliseconds describe the same span. Clients seek on the
    // ticks and label on the milliseconds, so a marker whose halves
    // disagree seeks somewhere other than where its button says.
    let scale = i64::from(credits.timescale);
    assert!(scale > 0);
    assert_eq!(credits.start_ticks * 1_000 / scale, credits.start_ms);
    assert_eq!(credits.end_ticks * 1_000 / scale, credits.end_ms);
}

/// A trailing chapter that overruns the runtime by a hair keeps its button.
///
/// MKV chapter atoms and `format.duration` are computed from different
/// places and disagree by a millisecond or two routinely. Dropping the
/// marker for it cost the viewer the button entirely — no Skip Preview, no
/// Skip Credits — and, on a *credits* chapter, silently demoted an authored
/// marker to the proportional estimate, so the button gained an "Estimated"
/// badge and lost auto-skip eligibility. The end is clamped to the runtime
/// instead.
#[test]
fn a_trailing_chapter_overrunning_the_runtime_by_a_hair_is_clamped() {
    let chapters = vec![
        chapter("Episode", "0.000", "1200.000"),
        // Two seconds past the declared runtime.
        chapter("Next Episode Preview", "1200.000", "1412.000"),
    ];
    let markers = markers_from_chapters(&chapters, Some(1_410_000));
    let preview = markers
        .iter()
        .find(|m| m.kind == "preview")
        .expect("the teaser still gets its button");
    assert!(preview.chapter, "and stays authored, not an estimate");
    assert_eq!(preview.end_ms, 1_410_000, "clamped to the runtime");
    assert!(preview.end_ticks > preview.start_ticks);
    assert!(
        !markers.iter().any(|m| m.kind == "credits"),
        "and no estimate is invented over it: {:?}",
        markers
            .iter()
            .filter(|m| m.kind == "credits")
            .map(|m| (m.start_ms, m.end_ms))
            .collect::<Vec<_>>()
    );
}

/// The same overrun on a credits chapter keeps it authored.
///
/// This is the half that mattered most: a 1 ms overrun used to drop the
/// real credits chapter, and the inference below then invented a
/// `chapter:false` marker in its place — a worse marker wearing an
/// "Estimated" badge, on a file that had told us exactly where the credits
/// were.
#[test]
fn an_overrunning_credits_chapter_is_not_demoted_to_an_estimate() {
    let chapters = vec![
        chapter("Episode", "0.000", "1350.000"),
        chapter("End Credits", "1350.000", "1410.001"),
    ];
    let markers = markers_from_chapters(&chapters, Some(1_410_000));
    let credits = markers
        .iter()
        .find(|m| m.kind == "credits")
        .expect("credits");
    assert!(credits.chapter, "authored, not inferred");
    assert_eq!((credits.start_ms, credits.end_ms), (1_350_000, 1_410_000));
}

/// An overrun too large to be rounding is still refused.
///
/// The clamp exists for a metadata artefact of a few milliseconds, not as a
/// blanket amnesty: a chapter claiming to end a minute past the runtime is
/// describing a different file, and clamping it would invent a span the
/// container never asserted.
#[test]
fn an_overrun_too_large_to_be_rounding_is_still_dropped() {
    let chapters = vec![
        chapter("Episode", "0.000", "1200.000"),
        chapter("Next Episode Preview", "1200.000", "1470.000"),
    ];
    let markers = markers_from_chapters(&chapters, Some(1_410_000));
    assert!(
        !markers.iter().any(|m| m.kind == "preview"),
        "60s past the runtime is broken metadata, not rounding"
    );
}

/// A keyword match in the tail still has to be the last thing there is.
///
/// `CREDITS_MIN_START_PCT` rejects a preview match below 70% of the runtime
/// and imposes nothing above it, so the last act is where a false positive
/// is unbounded — and it is not a spare button there. It bounds the
/// inferred credits, and with no chapter boundary before it in the window
/// there is nothing to infer from, so the file's real Skip Credits marker
/// disappears; on the web the false button also reports the episode
/// watched. Being the last thing in the file is the structural evidence
/// that separates a teaser from a scene title.
#[test]
fn a_tail_keyword_match_followed_by_more_story_is_not_a_preview() {
    let chapters = vec![
        chapter("Act Three", "1800.000", "2400.000"),
        // 89% in — well past the credits bound — and then the film keeps
        // going for another five minutes.
        chapter("Next Time", "2400.000", "2460.000"),
        chapter("Finale", "2460.000", "2700.000"),
    ];
    let markers = markers_from_chapters(&chapters, Some(2_700_000));
    assert!(
        !markers.iter().any(|m| m.kind == "preview"),
        "a teaser is the last thing in the file, and this is not"
    );
    let credits = markers
        .iter()
        .find(|m| m.kind == "credits")
        .expect("so the real credits marker survives");
    assert_eq!(credits.end_ms, 2_700_000);
}

/// A "preview" matched in the middle of a film cannot move the window.
///
/// `preview_kw` is substrings, so an ordinary scene title can match one
/// however carefully the list is drawn. When it does, the cost has to stay
/// a missing *preview* marker: a mid-film match that dragged `credits_end`
/// back with it would trade the real Skip Credits button at the end of the
/// film for nothing at all, which is strictly worse than the mislabelling
/// this change fixes. `CREDITS_MIN_START_PCT` is what prevents it, and it
/// is applied at the lookup as well as at the emit.
#[test]
fn a_mid_film_match_cannot_move_the_credits_window() {
    let chapters = vec![
        chapter("Opening", "0.000", "600.000"),
        // 41% in, and nowhere near the end: whatever this is, it is story.
        chapter("Next Time It Rains", "3000.000", "3200.000"),
        chapter("Finale", "6900.000", "7200.000"),
    ];
    let markers = markers_from_chapters(&chapters, Some(7_200_000));
    assert!(
        !markers.iter().any(|m| m.kind == "preview"),
        "a mid-film match is outside the credits window and is not offered"
    );
    let credits = markers
        .iter()
        .find(|m| m.kind == "credits")
        .expect("the credits are still inferred at the end of the film");
    assert_eq!(
        credits.end_ms, 7_200_000,
        "the guess still runs to the end of the file"
    );
    assert_eq!(
        credits.start_ms, 6_900_000,
        "and anchors on the last real boundary, which sits inside the \
             plausible tail"
    );
}

/// A teaser split across two chapter marks is one teaser.
///
/// Some rips mark the next-episode preview and its title card separately.
/// The credits have to stop at the first of the pair; stopping at the
/// second calls the first half of the teaser "credits", which is the same
/// mislabelling in miniature.
#[test]
fn a_teaser_split_across_two_chapters_is_bounded_at_its_first() {
    let chapters = vec![
        chapter("Episode", "0.000", "1170.000"),
        chapter("End", "1170.000", "1200.000"),
        chapter("Next Episode", "1200.000", "1300.000"),
        chapter("On The Next", "1300.000", "1410.000"),
    ];
    let markers = markers_from_chapters(&chapters, Some(1_410_000));
    let credits = markers
        .iter()
        .find(|m| m.kind == "credits")
        .expect("inferred credits");
    assert_eq!(
        credits.end_ms, 1_200_000,
        "the guess stops at the first of the two teaser chapters"
    );
    assert_eq!(credits.start_ms, 1_170_000);
}

/// A boundary closer than 15s to the end is a stinger, not the credits.
///
/// Post-credits scenes are chaptered on plenty of discs. Anchoring the
/// guess to one puts a "Skip Credits" button over ten seconds of the very
/// thing the viewer stayed for, so the boundary window has a floor as well
/// as a ceiling and the proportional estimate is used instead.
#[test]
fn a_post_credits_stinger_does_not_anchor_the_guess() {
    let chapters = vec![
        chapter("Opening", "0.000", "600.000"),
        chapter("Finale", "600.000", "7190.000"),
        chapter("Stinger", "7190.000", "7200.000"),
    ];
    let markers = markers_from_chapters(&chapters, Some(7_200_000));
    let credits = markers
        .iter()
        .find(|m| m.kind == "credits")
        .expect("credits");
    assert_eq!(
        credits.start_ms, 7_020_000,
        "the proportional estimate, not the stinger's boundary at 7190s"
    );
    assert!(!credits.chapter, "still an inference");
}

#[test]
fn chapters_overrunning_the_duration_still_fall_through_to_the_estimate() {
    // With no preview the boundary search is the pre-existing one: only
    // the final span is a candidate. Walking back would newly accept an
    // earlier boundary as exact on containers whose chapters overrun their
    // declared duration, which is common with mismatched metadata — the
    // estimate is the honest answer there, not a boundary chosen because
    // it happened to fit the window.
    // The final span starts past the declared duration, so it is not a
    // candidate. A walk-back would land on the 1370 s boundary — 30 s of
    // window, comfortably inside the filter — and call it exact. The
    // estimate is the honest answer, and asserting its exact value is what
    // distinguishes the two.
    let chapters = vec![
        chapter("Story", "0.000", "1370.000"),
        chapter("Tail", "1370.000", "1450.000"),
        chapter("Extra", "1450.000", "1500.000"),
    ];
    let markers = markers_from_chapters(&chapters, Some(1_400_000));
    let credits = markers
        .iter()
        .find(|m| m.kind == "credits")
        .expect("estimated credits");
    assert!(!credits.chapter, "the final span starts past the duration");
    assert_eq!(credits.end_ms, 1_400_000);
    // duration/40 = 35 s of tail, not the 1_370_000 boundary a walk-back
    // would have taken.
    assert_eq!(credits.start_ms, 1_400_000 - 35_000);
}

/// Markers are built from a chapters array, whatever produced it — which is
/// what lets the play path read them out of the scan probe instead of
/// running its own ffprobe against the NAS on every click.
#[test]
fn markers_come_from_a_chapters_array() {
    let chapters = vec![
        chapter("Opening", "0.000", "85.000"),
        chapter("The Heist", "85.000", "3000.000"),
        chapter("End Credits", "3000.000", "3180.000"),
    ];
    let m = markers_from_chapters(&chapters, Some(3_180_000));
    assert_eq!(m.len(), 2, "only intro and credits are markers");
    assert_eq!(m[0].kind, "intro");
    assert_eq!((m[0].start_ms, m[0].end_ms), (0, 85_000));
    assert!(m[0].chapter, "came from a real chapter");
    assert_eq!(m[1].kind, "credits");
    assert_eq!(m[1].start_ms, 3_000_000);

    // A chapterless file still offers Skip Credits, flagged as a guess —
    // and the guess is proportionate to the runtime instead of the flat
    // 2m30s tail that every file at or above 30 minutes used to get. A
    // 55-minute episode whose credits run 20–60s cannot afford to have the
    // button open a minute and a half into the last act.
    let hour = 55 * 60_000;
    let m = markers_from_chapters(&[], Some(hour));
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].kind, "credits");
    assert!(!m[0].chapter, "the duration heuristic is not a chapter");
    assert_eq!(
        m[0].end_ms, hour,
        "the estimate runs to the end of the file"
    );
    assert_eq!(
        hour - m[0].start_ms,
        82_500,
        "2.5% of a 55-minute runtime, not a flat 150s"
    );

    // Proportionate means it actually moves with the runtime: a 22-minute
    // episode and a 3-hour film do not get the same window.
    let short = markers_from_chapters(&[], Some(22 * 60_000));
    let long = markers_from_chapters(&[], Some(180 * 60_000));
    assert_eq!(22 * 60_000 - short[0].start_ms, 33_000);
    assert_eq!(
        180 * 60_000 - long[0].start_ms,
        180_000,
        "the ceiling only binds at feature length and above"
    );

    // Nothing to guess from, and nothing invented.
    assert!(markers_from_chapters(&[], None).is_empty());
    assert!(markers_from_chapters(&[], Some(60_000)).is_empty());

    // Malformed entries are skipped rather than trusted: a zero-length or
    // backwards chapter would render as an un-dismissable skip button.
    let junk = vec![
        chapter("Intro", "10.0", "10.0"),
        chapter("Intro", "90.0", "10.0"),
        serde_json::json!({ "tags": { "title": "Intro" } }),
    ];
    assert!(markers_from_chapters(&junk, None).is_empty());
}

#[test]
fn marker_wire_keeps_its_five_fields_and_adds_optional_annotation_evidence() {
    let derived = markers_from_chapters(&[chapter("Opening", "0", "90")], Some(600_000));
    let legacy = serde_json::to_value(&derived[0]).expect("serialize derived marker");
    let fields = legacy
        .as_object()
        .expect("marker object")
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        fields,
        ["chapter", "end_ms", "kind", "label", "start_ms"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );

    let set = annotation_set_from_markers(
        plurx_core::segplan::SourceIdentity::new(10, 20, "timeline-v1"),
        &derived,
    );
    let persisted = serde_json::to_value(&markers_from_annotation_set(set)[0])
        .expect("serialize persisted marker");
    for field in ["kind", "label", "start_ms", "end_ms", "chapter"] {
        assert_eq!(
            persisted[field], legacy[field],
            "compatibility field {field}"
        );
    }
    assert_eq!(persisted["provenance"], "authored");
    assert_eq!(persisted["confidence"], 1_000);
    assert!(persisted["generation"].is_string());
    assert_eq!(persisted["detector_version"], CHAPTER_ANNOTATION_VERSION);

    let mut manual_set = annotation_set_from_markers(
        plurx_core::segplan::SourceIdentity::new(10, 20, "timeline-v1"),
        &derived,
    );
    manual_set.annotations[0].provenance = plurx_core::segplan::AnnotationProvenance::Manual;
    manual_set.annotations[0].manual_override_revision = Some(1);
    let manual = serde_json::to_value(&markers_from_annotation_set(manual_set)[0])
        .expect("serialize manual marker");
    assert_eq!(manual["provenance"], "manual");
    assert_eq!(
        manual["chapter"], true,
        "legacy clients see manual as exact"
    );
}

#[test]
fn authored_chapter_persistence_keeps_ffprobe_ticks_authoritative() {
    let chapters = [serde_json::json!({
        "start": "1",
        "end": "2",
        "time_base": "1/24",
        "start_time": "0.041667",
        "end_time": "0.083333",
        "tags": { "title": "Opening" }
    })];
    let markers = markers_from_chapters(&chapters, Some(1_000));
    assert_eq!(markers.len(), 1);
    assert_eq!((markers[0].start_ms, markers[0].end_ms), (41, 83));

    let set = annotation_set_from_markers(
        plurx_core::segplan::SourceIdentity::new(10, 20, "timeline-v1"),
        &markers,
    );
    let annotation = &set.annotations[0];
    assert_eq!(
        (
            annotation.start_ticks,
            annotation.end_ticks,
            annotation.timescale,
        ),
        (1, 2, 24)
    );
    assert_eq!((annotation.start_ms, annotation.end_ms), (41, 83));

    let inferred = markers_from_chapters(
        &[
            chapters[0].clone(),
            serde_json::json!({
                "start": "13800",
                "end": "14400",
                "time_base": "1/24",
                "start_time": "575.0",
                "end_time": "600.0",
                "tags": { "title": "Finale" }
            }),
        ],
        Some(600_001),
    );
    let inferred_set = annotation_set_from_markers(
        plurx_core::segplan::SourceIdentity::new(10, 20, "timeline-v1"),
        &inferred,
    )
    .validate_and_normalize(600_001)
    .expect("non-frame-aligned duration remains tick-consistent");
    let credits = inferred_set
        .annotations
        .iter()
        .find(|annotation| annotation.kind == plurx_core::segplan::AnnotationKind::Credits)
        .expect("inferred credits");
    assert_eq!(credits.timescale, 3_000);
    assert_eq!(credits.end_ms, 600_001);
}

#[test]
fn malformed_or_extreme_chapter_times_are_rejected_without_arithmetic_wrap() {
    let chapters = [
        chapter("Opening", "-9000000000000000", "1"),
        chapter("Opening", "9000000000000000", "9100000000000000"),
        serde_json::json!({
            "start": i64::MAX.to_string(),
            "end": i64::MAX.to_string(),
            "time_base": "2/1",
            "tags": { "title": "Opening" }
        }),
    ];
    assert!(markers_from_chapters(&chapters, Some(60_000)).is_empty());
    let maximum_duration = markers_from_chapters(&[], Some(i64::MAX));
    assert_eq!(maximum_duration.len(), 1);
    assert!(maximum_duration[0].start_ms < maximum_duration[0].end_ms);
}

/// A chaptered file whose chapter titles never say "credits" still carries
/// positional evidence, and the final boundary is better evidence than a
/// proportion of the runtime. It is still an inference — no title confirmed
/// it — so it stays `chapter:false` and the UI keeps hedging; what changes
/// is that the start is exact.
#[test]
fn a_chaptered_file_without_a_credits_title_uses_its_last_boundary() {
    let hour = 3_300_000; // 55 minutes
    let m = markers_from_chapters(
        &[
            chapter("Opening", "0.000", "90.000"),
            chapter("Act One", "90.000", "1600.000"),
            chapter("Act Two", "1600.000", "3200.000"),
            chapter("Chapter 5", "3200.000", "3300.000"),
        ],
        Some(hour),
    );
    assert_eq!(m.len(), 2, "the intro chapter and one credits inference");
    assert_eq!(m[0].kind, "intro");
    assert!(m[0].chapter);
    assert_eq!(m[1].kind, "credits");
    assert_eq!(
        m[1].start_ms, 3_200_000,
        "the real final boundary, not dur minus a guessed tail"
    );
    assert_eq!(m[1].end_ms, hour);
    assert!(
        !m[1].chapter,
        "no title said credits, so the UI still hedges"
    );

    // A final boundary that is nowhere near the tail is a story chapter,
    // not the credits. Fall back to the proportional estimate rather than
    // opening the button 28 minutes early.
    let m = markers_from_chapters(
        &[
            chapter("Opening", "0.000", "90.000"),
            chapter("Act One", "90.000", "1600.000"),
            chapter("Finale", "1600.000", "3300.000"),
        ],
        Some(hour),
    );
    assert_eq!(m.len(), 2);
    assert_eq!(m[1].kind, "credits");
    assert_eq!(m[1].start_ms, hour - 82_500, "the deliberate fallback");
    assert!(!m[1].chapter);
}

/// `credit_kw` and `intro_kw` match broad substrings anywhere in a title, so
/// classification alone will happily call a mid-episode chapter the credits.
/// Clients take the *first* marker containing the playhead, so an unbounded
/// match does not sit harmlessly beside the real one — it becomes the
/// button. Position vetoes the title.
#[test]
fn a_marker_in_the_wrong_half_of_the_runtime_is_rejected() {
    let hour = 3_300_000; // 55 minutes

    // "Closing Time" and "The Ending" are ordinary scene titles that trip
    // the credits substrings halfway through the episode.
    let m = markers_from_chapters(
        &[
            chapter("Opening", "0.000", "90.000"),
            chapter("Closing Time", "1650.000", "1800.000"),
            chapter("The Ending", "1800.000", "3300.000"),
        ],
        Some(hour),
    );
    assert!(
        !m.iter()
            .any(|k| k.kind == "credits" && k.start_ms * 100 < hour * 70),
        "no Skip Credits button before the last 30% of the runtime: {m:?}"
    );
    assert_eq!(m.len(), 2, "the intro, and one credits inference: {m:?}");
    assert_eq!(m[0].kind, "intro");
    assert_eq!(m[1].kind, "credits");
    assert_eq!(
        m[1].start_ms,
        hour - 82_500,
        "the rejected titles do not suppress the fallback either"
    );

    // The mirror case: a "Recap" chapter at the very end is not an intro.
    let m = markers_from_chapters(
        &[
            chapter("Opening", "0.000", "90.000"),
            chapter("Act One", "90.000", "3200.000"),
            chapter("Recap", "3200.000", "3300.000"),
        ],
        Some(hour),
    );
    assert!(
        !m.iter().any(|k| k.kind == "intro" && k.start_ms * 2 > hour),
        "no Skip Intro button past the halfway mark: {m:?}"
    );
    assert_eq!(m.len(), 2);
    assert_eq!((m[0].kind.as_str(), m[0].start_ms), ("intro", 0));
    assert_eq!((m[1].kind.as_str(), m[1].start_ms), ("credits", 3_200_000));
}

/// The backfill writes chapters into the stored probe without disturbing
/// what else is in it — and refuses to invent a document for a file whose
/// probe never succeeded, because `probe_json IS NULL` is the fingerprint
/// the repair job keys on.
#[tokio::test]
async fn sharing_source_decision_reads_stored_markers_without_live_probe_or_backfill() {
    use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
    let (_, state) = super::super::tests::test_app_with_state();
    let directory = tempfile::tempdir().expect("Source marker fixture");
    let path = directory.path().join("legacy-source.mp4");
    std::fs::write(&path, b"Source marker presence fixture").expect("actual presence");
    let library = state
        .store
        .create_library(&NewLibrary {
            name: "Source markers".into(),
            kind: LibraryKind::Movies,
            paths: vec![],
            anime: false,
        })
        .await
        .expect("library");
    let item = state
        .store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Source".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let raw = r#"{"format":{"duration":"600.0"},"streams":[]}"#;
    let id = state
        .store
        .upsert_file(
            item,
            path.to_str().expect("path"),
            30,
            1000,
            &ProbeResult {
                duration_ms: Some(600_000),
                container: Some("mp4".into()),
                video_codec: Some("h264".into()),
                width: Some(1920),
                height: Some(1080),
                bit_depth: Some(8),
                raw_json: Some(raw.into()),
                ..Default::default()
            },
        )
        .await
        .expect("scan without chapter capture");
    let file = state
        .store
        .get_file(id)
        .await
        .expect("file read")
        .expect("file");
    let pause = pause_next_marker_fallback_for_test(&path);
    let caps = serde_json::from_value(serde_json::json!({"v":2,"video":[{"codec":"h264","max_height":2160,"present":["sdr"]}],"audio":["aac"],"containers":["mp4"],"transports":["hls","progressive"]})).expect("actual v2 caps");
    let decision = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        decision_for_source_file(
            &state,
            file.clone(),
            Caps {
                caps_v2: Some(caps),
                ..Default::default()
            },
        ),
    )
    .await
    .expect("Source never enters the paused live-probe fallback")
    .expect("actual Source engine");
    assert_eq!(
        serde_json::to_value(&decision.markers).expect("markers"),
        serde_json::to_value(markers_from_chapters(&[], file.duration_ms))
            .expect("stored evidence only")
    );
    assert_eq!(
        state
            .store
            .get_file_probe_json(id)
            .await
            .expect("unchanged scan"),
        Some(raw.into())
    );
    assert!(
        state
            .store
            .timeline_annotation_set(id, &annotation_source_identity(&file))
            .await
            .expect("annotation read")
            .is_none(),
        "decision must not backfill an annotation set"
    );
    // The ordinary Local fallback still enters its existing owned seam.
    let local_state = state.clone();
    let local_file = file.clone();
    let local = tokio::spawn(async move { markers_for(&local_state, &local_file).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), pause.wait())
        .await
        .expect("Local fallback retained");
    local.abort();
    let _ = local.await;
    marker_fallback_pauses()
        .lock()
        .expect("pause registry")
        .remove(path.to_str().expect("path"));
    let chapters =
        serde_json::to_string(&vec![chapter("Intro", "0.0", "60.0")]).expect("scan chapters");
    state
        .store
        .merge_file_probe_chapters(id, &chapters)
        .await
        .expect("scanner evidence fixture");
    assert_eq!(
        serde_json::to_value(stored_markers_for_source(&state, &file).await)
            .expect("stored markers"),
        serde_json::to_value(markers_from_chapters(
            &[chapter("Intro", "0.0", "60.0")],
            file.duration_ms
        ))
        .expect("same common marker engine")
    );
    let manual = annotation_set_from_markers(
        annotation_source_identity(&file),
        &markers_from_chapters(&[chapter("Opening", "0", "90")], file.duration_ms),
    );
    state
        .store
        .put_timeline_annotation_set_if_missing(id, 600_000, &manual)
        .await
        .expect("persisted annotation fixture");
    assert_eq!(
        serde_json::to_value(stored_markers_for_source(&state, &file).await)
            .expect("persisted markers"),
        serde_json::to_value(markers_from_annotation_set(manual)).expect("persisted evidence wins")
    );
}

#[tokio::test]
async fn chapters_backfill_into_the_stored_probe() {
    use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
    use plurx_core::store::{SqliteStore, Store};
    let store: std::sync::Arc<dyn Store> =
        std::sync::Arc::new(SqliteStore::open_in_memory().expect("store"));
    let lib = store
        .create_library(&NewLibrary {
            name: "L".into(),
            kind: LibraryKind::Movies,
            paths: vec![],
            anime: false,
        })
        .await
        .expect("lib");
    let item = store
        .insert_item(&NewItem {
            library_id: lib.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Heat".into(),
            year: Some(1995),
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    // A pre-chapters probe: real JSON, no `chapters` key.
    let legacy = store
        .upsert_file(
            item,
            "/media/legacy.mkv",
            1,
            1,
            &ProbeResult {
                duration_ms: Some(600_000),
                raw_json: Some(r#"{"format":{"duration":"600.0"},"streams":[]}"#.into()),
                ..Default::default()
            },
        )
        .await
        .expect("legacy file");
    // A file whose probe failed outright: probe_json IS NULL.
    let unprobed = store
        .upsert_file(item, "/media/broken.mkv", 1, 1, &ProbeResult::default())
        .await
        .expect("unprobed file");

    let chapters = serde_json::to_string(&vec![chapter("Intro", "0.0", "60.0")]).expect("json");
    store
        .merge_file_probe_chapters(legacy, &chapters)
        .await
        .expect("merge");
    store
        .merge_file_probe_chapters(unprobed, &chapters)
        .await
        .expect("merge on a null probe is a no-op, not an error");

    let raw = store
        .get_file_probe_json(legacy)
        .await
        .expect("read")
        .expect("has probe json");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
    let stored = v
        .get("chapters")
        .and_then(|c| c.as_array())
        .expect("grafted");
    assert_eq!(stored.len(), 1);
    assert_eq!(markers_from_chapters(stored, None)[0].kind, "intro");
    // The rest of the document survived the graft.
    assert!(v.get("format").is_some(), "format survived");
    assert!(v.get("streams").is_some(), "streams survived");

    // The failed probe is still recognisably a failed probe.
    assert!(
        store
            .get_file_probe_json(unprobed)
            .await
            .expect("read")
            .is_none(),
        "a null probe must stay null — the repair job keys on it"
    );
}

#[test]
fn native_is_not_the_same_question_as_text() {
    fn sub(index: i64, codec: &str) -> plurx_core::domain::SubtitleStream {
        plurx_core::domain::SubtitleStream {
            index,
            codec: codec.into(),
            ..Default::default()
        }
    }
    let mut file = MediaFile {
        downloaded_subtitles: Vec::new(),
        id: 1,
        item_id: 1,
        path: "/media/anime.mkv".into(),
        size: 1,
        mtime: 1,
        duration_ms: Some(1_000),
        container: Some("mkv".into()),
        video_codec: Some("hevc".into()),
        video_codec_tag: None,
        field_order: None,
        video_profile: None,
        width: Some(1920),
        height: Some(1080),
        bit_depth: Some(8),
        hdr: None,
        hdr_format: None,
        max_cll: None,
        max_fall: None,
        mastering_max_luminance: None,
        luminance_source: None,
        bitrate: Some(1_000),
        audio_streams: vec![],
        subtitle_streams: vec![
            sub(0, "subrip"),
            sub(1, "ass"),
            sub(2, "ssa"),
            sub(3, "hdmv_pgs_subtitle"),
            sub(4, "webvtt"),
        ],
        scanned_at: 0,
        audio_offset_ms: 0,
        probed: true,
        dolby_vision: Default::default(),
    };
    let tracks = sub_tracks(&file, false);

    // SRT and WebVTT: text, and servable as a rendition.
    assert!(tracks[0].text && tracks[0].native);
    assert!(tracks[4].text && tracks[4].native);

    // The trap. ASS/SSA carry text, so `text` is true — but their authored
    // styling does not survive WebVTT, the master never advertises them,
    // and `POST …/hls/sessions` rejects one. A client routing on `text`
    // asks for a session the server refuses, and the natural recovery from
    // that refusal is a burn.
    assert!(tracks[1].text, "ASS carries text");
    assert!(!tracks[1].native, "ASS cannot be a native rendition");
    assert!(tracks[2].text && !tracks[2].native, "SSA likewise");

    // Bitmap is neither, and always was.
    assert!(!tracks[3].text && !tracks[3].native);
    assert!(subtitle_requires_burn_in(&file, Some(3), false));
    assert!(!subtitle_requires_burn_in(&file, Some(0), false));

    // The three answers the capability governs. A client that posted a
    // document and did not claim the protocol cannot paint one; a client
    // that posted no document at all has said nothing, and the legacy GET
    // path — which both native clients fall back to on any 400/404/405 —
    // is exactly that case. Reading silence as a refusal is what would
    // send a capable client off to re-encode a whole film.
    let capable = plurx_core::playback::DeviceCaps {
        subtitle_overlays: vec![crate::pgs_overlay::PROTOCOL.to_owned()],
        ..Default::default()
    };
    let silent = plurx_core::playback::DeviceCaps::default();
    assert!(
        !silent.renders_subtitle_overlay(crate::pgs_overlay::PROTOCOL),
        "absent is never a claim"
    );
    assert!(capable.renders_subtitle_overlay(crate::pgs_overlay::PROTOCOL));
    assert!(
        !capable.renders_subtitle_overlay("pgs-v2"),
        "claiming one protocol claims nothing about its successor"
    );

    assert!(overlay_for_caller(true, Some(&capable)));
    assert!(
        !overlay_for_caller(true, Some(&silent)),
        "a document that claims nothing is a client that cannot paint it"
    );
    assert!(
        overlay_for_caller(true, None),
        "no document at all is silence, not a refusal — the legacy GET \
             fallback must not turn an overlay into a whole-film burn"
    );
    assert!(
        !overlay_for_caller(false, Some(&capable)),
        "the client's claim never overrides an operator switch that is off"
    );

    // Default-off and old servers remain wire-compatible: the additive
    // field is absent rather than null.
    let disabled = serde_json::to_value(&tracks).expect("subtitle DTOs");
    assert!(disabled
        .as_array()
        .expect("array")
        .iter()
        .all(|track| track.get("overlay").is_none()));

    let enabled = sub_tracks(&file, true);
    assert_eq!(enabled[3].overlay, Some("pgs-v1"));
    assert!(
        !subtitle_requires_burn_in(&file, Some(3), true),
        "an enabled PGS overlay keeps the video bytes untouched"
    );
    assert!(enabled
        .iter()
        .enumerate()
        .all(|(index, track)| index == 3 || track.overlay.is_none()));

    // And the field agrees with the classifier the master and the 400 use,
    // rather than being a second opinion about the same codecs.
    for (track, source) in tracks.iter().zip(&file.subtitle_streams) {
        assert_eq!(
            track.native,
            plurx_core::tracks::is_native_text_subtitle(&source.codec)
        );
    }

    // Bad Boys for Life's exact shape: HDR source, but `hdr=0` already
    // made the base decision SDR before its forced PGS track was applied.
    // Burning that track is not an HDR downgrade and must remain valid.
    file.hdr = Some("hdr10".into());
    let mut already_sdr = planned(playback::PlaybackMethod::Transcode);
    already_sdr.delivered_dynamic_range = "sdr";
    assert!(!plurx_core::playback::burn_would_discard_hdr("sdr", true));
    apply_selected_subtitle(&mut already_sdr, &file, Some(3), true, "sdr");
    assert_eq!(already_sdr.method, playback::PlaybackMethod::Transcode);
    assert!(already_sdr
        .reasons
        .iter()
        .any(|reason| reason.contains("requires burn-in")));

    let mut still_hdr = planned(playback::PlaybackMethod::Remux);
    still_hdr.delivered_dynamic_range = "hdr10";
    assert!(plurx_core::playback::burn_would_discard_hdr("hdr10", true));
    apply_selected_subtitle(&mut still_hdr, &file, Some(3), true, "hdr10");
    assert_eq!(still_hdr.method, playback::PlaybackMethod::Remux);
    assert!(
        still_hdr.reasons.is_empty(),
        "a burn must not silently replace an HDR delivery with SDR"
    );

    // The row the old `/decision` predicate got wrong, and the reason the
    // `method != Transcode` term is gone: since the M4 HDR10 rung, an HDR
    // title that transcodes for a height cap is putting HDR10 on the
    // wire. Burning into it drops the grade, so it must refuse — the old
    // predicate exempted it purely for being a transcode.
    let mut negotiated_hdr10 = planned(playback::PlaybackMethod::Transcode);
    negotiated_hdr10.delivered_dynamic_range = "hdr10";
    assert!(plurx_core::playback::burn_would_discard_hdr("hdr10", true));
    apply_selected_subtitle(&mut negotiated_hdr10, &file, Some(3), true, "hdr10");
    assert_eq!(negotiated_hdr10.delivered_dynamic_range, "hdr10");
    assert!(
        negotiated_hdr10.reasons.is_empty(),
        "an HDR10 transcode is an HDR delivery; a burn into it is a downgrade"
    );
}

/// One classifier for all four routes, so a client never has to rebuild
/// it out of `text`/`native`/`overlay` — which is how the web client came
/// to hand ASS to a `<track>` while Android re-encoded the same file.
#[test]
fn the_subtitle_route_names_how_the_cues_are_produced() {
    let file = MediaFile {
        downloaded_subtitles: Vec::new(),
        id: 1,
        item_id: 1,
        path: "/media/routes.mkv".into(),
        size: 1,
        mtime: 1,
        duration_ms: Some(1_000),
        container: Some("mkv".into()),
        video_codec: Some("hevc".into()),
        video_codec_tag: None,
        field_order: None,
        video_profile: None,
        width: Some(1920),
        height: Some(1080),
        bit_depth: Some(8),
        hdr: None,
        hdr_format: None,
        max_cll: None,
        max_fall: None,
        mastering_max_luminance: None,
        luminance_source: None,
        bitrate: Some(1_000),
        audio_streams: vec![],
        subtitle_streams: [
            "subrip",
            "ass",
            "mov_text",
            "hdmv_pgs_subtitle",
            "dvd_subtitle",
        ]
        .iter()
        .enumerate()
        .map(|(index, codec)| plurx_core::domain::SubtitleStream {
            index: index as i64,
            codec: (*codec).into(),
            language: None,
            title: None,
            default: false,
            forced: false,
            hearing_impaired: false,
        })
        .collect(),
        scanned_at: 0,
        audio_offset_ms: 0,
        probed: true,
        dolby_vision: Default::default(),
    };

    for (index, off, on) in [
        (0, "native", "native"),
        (1, "sidecar", "sidecar"),
        (2, "sidecar", "sidecar"),
        // The only row the overlay moves, and it moves it off `burn`.
        (3, "burn", "overlay"),
        // VobSub is not PGS; the overlay contract does not cover it.
        (4, "burn", "burn"),
    ] {
        assert_eq!(subtitle_route(&file, Some(index), false), Some(off));
        assert_eq!(subtitle_route(&file, Some(index), true), Some(on));
    }

    assert_eq!(subtitle_route(&file, None, true), None, "nothing selected");
    assert_eq!(
        subtitle_route(&file, Some(99), true),
        None,
        "a track this file does not have is not a route"
    );

    // And it agrees with the two flags it exists to stop clients from
    // combining by hand.
    for index in 0..5 {
        let burns = subtitle_route(&file, Some(index), false) == Some("burn");
        assert_eq!(burns, subtitle_requires_burn_in(&file, Some(index), false));
    }
}

/// The whole guard, as one table. Four rows, one function, and the same
/// function the create path and the preparation path now call.
#[test]
fn one_burn_guard_judges_the_delivery_without_the_burn() {
    for (base, want, why) in [
        (
            "dolby_vision",
            true,
            "an HDR copy has no encode to burn into",
        ),
        ("hdr10", true, "including one a transcode negotiated"),
        ("hlg", true, "and HLG is HDR too"),
        ("sdr", false, "an already tone-mapped plan loses nothing"),
        ("", false, "an unknown grade is not a claim of HDR"),
    ] {
        assert_eq!(
            plurx_core::playback::burn_would_discard_hdr(base, true),
            want,
            "{base}: {why}"
        );
        assert!(
            !plurx_core::playback::burn_would_discard_hdr(base, false),
            "{base}: a track that needs no burn is never refused"
        );
    }
}
