#[test]
fn dovi_reshape_pairing_proof_uses_a_delivery_raster_every_encoder_accepts() {
    use super::*;
    for encoder in [
        Encoder::Software,
        Encoder::Vaapi,
        Encoder::Qsv,
        Encoder::Nvenc,
        Encoder::VideoToolbox,
    ] {
        let args = dovi_reshape_probe_args(encoder);
        let (width, height) = HARDWARE_ENCODE_PROOF_RASTER;
        // AMD VA-API's floor is 128x128; the proof must not test below
        // the sizes the reshape is actually delivered at.
        assert!(width >= 1280 && height >= 720, "{encoder:?}");
        assert!(args.contains(&format!("color=size={width}x{height}:rate=1:color=black")));
        let filter = &args[args.iter().position(|a| a == "-vf").expect("filter") + 1];
        let production = Pipeline::DoviTonemapx
            .filters(Some(i64::from(width)), i64::from(height), Some("dovi"))
            .expect("production reshape chain");
        assert!(filter.starts_with(&production), "{encoder:?}: {filter}");
        assert!(filter.contains(&format!("apply_dovi=1,scale={width}:{height},")));
        assert!(!args
            .iter()
            .any(|a| a.contains("64x64") || a.contains("scale=64:64")));
    }
}

#[test]
fn hardware_main10_proofs_use_a_raster_every_hardware_encoder_accepts() {
    use super::*;
    let (width, height) = HARDWARE_ENCODE_PROOF_RASTER;
    // Intel QSV's HEVC Main10 encoder fails a 64x64 frame ("Invalid
    // FrameType"); AMD VA-API refuses anything under 128x128.
    assert!(width >= 1280 && height >= 720);
    let args = hardware_main10_probe_args(Encoder::Qsv).expect("QSV Main10 proof");
    assert!(args.contains(&format!("color=size={width}x{height}:rate=1:color=black")));
    assert!(!args.iter().any(|a| a.contains("64x64")));
    // The proof is the production device init, upload and encoder.
    let init = Encoder::Qsv.init_args();
    assert!(args
        .windows(init.len())
        .any(|window| window == init.as_slice()));
    let filter = &args[args.iter().position(|a| a == "-vf").expect("filter") + 1];
    assert_eq!(
        Some(filter.as_str()),
        Encoder::Qsv.filter_suffix_for(OutputGrade::Hdr10)
    );
    let encode = Encoder::Qsv.encode_args_for(
        OutputGrade::Hdr10,
        20_000,
        EffectiveRateControl::Vbr,
        false,
        None,
    );
    assert!(args
        .windows(encode.len())
        .any(|window| window == encode.as_slice()));
    assert!(encode.iter().any(|a| a == "main10"));
}

/// The two QSV Main10 probes must keep taking their graph from the proof
/// builder: an inline frame size in either one is how the 64x64 proof
/// that refused Intel's 4K HDR10 rung came about.
#[test]
fn qsv_main10_probes_take_their_graph_from_the_proof_builder() {
    let source = include_str!("../ffmpeg.rs");
    // Spelled with concat! so this guard does not add a second copy of
    // the inventory's exact source anchors for these probes.
    for probe in [
        concat!("pub async fn ", "has_hdr10_passthrough_qsv("),
        concat!("pub async fn ", "has_dovi_passthrough_with("),
    ] {
        let start = source.find(probe).expect("probe is defined");
        let body = &source[start..];
        let end = body[probe.len()..]
            .find("\npub ")
            .map_or(body.len(), |offset| offset + probe.len());
        let body = &body[..end];
        assert!(body.contains("hardware_main10_probe_args("), "{probe}");
        assert!(!body.contains("color=size="), "{probe}");
    }
}

#[tokio::test]
async fn held_clock_reporter_bytes_enter_existing_engine_identity_and_publication_fence() {
    use super::*;
    let directory = tempfile::tempdir().expect("reporter directory");
    let path = directory.path().join("clock-reporter");
    std::fs::write(&path, b"held reporter first version").expect("reporter");
    let reporter = EncodedExecutable::capture_at(path.clone())
        .await
        .expect("capture");
    let mut engine = EncodedEngine {
        digest: "initial engine".into(),
        process_identity: encoded_process_identity().into(),
        objects: Arc::from([]),
        font_objects: Arc::from([]),
        font_env: None,
    };
    let before = engine.digest.clone();
    engine
        .bind_clock_reporter(&reporter)
        .await
        .expect("held reporter binding");
    assert_ne!(engine.digest, before);
    assert!(engine.is_current().await);
    std::fs::write(path, b"held reporter newer version").expect("same-size replacement");
    assert!(
        !engine.is_current().await,
        "old clock evidence cannot publish after its reporter changes"
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn held_clock_matched_package_rejects_swapped_module_foreign_reporter_and_demux_patch() {
    use super::*;
    let directory = tempfile::tempdir().expect("package");
    let root = directory.path();
    std::fs::create_dir_all(root.join("provenance/source-parser")).expect("provenance");
    std::fs::create_dir_all(root.join("provenance/local-patches")).expect("patch directory");
    for (name, bytes) in [
        ("ffmpeg", b"producer".as_slice()),
        ("ffprobe", b"native reporter".as_slice()),
        ("plurx-source-parser.wasm", b"sealed parser".as_slice()),
        ("foreign", b"foreign reporter".as_slice()),
    ] {
        std::fs::write(root.join(name), bytes).expect("image");
    }
    let producer = EncodedExecutable::capture_at(root.join("ffmpeg"))
        .await
        .expect("producer");
    let native = EncodedExecutable::capture_at(root.join("ffprobe"))
        .await
        .expect("native");
    let module = EncodedExecutable::capture_at(root.join("plurx-source-parser.wasm"))
        .await
        .expect("module");
    let foreign = EncodedExecutable::capture_at(root.join("foreign"))
        .await
        .expect("foreign");
    let quilt: serde_json::Map<String, serde_json::Value> = (0..100)
        .map(|i| {
            (
                format!("{i:04}-test.patch"),
                serde_json::json!("a".repeat(64)),
            )
        })
        .collect();
    let recipes: serde_json::Map<String, serde_json::Value> = quilt
        .iter()
        .map(|(name, digest)| (format!("debian/patches/{name}"), digest.clone()))
        .collect();
    let source = "87bedcefc860cd234cdeddbebdcd3ce45aeedc81d0fbb258ed839ff778be3140";
    let commit = "253db2a7b0a8045c54ce68ce33d7f601229b1822";
    let mut manifest = serde_json::json!({"source_sha256":source,"source_commit":commit,
            "source_recipe_sha256":recipes,"binaries":{"ffmpeg":{"sha256":producer.digest},"ffprobe":{"sha256":native.digest}},
            "source_parser":{"sha256":module.digest},"compiled_source":{"local_patches":{}}});
    let parser = serde_json::json!({"source_sha256":source,"source_commit":commit,
            "source_patch_sha256":quilt,"module_sha256":module.digest});
    let native_path = root.join("provenance/manifest.json");
    std::fs::write(&native_path, manifest.to_string()).expect("native manifest");
    std::fs::write(
        root.join("provenance/source-parser/manifest.json"),
        parser.to_string(),
    )
    .expect("parser manifest");
    let initial = EncodedEngine {
        digest: "initial engine".into(),
        process_identity: encoded_process_identity().into(),
        objects: Arc::from([]),
        font_objects: Arc::from([]),
        font_env: None,
    };
    initial
        .clone()
        .bind_clock_pair(&module, &producer)
        .await
        .expect("matched sealed pair");
    initial
        .clone()
        .bind_clock_pair(&native, &producer)
        .await
        .expect("matched native pair");
    assert!(initial
        .clone()
        .bind_clock_pair(&foreign, &producer)
        .await
        .is_err());
    std::fs::write(root.join("plurx-source-parser.wasm"), b"changed parser").expect("swap module");
    assert!(initial
        .clone()
        .bind_clock_pair(&native, &producer)
        .await
        .is_err());
    std::fs::write(root.join("plurx-source-parser.wasm"), b"sealed parser").expect("restore bytes");
    let patch =
        b"--- a/libavformat/demux.c\n+++ b/libavformat/demux.c\n@@ -1 +1 @@\n-old\n+changed\n";
    std::fs::write(root.join("provenance/local-patches/timing.patch"), patch)
        .expect("timing patch");
    manifest["compiled_source"]["local_patches"]["timing.patch"] =
        serde_json::json!(hex::encode(Sha256::digest(patch)));
    std::fs::write(&native_path, manifest.to_string()).expect("updated provenance");
    assert!(
        initial
            .clone()
            .bind_clock_pair(&native, &producer)
            .await
            .is_err(),
        "rehashed timing changes do not prove parser/producer equivalence"
    );
}

#[tokio::test]
async fn engine_object_digest_is_reused_per_version_and_rehashed_on_change() {
    use sha2::Digest as _;
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("engine-object");
    std::fs::write(&path, b"first engine bytes").expect("object");
    let (first, version) = super::hash_engine_object(&path).await.expect("first hash");
    assert_eq!(first, sha2::Sha256::digest(b"first engine bytes").to_vec());
    assert_eq!(
        super::cached_engine_digest(&path, &version),
        Some(first.clone()),
        "a captured version is served without reading the object again"
    );
    let (again, same_version) = super::hash_engine_object(&path).await.expect("cached");
    assert_eq!((again, same_version), (first.clone(), version.clone()));
    // A replacement changes the cheap object version (size, ctime, inode).
    std::fs::write(&path, b"replaced engine object bytes").expect("replacement");
    let (replaced, replaced_version) = super::hash_engine_object(&path).await.expect("rehash");
    assert_ne!(replaced_version, version);
    assert_eq!(
        replaced,
        sha2::Sha256::digest(b"replaced engine object bytes").to_vec()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn cancelled_probe_reaps_child_before_returning() {
    let directory = tempfile::tempdir().expect("directory");
    let marker = directory.path().join("child.pid");
    let mut command = tokio::process::Command::new("sh");
    command
        .args([
            "-c",
            "echo $$ > \"$1\"; exec sleep 60",
            "durable-queue-test",
        ])
        .arg(&marker);
    let cancel = tokio_util::sync::CancellationToken::new();
    let child_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        super::bounded_command_output_cancellable(
            command,
            std::time::Duration::from_secs(60),
            1024,
            "cancel regression",
            Some(&child_cancel),
            crate::process_control::ChildWork::background("cancel regression"),
        )
        .await
    });
    let pid = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Ok(text) = tokio::fs::read_to_string(&marker).await {
                if let Ok(pid) = text.trim().parse::<i32>() {
                    break pid;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("child started");
    cancel.cancel();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .expect("cancel joined")
        .expect("task");
    assert!(result.is_err());
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "child remains alive after admission could be released"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}
/// Admission as a boolean. Production reads the richer comparison; these
/// regressions are about the verdict, which must stay identical.
fn probes_describe_same_input(stored: &str, held: &str) -> Result<bool, String> {
    Ok(super::compare_probe_documents(stored, held)?.same)
}

/// The Dolby Vision proof consumes non-comment frame hashes from stdout.
/// Keep the fixture independent of private media while exercising the
/// same ffmpeg output shape and the production process helper.
#[tokio::test]
#[ignore = "needs ffmpeg"]
async fn dovi_probe_output_receives_frame_hashes() {
    let mut command = tokio::process::Command::new(super::ffmpeg_bin());
    command
        .args(["-hide_banner", "-loglevel", "error"])
        .args(["-f", "lavfi", "-i", "testsrc=size=64x64:rate=1:duration=2"])
        .args(["-frames:v", "2", "-an", "-f", "framemd5", "-"]);
    let output = crate::process_control::output_job_owned(
        &mut command,
        crate::process_control::ChildWork::background("test"),
    )
    .await
    .expect("framemd5 probe output");
    assert!(
        output.status.success(),
        "framemd5 probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let hashes: Vec<_> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(str::to_owned)
        .collect();
    assert!(!hashes.is_empty(), "framemd5 stdout had no frame hashes");
}

/// A production file on `media1`: 4K HEVC Dolby Vision Profile 8 in MP4
/// with E-AC-3 Atmos, scanned by the reporter the daemon used then —
/// which is not the one it probes the held descriptor with now.
fn legacy_reporter_probe() -> serde_json::Value {
    serde_json::from_str(
            r#"{
            "streams": [
                {"index": 0, "codec_type": "video", "codec_name": "hevc",
                 "profile": "Main 10", "codec_tag_string": "hvc1",
                 "width": 3840, "height": 2076, "coded_width": 3840, "coded_height": 2076,
                 "pix_fmt": "yuv420p10le", "level": 153, "color_range": "tv",
                 "color_space": "bt2020nc", "color_transfer": "smpte2084",
                 "color_primaries": "bt2020", "chroma_location": "topleft",
                 "field_order": "progressive", "has_b_frames": 2,
                 "extradata_size": 110, "nal_length_size": "4",
                 "r_frame_rate": "24000/1001", "avg_frame_rate": "24000/1001",
                 "time_base": "1/24000", "start_pts": 0, "start_time": "0.000000",
                 "closed_captions": 0, "film_grain": 0, "refs": 1,
                 "disposition": {"default": 1, "forced": 0, "attached_pic": 0,
                                 "hearing_impaired": 0, "visual_impaired": 0},
                 "tags": {"language": "eng", "handler_name": "VideoHandler",
                          "vendor_id": "[0][0][0][0]"},
                 "side_data_list": [{"side_data_type": "DOVI configuration record",
                                     "dv_version_major": 1, "dv_version_minor": 0,
                                     "dv_profile": 8, "dv_level": 6,
                                     "rpu_present_flag": 1, "el_present_flag": 0,
                                     "bl_present_flag": 1,
                                     "dv_bl_signal_compatibility_id": 1}]},
                {"index": 1, "codec_type": "audio", "codec_name": "eac3",
                 "codec_tag_string": "ec-3", "channels": 6, "sample_rate": "48000",
                 "channel_layout": "5.1(side)", "sample_fmt": "fltp",
                 "time_base": "1/48000", "start_pts": 0, "start_time": "0.000000",
                 "disposition": {"default": 1, "forced": 0, "attached_pic": 0,
                                 "hearing_impaired": 0, "visual_impaired": 0},
                 "tags": {"language": "eng", "handler_name": "SoundHandler",
                          "vendor_id": "[0][0][0][0]"}}
            ],
            "chapters": [{"id": 0, "time_base": "1/1000", "start": 0,
                          "start_time": "0.000000", "end": 600000,
                          "end_time": "600.000000"}],
            "format": {"filename": "/media/movies/Harbor Lights (2024)/Harbor Lights (2024) WEBDL-2160p.mp4",
                       "format_name": "mov,mp4,m4a,3gp,3g2,mj2", "nb_streams": 2,
                       "duration": "11361.520000", "size": "81604378624",
                       "bit_rate": "2231402"}
        }"#,
        )
        .expect("fixture")
}

/// The same bytes through the reporter the daemon probes with now: every
/// one of these differences was measured on `media1` on 2026-09-17 between
/// FFprobe 5.1.9 and jellyfin-ffmpeg 8.1.2 over one unchanged file.
fn current_reporter_probe() -> serde_json::Value {
    let mut current = legacy_reporter_probe();
    current["format"]["filename"] = serde_json::json!("/dev/fd/3");
    current["format"]["nb_stream_groups"] = serde_json::json!(0);
    current["format"]["duration"] = serde_json::json!("11361.477000");
    current["format"]["bit_rate"] = serde_json::json!("2231440");
    for stream in current["streams"].as_array_mut().expect("streams") {
        let fields = stream.as_object_mut().expect("stream");
        for dropped in ["closed_captions", "film_grain", "refs"] {
            fields.remove(dropped);
        }
        fields.insert("view_ids_available".to_owned(), serde_json::json!(""));
        fields.insert("view_pos_available".to_owned(), serde_json::json!(""));
        stream["disposition"]["multilayer"] = serde_json::json!(0);
        stream["disposition"]["non_diegetic"] = serde_json::json!(0);
        stream["tags"]
            .as_object_mut()
            .expect("tags")
            .remove("vendor_id");
    }
    current["streams"][0]["side_data_list"][0]["dv_md_compression"] = serde_json::json!("none");
    current["streams"][1]["profile"] = serde_json::json!("Dolby Digital Plus + Dolby Atmos");
    current["streams"][1]["initial_padding"] = serde_json::json!(0);
    current["streams"][1]["mime_codec_string"] = serde_json::json!("ec-3");
    current["streams"][1]["dmix_mode"] = serde_json::json!("0");
    current["streams"][1]["start_pts"] = serde_json::json!(-5);
    current["streams"][1]["start_time"] = serde_json::json!("-0.005000");
    current["streams"][1]["tags"]["name"] = serde_json::json!("GROUP DDP5.1 Atmos");
    plurx_core::scan::probe::stamp_reporter(&mut current, "ffprobe version 8.1.2-Jellyfin");
    current
}

/// A stored probe carrying the HEVC parameter-set census still describes
/// the bytes a fresh probe of the same file reads. Without this every
/// encoded session of a censused HEVC title was refused as
/// `vod_source_rescan_required`, and a reanalysis only earned it a new
/// census and the same refusal.
#[test]
fn a_recorded_hevc_census_is_not_a_change_in_the_source() {
    let held = current_reporter_probe();
    let mut stored = held.clone();
    stored[plurx_core::transcode::hevc_census::PROBE_KEY] = serde_json::json!({
        "revision": 1, "verdict": "varying", "size": 1, "mtime": 1,
        "samples": 7, "differing": 7,
    });
    let comparison =
        super::compare_probe_documents(&stored.to_string(), &held.to_string()).expect("compare");
    assert!(comparison.same, "{:?}", comparison.differences);
    assert!(
        !comparison.admitted_on_reporter_drift,
        "one build on both sides compares the whole document"
    );
    // Still a real comparison: a changed fact beside the record refuses.
    stored["streams"][0]["width"] = serde_json::json!(1920);
    assert!(
        !super::compare_probe_documents(&stored.to_string(), &held.to_string())
            .expect("compare")
            .same
    );
}

/// A title's FEL qualification verdict is stored beside its Dolby Vision
/// facts in the probe document. Without this exemption storing a verdict
/// made every later encoded session of the title refuse as
/// `vod_source_rescan_required` ("/plurx_dv_fel_v1 missing").
#[test]
fn a_fel_qualification_verdict_is_not_a_change_in_the_source() {
    let held = current_reporter_probe();
    let mut stored = held.clone();
    stored[plurx_core::transcode::dv_fel_qualification::PROBE_KEY] = serde_json::json!({
        "schema": 1, "identity": "a".repeat(64), "admitted": false,
    });
    let comparison =
        super::compare_probe_documents(&stored.to_string(), &held.to_string()).expect("compare");
    assert!(comparison.same, "{:?}", comparison.differences);
    stored["streams"][0]["width"] = serde_json::json!(1920);
    assert!(
        !super::compare_probe_documents(&stored.to_string(), &held.to_string())
            .expect("compare")
            .same
    );
}

/// Background content analysis adds application metadata without changing
/// media. Same-reporter comparison must ignore that record alone, including
/// unavailable/negative reports, while still rejecting actual stream drift.
#[test]
fn a_content_encoding_report_is_not_a_source_change_for_the_same_reporter() {
    let held = current_reporter_probe();
    for outcome in ["measured", "scorer_unavailable", "bounded_failure"] {
        let mut stored = held.clone();
        stored[plurx_core::store::CONTENT_ENCODING_PROBE_KEY] = serde_json::json!({
            "context": {"version": 1, "engine": "fixture", "threads": 2},
            "outcome": outcome, "source_sha256": "a".repeat(64), "windows": [],
        });
        let comparison = super::compare_probe_documents(&stored.to_string(), &held.to_string())
            .expect("compare unchanged source with content metadata");
        assert!(comparison.same, "{outcome}: {:?}", comparison.differences);
        assert!(!comparison.admitted_on_reporter_drift);

        // Use a fact present in both probes. The current reporter omits
        // `refs`, and optional field omission is not source replacement.
        stored["streams"][0]["width"] = serde_json::json!(1920);
        let changed = super::compare_probe_documents(&stored.to_string(), &held.to_string())
            .expect("compare changed stream with content metadata");
        assert!(!changed.same, "{outcome}: real stream changes must refuse");
        assert!(!changed.admitted_on_reporter_drift);
        assert!(changed.rendered_differences().contains("/streams/0/width"));
    }
}

/// Fenced title publication adds application metadata to the stored probe.
/// Source identity compares the underlying media; title eligibility and its
/// publication fences remain independently enforced by their owners.
#[test]
fn a_dv_title_report_is_not_a_source_change_for_either_reporter_direction() {
    use plurx_core::scan::dv_title::{DvTitleStats, TitleConstant, SCHEMA};

    let title = DvTitleStats {
        schema: SCHEMA,
        complete: true,
        decoded_frames: 17_983,
        valid_l1_frames: 17_983,
        missing_l1_frames: 0,
        invalid_l1_frames: 0,
        max_l1_pq: Some(3441),
        mastering_luminance: TitleConstant::Unknown,
        mastering_primaries: TitleConstant::Unknown,
    };
    let held = current_reporter_probe();
    for different_reporter in [false, true] {
        let mut stored = held.clone();
        stored[plurx_core::scan::dv_title::PROBE_FIELD] =
            serde_json::to_value(title).expect("title report");
        if different_reporter {
            plurx_core::scan::probe::stamp_reporter(&mut stored, "ffprobe version 5.1.9");
        }
        // Normalization is symmetric: neither operand's application metadata
        // can make unchanged media appear replaced.
        for (left, right) in [(&stored, &held), (&held, &stored)] {
            let comparison = super::compare_probe_documents(&left.to_string(), &right.to_string())
                .expect("compare title enrichment");
            assert!(comparison.same, "{:?}", comparison.differences);
            assert_eq!(comparison.admitted_on_reporter_drift, different_reporter);
        }

        stored["streams"][0]["width"] = serde_json::json!(1920);
        let changed = super::compare_probe_documents(&stored.to_string(), &held.to_string())
            .expect("compare changed media beside title enrichment");
        assert!(!changed.same);
        assert!(changed.rendered_differences().contains("/streams/0/width"));
    }

    // Only the exact application-owned key is excluded. The same reporter's
    // strict comparison still refuses an unrelated, unknown root field.
    let mut unknown = held.clone();
    unknown[plurx_core::scan::dv_title::PROBE_FIELD] =
        serde_json::to_value(title).expect("title report");
    unknown["unexpected_probe_field"] = serde_json::json!(true);
    let comparison = super::compare_probe_documents(&unknown.to_string(), &held.to_string())
        .expect("compare unrelated root field");
    assert!(!comparison.same);
    assert!(!comparison.admitted_on_reporter_drift);
    assert!(comparison
        .rendered_differences()
        .contains("/<unknown-field> missing"));
}

/// The production failure this exists to stop: three refusals on `media1`
/// inside four minutes, on a file nothing had touched, because the scan
/// and the held-descriptor probe came from different FFprobe builds. 5,061
/// of the 5,955 files in that library carry a scan from the older one, so
/// this is not one title.
#[test]
fn a_different_reporter_admits_its_schema_drift_over_the_same_bytes() {
    let legacy = legacy_reporter_probe().to_string();
    let current = current_reporter_probe().to_string();
    let comparison = super::compare_probe_documents(&legacy, &current).expect("compare");
    assert!(comparison.same, "{:?}", comparison.differences);
    assert!(comparison.admitted_on_reporter_drift);
    assert!(comparison.differences.is_empty());
    // Symmetric between two named builds, because a replicated catalog can
    // be newer than the node reading it.
    let mut legacy_named: serde_json::Value = serde_json::from_str(&legacy).expect("fixture");
    plurx_core::scan::probe::stamp_reporter(&mut legacy_named, "ffprobe version 5.1.9-0+deb12u1");
    let reversed =
        super::compare_probe_documents(&current, &legacy_named.to_string()).expect("compare");
    assert!(reversed.same, "{:?}", reversed.differences);
    assert!(reversed.admitted_on_reporter_drift);
    let forward =
        super::compare_probe_documents(&legacy_named.to_string(), &current).expect("compare");
    assert!(forward.same, "{:?}", forward.differences);
}

/// The reason the comparison exists is unchanged: a same-size, same-second
/// replacement must not pair fresh bytes with stale geometry, tracks,
/// cadence or color. Every one of these is still a refusal across the same
/// reporter drift that the case above admits whole.
#[test]
fn a_different_reporter_still_refuses_every_measured_media_change() {
    let legacy = legacy_reporter_probe().to_string();
    let drifted = current_reporter_probe();
    let control = super::compare_probe_documents(&legacy, &drifted.to_string()).expect("control");
    assert!(control.same);
    // The premise of every case below: this pair reaches the narrower
    // comparison. Without it the loop would only prove that refusal still
    // works on the whole-document path.
    assert!(control.admitted_on_reporter_drift);
    for (pointer, replacement) in [
        ("/streams/0/width", serde_json::json!(1920)),
        ("/streams/0/height", serde_json::json!(1080)),
        ("/streams/0/pix_fmt", serde_json::json!("yuv420p")),
        ("/streams/0/codec_name", serde_json::json!("h264")),
        ("/streams/0/profile", serde_json::json!("Main")),
        ("/streams/0/level", serde_json::json!(120)),
        ("/streams/0/color_transfer", serde_json::json!("bt709")),
        ("/streams/0/color_primaries", serde_json::json!("bt709")),
        ("/streams/0/r_frame_rate", serde_json::json!("30000/1001")),
        ("/streams/0/avg_frame_rate", serde_json::json!("30000/1001")),
        ("/streams/0/time_base", serde_json::json!("1/30000")),
        ("/streams/0/field_order", serde_json::json!("tt")),
        ("/streams/0/codec_tag_string", serde_json::json!("dvh1")),
        ("/streams/0/disposition/default", serde_json::json!(0)),
        (
            "/streams/0/side_data_list/0/dv_profile",
            serde_json::json!(5),
        ),
        (
            "/streams/0/side_data_list/0/bl_present_flag",
            serde_json::json!(0),
        ),
        ("/streams/1/codec_type", serde_json::json!("data")),
        ("/streams/1/codec_name", serde_json::json!("ac3")),
        ("/streams/1/channels", serde_json::json!(8)),
        ("/streams/1/channel_layout", serde_json::json!("7.1")),
        ("/streams/1/sample_rate", serde_json::json!("44100")),
        ("/streams/1/sample_fmt", serde_json::json!("s16")),
        ("/streams/1/tags/language", serde_json::json!("fra")),
        ("/streams/1/disposition/forced", serde_json::json!(1)),
        ("/format/size", serde_json::json!("81604378625")),
        ("/format/nb_streams", serde_json::json!(3)),
        ("/format/format_name", serde_json::json!("matroska,webm")),
        ("/format/duration", serde_json::json!("11000.000000")),
        ("/chapters/0/end", serde_json::json!(660000)),
    ] {
        let mut held = drifted.clone();
        *held.pointer_mut(pointer).expect(pointer) = replacement;
        let comparison = super::compare_probe_documents(&legacy, &held.to_string()).expect(pointer);
        assert!(!comparison.same, "{pointer} is a measured media fact");
        assert!(!comparison.admitted_on_reporter_drift);
    }
    let mut fewer = drifted.clone();
    fewer["streams"].as_array_mut().expect("streams").pop();
    assert!(
        !super::compare_probe_documents(&legacy, &fewer.to_string())
            .expect("track count")
            .same
    );
    let mut chaptered = drifted.clone();
    chaptered["chapters"]
        .as_array_mut()
        .expect("chapters")
        .push(
            serde_json::json!({"id": 1, "time_base": "1/1000", "start": 600000,
                                     "start_time": "600.000000", "end": 1200000,
                                     "end_time": "1200.000000"}),
        );
    assert!(
        !super::compare_probe_documents(&legacy, &chaptered.to_string())
            .expect("added chapter")
            .same
    );
}

/// Every fact the projection names is compared strictly, including its
/// absence. A replacement that simply does not report a property is not
/// evidence that the property is unknown — the stored scan measured it.
#[test]
fn a_fact_only_one_document_reports_is_a_refusal_not_an_unknown() {
    let legacy = legacy_reporter_probe().to_string();
    for pointer in [
        "/streams/0/color_range",
        "/streams/0/color_transfer",
        "/streams/0/color_primaries",
        "/streams/0/pix_fmt",
        "/streams/0/level",
        "/streams/0/profile",
        "/streams/0/extradata_size",
        "/streams/0/field_order",
        "/streams/1/channel_layout",
        "/streams/1/channels",
        "/streams/1/sample_rate",
        "/streams/1/tags/language",
        "/format/duration",
        "/format/size",
        "/format/format_name",
    ] {
        let mut held = current_reporter_probe();
        let (parent, field) = pointer.rsplit_once('/').expect("pointer");
        held.pointer_mut(parent)
            .expect(parent)
            .as_object_mut()
            .expect(parent)
            .remove(field)
            .unwrap_or_else(|| panic!("{pointer} is in the fixture"));
        assert!(
            !super::compare_probe_documents(&legacy, &held.to_string())
                .expect(pointer)
                .same,
            "{pointer} reported only by the scan must refuse"
        );
        let mut stored = legacy_reporter_probe();
        stored
            .pointer_mut(parent)
            .expect(parent)
            .as_object_mut()
            .expect(parent)
            .remove(field);
        assert!(
            !super::compare_probe_documents(
                &stored.to_string(),
                &current_reporter_probe().to_string()
            )
            .expect(pointer)
            .same,
            "{pointer} reported only by the held probe must refuse"
        );
    }
}

/// Side data is carried whole and keyed by type, so a record this
/// projection never heard of still refuses when it appears or disappears.
/// HDR10+ is the case that matters: `detect_hdr_format` classifies a
/// source from a dynamic-metadata record alone, and an HDR10+ title
/// replaced by a plain HDR10 one differs in nothing else.
#[test]
fn a_side_data_record_that_appears_or_disappears_refuses() {
    let legacy_base = legacy_reporter_probe();
    for record in [
        serde_json::json!({"side_data_type": "HDR Dynamic Metadata SMPTE2094-40 (HDR10+)",
                               "application version": 1}),
        serde_json::json!({"side_data_type": "Mastering display metadata",
                               "max_luminance": "1000/1", "min_luminance": "1/10000"}),
        serde_json::json!({"side_data_type": "Content light level metadata",
                               "max_content": 1000, "max_average": 400}),
        serde_json::json!({"side_data_type": "Display Matrix", "rotation": -90}),
        serde_json::json!({"side_data_type": "Stereo 3D", "type": "side by side"}),
    ] {
        let mut stored = legacy_base.clone();
        stored["streams"][0]["side_data_list"]
            .as_array_mut()
            .expect("side data")
            .push(record.clone());
        assert!(
            !super::compare_probe_documents(
                &stored.to_string(),
                &current_reporter_probe().to_string()
            )
            .expect("compare")
            .same,
            "a dropped {record} must refuse"
        );
        let mut held = current_reporter_probe();
        held["streams"][0]["side_data_list"]
            .as_array_mut()
            .expect("side data")
            .push(record.clone());
        assert!(
            !super::compare_probe_documents(&legacy_base.to_string(), &held.to_string())
                .expect("compare")
                .same,
            "an added {record} must refuse"
        );
    }
    // A Dolby Vision record whose delivery facts moved is still a refusal
    // even though the record type is unchanged.
    let mut held = current_reporter_probe();
    held["streams"][0]["side_data_list"][0]["dv_bl_signal_compatibility_id"] = serde_json::json!(4);
    assert!(
        !super::compare_probe_documents(&legacy_base.to_string(), &held.to_string())
            .expect("compare")
            .same
    );
}

/// A stream list that is not an array cannot be projected, so the
/// whole-document verdict has to stand rather than a comparison of
/// nothing at all.
#[test]
fn a_malformed_stream_list_keeps_the_whole_document_verdict() {
    let legacy = legacy_reporter_probe().to_string();
    for broken in [
        serde_json::json!({}),
        serde_json::Value::Null,
        serde_json::json!("two"),
    ] {
        let mut held = current_reporter_probe();
        held["streams"] = broken.clone();
        let comparison =
            super::compare_probe_documents(&legacy, &held.to_string()).expect("compare");
        assert!(!comparison.same, "streams {broken} must refuse");
        assert!(!comparison.admitted_on_reporter_drift);
    }
    let mut held = current_reporter_probe();
    held.as_object_mut().expect("document").remove("streams");
    assert!(
        !super::compare_probe_documents(&legacy, &held.to_string())
            .expect("compare")
            .same
    );
}

/// The tolerance is relative, so it cannot swallow a quarter of a short
/// clip, and it is exercised at its own boundary.
#[test]
fn the_duration_tolerance_is_relative_and_exact_at_its_boundary() {
    let legacy = legacy_reporter_probe();
    let stored_seconds = 11361.520_f64;
    let tolerance = (stored_seconds * super::FACT_DURATION_DRIFT_FRACTION).clamp(
        super::FACT_DURATION_TOLERANCE_MIN_SECONDS,
        super::FACT_DURATION_TOLERANCE_MAX_SECONDS,
    );
    assert!((tolerance - 1.1361520).abs() > f64::EPSILON || tolerance <= 1.0);
    for (delta, admitted) in [
        (0.043_f64, true),
        (tolerance, true),
        (tolerance + 0.001, false),
        (300.0, false),
    ] {
        let mut held = current_reporter_probe();
        held["format"]["duration"] = serde_json::json!(format!("{:.6}", stored_seconds - delta));
        assert_eq!(
            super::compare_probe_documents(&legacy.to_string(), &held.to_string())
                .expect("compare")
                .same,
            admitted,
            "duration delta {delta}"
        );
    }
    // A four-second clip gets the floor, not a quarter of itself.
    let mut short_stored = legacy_reporter_probe();
    short_stored["format"]["duration"] = serde_json::json!("4.000000");
    let mut short_held = current_reporter_probe();
    short_held["format"]["duration"] = serde_json::json!("3.100000");
    assert!(
        !super::compare_probe_documents(&short_stored.to_string(), &short_held.to_string())
            .expect("compare")
            .same,
        "a 0.9s change in a 4s clip is not reporter drift"
    );
    short_held["format"]["duration"] = serde_json::json!("3.980000");
    assert!(
        super::compare_probe_documents(&short_stored.to_string(), &short_held.to_string())
            .expect("compare")
            .same,
        "20ms on a 4s clip is inside the floor"
    );
    // A duration neither side can state is not silently uncompared.
    for unparsable in [
        serde_json::json!("N/A"),
        serde_json::json!("nan"),
        serde_json::json!("inf"),
    ] {
        let mut held = current_reporter_probe();
        held["format"]["duration"] = unparsable.clone();
        assert!(
            !super::compare_probe_documents(&legacy.to_string(), &held.to_string())
                .expect("compare")
                .same,
            "duration {unparsable} must refuse"
        );
    }
}

/// The held probe naming itself is what proves a different reporter. A
/// probe that could not name itself proves nothing, and must not be read
/// as a difference — that direction weakens the gate.
#[test]
fn an_unnamed_held_probe_is_not_proof_of_a_different_reporter() {
    let mut stored = legacy_reporter_probe();
    plurx_core::scan::probe::stamp_reporter(&mut stored, "ffprobe version 8.1.2-Jellyfin");
    let mut held: serde_json::Value = current_reporter_probe();
    held.as_object_mut()
        .expect("document")
        .remove(plurx_core::scan::probe::REPORTER_FIELD);
    // Identical bytes, one stamped scan, one probe that could not say who
    // it was: the whole-document comparison stands, drift and all.
    let comparison =
        super::compare_probe_documents(&stored.to_string(), &held.to_string()).expect("same");
    assert!(!comparison.same);
    assert!(!comparison.admitted_on_reporter_drift);
}

/// The refusal sentence names the stamp rather than calling it unknown,
/// because on a library scanned before this landed it is the most common
/// difference an operator will read.
#[test]
fn the_reporter_stamp_is_nameable_in_a_refusal() {
    let mut stored = legacy_reporter_probe();
    stored["streams"][0]["width"] = serde_json::json!(1920);
    let mut held = legacy_reporter_probe();
    plurx_core::scan::probe::stamp_reporter(&mut held, "ffprobe version 8.1.2-Jellyfin");
    held["streams"][1]["index"] = serde_json::json!(7);
    let rendered = super::compare_probe_documents(&stored.to_string(), &held.to_string())
        .expect("compare")
        .rendered_differences();
    assert!(rendered.contains("/plurx_probe_reporter"), "{rendered}");
    assert!(!rendered.contains("<unknown-field>"), "{rendered}");
}

/// The relaxation is bought by evidence and nothing else. Two documents
/// from the same reporter are still compared whole, so a field that build
/// reports on both sides still refuses when it moves.
#[test]
fn the_same_reporter_still_demands_the_whole_document() {
    let mut stored = legacy_reporter_probe();
    plurx_core::scan::probe::stamp_reporter(&mut stored, "ffprobe version 8.1.2-Jellyfin");
    let mut held = stored.clone();
    held["streams"][0]["refs"] = serde_json::json!(2);
    let comparison =
        super::compare_probe_documents(&stored.to_string(), &held.to_string()).expect("same");
    assert!(!comparison.same);
    assert!(!comparison.admitted_on_reporter_drift);
    assert_eq!(comparison.rendered_differences(), "/streams/0/refs value");
    let mut unstamped = legacy_reporter_probe();
    unstamped["streams"][0]["refs"] = serde_json::json!(2);
    assert!(
        !super::compare_probe_documents(
            &legacy_reporter_probe().to_string(),
            &unstamped.to_string()
        )
        .expect("two unknown reporters")
        .same,
        "an unproved reporter difference is not a reporter difference"
    );
}

/// Sub-second duration drift over unchanged bytes was measured; a real
/// re-encode is not sub-second.
#[test]
fn duration_drift_is_bounded_by_a_second() {
    let legacy = legacy_reporter_probe().to_string();
    for (duration, admitted) in [
        ("11361.477000", true),
        ("11362.400000", true),
        ("11363.000000", false),
        ("11360.000000", false),
    ] {
        let mut held = current_reporter_probe();
        held["format"]["duration"] = serde_json::json!(duration);
        assert_eq!(
            super::compare_probe_documents(&legacy, &held.to_string())
                .expect(duration)
                .same,
            admitted,
            "duration {duration}"
        );
    }
}

/// The fact comparison pairs streams by position, so an unproved pairing
/// gets no relaxation at all: the whole-document verdict stands.
#[test]
fn an_unpairable_stream_list_keeps_the_whole_document_verdict() {
    let legacy = legacy_reporter_probe().to_string();
    for broken in [
        serde_json::json!(5),
        serde_json::Value::Null,
        serde_json::json!(-1),
        serde_json::json!("1"),
    ] {
        let mut held = current_reporter_probe();
        held["streams"][1]["index"] = broken.clone();
        assert!(
            !super::compare_probe_documents(&legacy, &held.to_string())
                .expect("compare")
                .same,
            "index {broken} is not a pairing"
        );
    }
    let mut held = current_reporter_probe();
    held["streams"][1]
        .as_object_mut()
        .expect("stream")
        .remove("index");
    assert!(
        !super::compare_probe_documents(&legacy, &held.to_string())
            .expect("compare")
            .same
    );
}

/// A reporter that cannot name the codec is not describing the stream the
/// other one described, so the spine is never treated as unknown.
#[test]
fn a_one_sided_codec_identity_is_a_refusal_not_an_unknown() {
    let legacy = legacy_reporter_probe().to_string();
    for field in ["codec_name", "codec_type"] {
        let mut held = current_reporter_probe();
        held["streams"][1]
            .as_object_mut()
            .expect("stream")
            .remove(field);
        let comparison = super::compare_probe_documents(&legacy, &held.to_string()).expect(field);
        assert!(!comparison.same, "{field} omitted on one side");
    }
}

/// reference film G's track arrangement: 4K HEVC, a default TrueHD Atmos track, a
/// second E-AC-3 track, and two AC-3 tracks. Only the two Atmos-capable
/// codecs have a derived profile a newer reporter can add.
fn atmos_capable_probe(
    truehd_profile: Option<serde_json::Value>,
    eac3_profile: Option<serde_json::Value>,
) -> String {
    let mut document = serde_json::json!({
        "streams": [
            {"index": 0, "codec_type": "video", "codec_name": "hevc",
             "width": 3840, "height": 2160, "r_frame_rate": "24000/1001"},
            {"index": 1, "codec_type": "audio", "codec_name": "truehd",
             "channels": 8, "sample_rate": "48000", "channel_layout": "7.1"},
            {"index": 2, "codec_type": "audio", "codec_name": "eac3",
             "channels": 6, "sample_rate": "48000", "channel_layout": "5.1(side)"},
            {"index": 3, "codec_type": "audio", "codec_name": "ac3", "channels": 6},
            {"index": 4, "codec_type": "audio", "codec_name": "ac3", "channels": 2}
        ],
        "chapters": [],
        "format": {"filename": "/media/library/w.mkv", "nb_streams": 5,
                   "duration": "9360.000000", "size": "77309411328"}
    });
    if let Some(profile) = truehd_profile {
        document["streams"][1]["profile"] = profile;
    }
    if let Some(profile) = eac3_profile {
        document["streams"][2]["profile"] = profile;
    }
    document.to_string()
}

fn eac3_atmos() -> serde_json::Value {
    serde_json::json!("Dolby Digital Plus + Dolby Atmos")
}

fn truehd_atmos() -> serde_json::Value {
    serde_json::json!("Dolby TrueHD + Dolby Atmos")
}

/// The reported incident. A July scan omitted `profile` on the E-AC-3
/// track; the playback node's newer FFprobe derives the Atmos name from
/// the extension type A flag. Nothing about the file changed, so the
/// encoded session must be admitted — in either direction, because a
/// replicated catalog can be newer than the node that reads it.
#[test]
fn held_probe_comparison_admits_the_legacy_eac3_atmos_profile_omission() {
    let legacy = atmos_capable_probe(None, None);
    let current = atmos_capable_probe(Some(truehd_atmos()), Some(eac3_atmos()));
    assert!(probes_describe_same_input(&legacy, &current).expect("legacy scan admitted"));
    assert!(probes_describe_same_input(&current, &legacy).expect("newer catalog admitted"));
    let eac3_only = atmos_capable_probe(None, Some(eac3_atmos()));
    assert!(probes_describe_same_input(&legacy, &eac3_only).expect("E-AC-3 alone"));
    assert!(probes_describe_same_input(&eac3_only, &legacy).expect("E-AC-3 alone reversed"));
    let truehd_only = atmos_capable_probe(Some(truehd_atmos()), None);
    assert!(probes_describe_same_input(&legacy, &truehd_only).expect("TrueHD alone"));
    assert!(probes_describe_same_input(&current, &current).expect("identical reports"));
}

/// The exception admits one measured omission and nothing more. Two
/// reported profiles are always compared, and "missing" is not null, an
/// empty string, a number, or a name nobody measured.
#[test]
fn held_probe_comparison_refuses_profile_differences_outside_the_exception() {
    let legacy = atmos_capable_probe(None, None);
    let atmos = atmos_capable_probe(None, Some(eac3_atmos()));
    let plain = atmos_capable_probe(None, Some(serde_json::json!("Dolby Digital Plus")));
    assert!(!probes_describe_same_input(&atmos, &plain).expect("reported disagreement"));
    assert!(!probes_describe_same_input(&plain, &atmos).expect("reported disagreement"));
    for reported in [
        serde_json::Value::Null,
        serde_json::json!(""),
        serde_json::json!(5),
        serde_json::json!("Dolby Digital Plus + Atmos"),
        truehd_atmos(),
    ] {
        let held = atmos_capable_probe(None, Some(reported.clone()));
        assert!(
            !probes_describe_same_input(&legacy, &held).expect("compare probes"),
            "{reported} must not read as the measured omission"
        );
        assert!(
            !probes_describe_same_input(&held, &legacy).expect("compare probes"),
            "{reported} must not read as the measured omission, reversed"
        );
    }
}

/// The pairing is by codec and by an explicit, equal, non-negative integer
/// index. An Atmos name on any other codec, or a stream list that moved,
/// buys nothing.
#[test]
fn held_probe_comparison_confines_the_atmos_exception_to_its_codecs_and_indices() {
    let base: serde_json::Value =
        serde_json::from_str(&atmos_capable_probe(None, None)).expect("fixture");
    for (at, codec_name, profile) in [
        (3usize, "ac3", "Dolby Digital Plus + Dolby Atmos"),
        (3, "aac", "Dolby Digital Plus + Dolby Atmos"),
        (3, "eac3", "Dolby TrueHD + Dolby Atmos"),
        (0, "hevc", "Dolby TrueHD + Dolby Atmos"),
    ] {
        let mut stored = base.clone();
        stored["streams"][at]["codec_name"] = serde_json::json!(codec_name);
        let mut held = stored.clone();
        held["streams"][at]["profile"] = serde_json::json!(profile);
        assert!(
            !probes_describe_same_input(&stored.to_string(), &held.to_string())
                .expect("compare probes"),
            "{codec_name} must not earn the Atmos exception"
        );
    }
    for broken in [
        serde_json::Value::Null,
        serde_json::json!(-1),
        serde_json::json!("2"),
        serde_json::json!(2.0),
    ] {
        let mut stored = base.clone();
        let mut held = base.clone();
        held["streams"][2]["profile"] = eac3_atmos();
        stored["streams"][2]["index"] = broken.clone();
        held["streams"][2]["index"] = broken.clone();
        assert!(
            !probes_describe_same_input(&stored.to_string(), &held.to_string())
                .expect("compare probes"),
            "index {broken} is not an explicit pairing"
        );
    }
    let mut unindexed = base.clone();
    unindexed["streams"][2]
        .as_object_mut()
        .expect("stream")
        .remove("index");
    let mut unindexed_held = unindexed.clone();
    unindexed_held["streams"][2]["profile"] = eac3_atmos();
    assert!(
        !probes_describe_same_input(&unindexed.to_string(), &unindexed_held.to_string())
            .expect("compare probes")
    );
    let mut renumbered = base.clone();
    renumbered["streams"][2]["profile"] = eac3_atmos();
    renumbered["streams"][2]["index"] = serde_json::json!(5);
    assert!(
        !probes_describe_same_input(&atmos_capable_probe(None, None), &renumbered.to_string())
            .expect("compare probes")
    );
    let mut reordered = base.clone();
    reordered["streams"][2]["profile"] = eac3_atmos();
    let streams = reordered["streams"].as_array_mut().expect("streams");
    streams.swap(1, 2);
    assert!(
        !probes_describe_same_input(&atmos_capable_probe(None, None), &reordered.to_string())
            .expect("compare probes")
    );
}

/// Everything the encoded recipe actually depends on still refuses, with
/// the admitted omission present at the same time.
#[test]
fn held_probe_comparison_still_refuses_media_changes_beside_the_atmos_omission() {
    let legacy = atmos_capable_probe(None, None);
    let current: serde_json::Value = serde_json::from_str(&atmos_capable_probe(
        Some(truehd_atmos()),
        Some(eac3_atmos()),
    ))
    .expect("fixture");
    assert!(probes_describe_same_input(&legacy, &current.to_string()).expect("control"));
    for (pointer, replacement) in [
        ("/streams/2/channels", serde_json::json!(8)),
        ("/streams/2/sample_rate", serde_json::json!("44100")),
        ("/streams/2/channel_layout", serde_json::json!("7.1")),
        ("/streams/2/codec_name", serde_json::json!("ac3")),
        ("/streams/2/codec_type", serde_json::json!("data")),
        ("/streams/0/width", serde_json::json!(1920)),
        ("/streams/0/height", serde_json::json!(1080)),
        ("/streams/0/r_frame_rate", serde_json::json!("30000/1001")),
        ("/streams/0/codec_name", serde_json::json!("h264")),
        ("/format/duration", serde_json::json!("9000.000000")),
        ("/format/size", serde_json::json!("77309411329")),
    ] {
        let mut held = current.clone();
        *held.pointer_mut(pointer).expect(pointer) = replacement;
        assert!(
            !probes_describe_same_input(&legacy, &held.to_string()).expect("compare probes"),
            "{pointer} is a measured media fact"
        );
    }
    let mut fewer = current.clone();
    fewer["streams"].as_array_mut().expect("streams").pop();
    assert!(!probes_describe_same_input(&legacy, &fewer.to_string()).expect("track count"));
    let mut chaptered = current.clone();
    chaptered["chapters"] = serde_json::json!([
        {"id": 0, "start_time": "0.000000", "end_time": "60.000000"}
    ]);
    assert!(!probes_describe_same_input(&legacy, &chaptered.to_string()).expect("added chapter"));
}

/// A refusal has to be diagnosable without dumping a probe into a log.
#[test]
fn held_probe_comparison_names_the_normalized_field_that_refused() {
    let legacy = atmos_capable_probe(None, None);
    let mut held: serde_json::Value = serde_json::from_str(&atmos_capable_probe(
        Some(truehd_atmos()),
        Some(eac3_atmos()),
    ))
    .expect("fixture");
    held["streams"][0]["width"] = serde_json::json!(1920);
    let comparison =
        super::compare_probe_documents(&legacy, &held.to_string()).expect("compare probes");
    assert!(!comparison.same);
    assert!(!comparison.truncated);
    assert_eq!(
        comparison.differences,
        vec![super::ProbeDifference {
            path: "/streams/0/width".to_string(),
            kind: super::ProbeDifferenceKind::Value,
        }]
    );
    assert_eq!(comparison.rendered_differences(), "/streams/0/width value");
    // Removing the deployed exception is what this whole change is about:
    // with it in place the E-AC-3 profile is not a difference at all.
    let admitted = super::compare_probe_documents(
        &legacy,
        &atmos_capable_probe(Some(truehd_atmos()), Some(eac3_atmos())),
    )
    .expect("compare probes");
    assert!(admitted.same);
    assert!(admitted.differences.is_empty());
    // The four kinds are distinguishable.
    let mut typed = held.clone();
    typed["streams"][0]["width"] = serde_json::json!("3840");
    let kinds = super::compare_probe_documents(&legacy, &typed.to_string())
        .expect("compare probes")
        .differences;
    assert_eq!(kinds[0].kind, super::ProbeDifferenceKind::Type);
    let mut shorter = held.clone();
    shorter["streams"].as_array_mut().expect("streams").pop();
    assert_eq!(
        super::compare_probe_documents(&legacy, &shorter.to_string())
            .expect("compare probes")
            .differences[0],
        super::ProbeDifference {
            path: "/streams".to_string(),
            kind: super::ProbeDifferenceKind::Length,
        }
    );
    let mut absent = held.clone();
    absent["streams"][0]
        .as_object_mut()
        .expect("stream")
        .remove("width");
    assert_eq!(
        super::compare_probe_documents(&legacy, &absent.to_string())
            .expect("compare probes")
            .differences[0]
            .kind,
        super::ProbeDifferenceKind::Missing
    );
}

/// The diagnostic is written to an operator log, so it may never carry a
/// title, a pathname, a private container tag name, or an unbounded list.
#[test]
fn held_probe_comparison_diagnostics_stay_bounded_and_carry_no_private_text() {
    let base: serde_json::Value =
        serde_json::from_str(&atmos_capable_probe(None, None)).expect("fixture");
    let mut stored = base.clone();
    stored["format"]["tags"] = serde_json::json!({
        "title": "reference film G (2024) Private Cut",
        "COMPANY_NAME": "a private label"
    });
    let mut held = base.clone();
    held["format"]["tags"] = serde_json::json!({
        "title": "reference film G (2024) Other Cut",
        "COMPANY_NAME": "a different private label"
    });
    let comparison = super::compare_probe_documents(&stored.to_string(), &held.to_string())
        .expect("compare probes");
    assert!(!comparison.same);
    assert!(!comparison.differences.is_empty());
    for difference in &comparison.differences {
        assert_eq!(difference.path, "/format/tags/<field>", "{difference:?}");
    }
    let rendered = comparison.rendered_differences();
    for secret in [
        "reference film G",
        "title",
        "COMPANY_NAME",
        "private",
        "/media/",
    ] {
        assert!(!rendered.contains(secret), "{rendered} leaked {secret}");
    }
    // An unknown field a future FFprobe adds is named by category only.
    let mut many_stored = base.clone();
    let mut many_held = base.clone();
    for at in 0..12 {
        let key = format!("some_future_field_{at}");
        many_stored["streams"][0][key.as_str()] = serde_json::json!(at);
        many_held["streams"][0][key.as_str()] = serde_json::json!(at + 1);
    }
    let many = super::compare_probe_documents(&many_stored.to_string(), &many_held.to_string())
        .expect("compare probes");
    assert_eq!(many.differences.len(), super::PROBE_DIFFERENCE_LIMIT);
    assert!(many.truncated);
    for difference in &many.differences {
        assert_eq!(difference.path, "/streams/0/<unknown-field>");
    }
    assert!(many.rendered_differences().ends_with("…more"));
    // A pathological nesting depth cannot grow a log line without bound.
    let mut deep_leaf = serde_json::json!("leaf");
    let mut deep_other = serde_json::json!("other leaf");
    for _ in 0..40 {
        deep_leaf = serde_json::json!({"a_private_nested_name": deep_leaf});
        deep_other = serde_json::json!({"a_private_nested_name": deep_other});
    }
    let mut deep_stored = base.clone();
    deep_stored["format"]["tags"] = serde_json::json!({"chain": deep_leaf});
    let mut deep_held = base.clone();
    deep_held["format"]["tags"] = serde_json::json!({"chain": deep_other});
    let deep_comparison =
        super::compare_probe_documents(&deep_stored.to_string(), &deep_held.to_string())
            .expect("compare probes");
    assert!(!deep_comparison.same);
    assert!(!deep_comparison.differences.is_empty());
    for difference in &deep_comparison.differences {
        assert!(
            difference.path.chars().count() <= super::PROBE_PATH_MAX_CHARS,
            "{} is unbounded",
            difference.path
        );
        assert!(!difference.path.contains("a_private_nested_name"));
        assert!(!difference.path.contains("chain"));
    }
}

use super::*;

#[cfg(target_os = "linux")]
#[tokio::test]
async fn linux_clock_pair_requires_bound_reporter_origin_and_retained_capsule() {
    use plurx_core::transcode::{
        LinuxDolbyExecutionBinding, LinuxExecutionObject, LinuxExecutionObjectKind,
        LinuxSourceClockAssociation,
    };
    let base = crate::test_tempdir().expect("Linux clock package");
    let producer_path = base.path().join("ffmpeg");
    let reporter_path = base.path().join("ffprobe");
    let parser_path = base.path().join("sealed-parser");
    let foreign_path = base.path().join("different-origin-ffprobe");
    let capsule = base.path().join("package.json");
    for (path, bytes) in [
        (&producer_path, b"producer".as_slice()),
        (&reporter_path, b"reporter"),
        (&parser_path, b"sealed parser"),
        (&foreign_path, b"reporter"),
        (&capsule, b"bound capsule"),
    ] {
        std::fs::write(path, bytes).expect("clock package object");
    }
    let producer = EncodedExecutable::capture_at(producer_path)
        .await
        .expect("producer capture");
    let reporter = EncodedExecutable::capture_at(reporter_path)
        .await
        .expect("reporter capture");
    let parser = EncodedExecutable::capture_at(parser_path)
        .await
        .expect("parser capture");
    let foreign = EncodedExecutable::capture_at(foreign_path)
        .await
        .expect("foreign origin capture");
    let (capsule_digest, capsule_version) =
        hash_engine_object(&capsule).await.expect("capsule capture");
    let binding = LinuxDolbyExecutionBinding::new(
        vec![
            LinuxExecutionObject {
                path: producer.path.clone(),
                version: Some(producer.object_version.clone()),
                kind: LinuxExecutionObjectKind::ProducerExecutable,
            },
            LinuxExecutionObject {
                path: reporter.path.clone(),
                version: Some(reporter.object_version.clone()),
                kind: LinuxExecutionObjectKind::NativeReporterExecutable,
            },
            LinuxExecutionObject {
                path: parser.path.clone(),
                version: Some(parser.object_version.clone()),
                kind: LinuxExecutionObjectKind::SealedParserExecutable,
            },
            LinuxExecutionObject {
                path: capsule.clone(),
                version: Some(capsule_version),
                kind: LinuxExecutionObjectKind::FollowedFile,
            },
        ],
        vec![],
    )
    .expect("clock role binding")
    .with_source_clock(
        LinuxSourceClockAssociation::new(
            hex::encode(capsule_digest),
            "5".repeat(64),
            reporter.digest.clone(),
            parser.digest.clone(),
        )
        .expect("matched source witness"),
    )
    .expect("reporter roles");
    let producer = producer
        .bind_linux_execution(binding)
        .expect("producer role");
    let engine = || EncodedEngine {
        digest: "base engine".into(),
        process_identity: encoded_process_identity().into(),
        objects: Arc::from([]),
        font_objects: Arc::from([]),
        font_env: None,
    };
    let mut valid = engine();
    valid
        .bind_clock_pair(&reporter, &producer)
        .await
        .expect("actual native reporter association");
    let mut sealed = engine();
    sealed
        .bind_clock_pair(&parser, &producer)
        .await
        .expect("actual sealed reporter association");
    assert_ne!(
        valid.digest, sealed.digest,
        "reporter image role remains in the clock identity"
    );
    assert_eq!(
        foreign.digest, reporter.digest,
        "same bytes are intentionally not enough"
    );
    assert!(
        engine().bind_clock_pair(&foreign, &producer).await.is_err(),
        "different loader origin cannot claim the package reporter"
    );
    let replacement = base.path().join("replacement.json");
    std::fs::write(&replacement, b"different capsule").expect("changed package");
    std::fs::rename(replacement, capsule).expect("atomic capsule replacement");
    assert!(
        engine()
            .bind_clock_pair(&reporter, &producer)
            .await
            .is_err(),
        "captured source-package replacement refuses association"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn linux_processing_binding_fences_driver_lookup_and_package_replacement() {
    use plurx_core::transcode::{
        LinuxDolbyExecutionBinding, LinuxExecutionObject, LinuxExecutionObjectKind,
    };
    let base = crate::test_tempdir().expect("Linux processing capture");
    let encoder = base.path().join("ffmpeg");
    let driver = base.path().join("driver.so");
    let alias = base.path().join("iHD_drv_video.so");
    let shadow = base.path().join("higher-priority-driver.so");
    std::fs::write(&encoder, b"encoder").expect("encoder bytes");
    std::fs::write(&driver, b"driver-a").expect("driver bytes");
    std::os::unix::fs::symlink(&driver, &alias).expect("driver alias");
    let mut captured = EncodedExecutable::capture_at(encoder)
        .await
        .expect("capture encoder");
    captured.linux_binding = Some(
        LinuxDolbyExecutionBinding::new(
            vec![
                LinuxExecutionObject {
                    path: captured.path.clone(),
                    version: Some(captured.object_version.clone()),
                    kind: LinuxExecutionObjectKind::ProducerExecutable,
                },
                LinuxExecutionObject {
                    path: driver.clone(),
                    version: Some(engine_path_version(&driver).expect("driver version")),
                    kind: LinuxExecutionObjectKind::FollowedFile,
                },
                LinuxExecutionObject {
                    path: alias.clone(),
                    version: Some(
                        engine_object_version(
                            &std::fs::symlink_metadata(&alias).expect("alias metadata"),
                        )
                        .expect("alias version"),
                    ),
                    kind: LinuxExecutionObjectKind::LookupObject,
                },
                LinuxExecutionObject {
                    path: shadow.clone(),
                    version: None,
                    kind: LinuxExecutionObjectKind::AbsentLookup,
                },
            ],
            vec![("LIBVA_DRIVER_NAME_JELLYFIN".into(), "iHD".into())],
        )
        .expect("binding"),
    );
    assert!(captured.is_current().await);
    assert_eq!(
        captured.processing_child_env()[0].0,
        "LIBVA_DRIVER_NAME_JELLYFIN"
    );
    std::fs::write(&shadow, b"new shadow").expect("higher-priority driver");
    assert!(
        !captured.is_current().await,
        "a newly present lookup must not silently replace the observed driver"
    );
    std::fs::remove_file(&shadow).expect("remove shadow");
    assert!(captured.is_current().await);
    std::fs::remove_file(&alias).expect("remove alias");
    std::os::unix::fs::symlink(&driver, &alias).expect("replace alias to identical target");
    assert!(
        !captured.is_current().await,
        "lookup inode changes remain visible even when target content is unchanged"
    );
    let replacement = base.path().join("replacement");
    std::fs::write(&replacement, b"driver-b").expect("replacement driver");
    std::fs::rename(&replacement, &driver).expect("replace driver");
    assert!(
        !captured.is_current().await,
        "retained package capture cannot recapture a changed driver"
    );
}

#[tokio::test]
async fn linux_processing_retains_finite_preparation_and_refuses_identical_other_loader_origin() {
    use plurx_core::transcode::{
        LinuxDolbyExecutionBinding, LinuxExecutionObject, LinuxExecutionObjectKind,
    };
    let base = crate::test_tempdir().expect("Linux loader origins");
    let a = base.path().join("package-a");
    let b = base.path().join("package-b");
    std::fs::create_dir(&a).expect("first package");
    std::fs::create_dir(&b).expect("second package");
    let a = a.join("ffmpeg");
    let b = b.join("ffmpeg");
    std::fs::write(&a, b"identical encoder").expect("first encoder");
    std::fs::write(&b, b"identical encoder").expect("second encoder");
    let first = EncodedExecutable::capture_at(a)
        .await
        .expect("capture first");
    let second = EncodedExecutable::capture_at(b)
        .await
        .expect("capture second");
    assert_eq!(
        first.digest, second.digest,
        "content equality cannot prove loader-origin equality"
    );
    let binding = LinuxDolbyExecutionBinding::new(
        vec![LinuxExecutionObject {
            path: first.path.clone(),
            version: Some(first.object_version.clone()),
            kind: LinuxExecutionObjectKind::ProducerExecutable,
        }],
        vec![],
    )
    .expect("captured binding");
    assert!(second.bind_linux_execution(binding.clone()).is_err());
    let frozen = first
        .bind_linux_execution(binding)
        .expect("original origin");
    let retained = EncodedExecutable::capture_for_preparation(&frozen)
        .await
        .expect("retain finite preparation");
    assert_eq!(retained.path, frozen.path);
    assert_eq!(retained.object_version, frozen.object_version);
    assert!(retained.linux_binding.is_some());
    assert!(retained.is_current().await);
}

#[tokio::test]
async fn alternating_tool_captures_do_not_rehash_each_other() {
    let base = crate::test_tempdir().expect("tool capture cache");
    let encoder = base.path().join("encoder");
    let reporter = base.path().join("reporter");
    std::fs::write(&encoder, b"encoder").expect("encoder bytes");
    std::fs::write(&reporter, b"reporter").expect("reporter bytes");
    let cache = tokio::sync::Mutex::new(ExecutableCaptureCache::default());
    for _ in 0..3 {
        EncodedExecutable::capture_at_with_cache(reporter.clone(), &cache)
            .await
            .expect("reporter capture");
        EncodedExecutable::capture_at_with_cache(encoder.clone(), &cache)
            .await
            .expect("encoder capture");
    }
    assert_eq!(
        cache.lock().await.hashes,
        2,
        "a VOD start's reporter and encoder captures keep both attestations"
    );
}

#[tokio::test]
async fn encoded_executable_capture_shares_hash_and_rechecks_replacement() {
    let base = crate::test_tempdir().expect("encoder capture cache");
    let path = base.path().join("encoder");
    tokio::fs::write(&path, b"encoder-a")
        .await
        .expect("encoder");
    let modified = std::fs::metadata(&path)
        .expect("encoder metadata")
        .modified()
        .expect("encoder mtime");
    let cache = tokio::sync::Mutex::new(ExecutableCaptureCache::default());
    let (first, second) = tokio::join!(
        EncodedExecutable::capture_at_with_cache(path.clone(), &cache),
        EncodedExecutable::capture_at_with_cache(path.clone(), &cache),
    );
    let first = first.expect("first capture");
    assert_eq!(first.digest, second.expect("concurrent capture").digest);
    assert_eq!(
        cache.lock().await.hashes,
        1,
        "one actual hash for concurrent recipes"
    );
    let replacement = base.path().join("replacement");
    std::fs::write(&replacement, b"encoder-b").expect("replacement bytes");
    std::fs::File::options()
        .write(true)
        .open(&replacement)
        .expect("replacement handle")
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .expect("preserve mtime");
    std::fs::rename(replacement, &path).expect("replace encoder");
    let changed = EncodedExecutable::capture_at_with_cache(path.clone(), &cache)
        .await
        .expect("replacement capture");
    assert_ne!(
        first.digest, changed.digest,
        "same size and mtime do not reuse old bytes"
    );
    assert_eq!(cache.lock().await.hashes, 2);
    tokio::fs::remove_file(&path).await.expect("remove encoder");
    assert!(
        EncodedExecutable::capture_at_with_cache(path, &cache)
            .await
            .is_err(),
        "a missing encoder cannot reuse its cached attestation"
    );
}

#[tokio::test]
async fn encoded_executable_refuses_same_size_mtime_replacement() {
    let base = crate::test_tempdir().expect("engine identity");
    let path = base.path().join("encoder");
    tokio::fs::write(&path, b"encoder-a")
        .await
        .expect("first engine");
    let modified = std::fs::metadata(&path)
        .expect("metadata")
        .modified()
        .expect("mtime");
    let engine = EncodedExecutable::capture_at(path.clone())
        .await
        .expect("capture engine");
    // Exactly the composition `recipe_engine_is_current` uses: the
    // executable is attested inside the engine's blocking batch.
    let closure = EncodedEngine::capture_test_objects(&[], "process-a")
        .await
        .expect("engine closure");
    assert!(closure.is_current_with_executable(&engine).await);
    let replacement = base.path().join("replacement");
    tokio::fs::write(&replacement, b"encoder-b")
        .await
        .expect("new engine");
    std::fs::File::options()
        .write(true)
        .open(&replacement)
        .expect("replacement handle")
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .expect("preserve mtime");
    std::fs::rename(replacement, &path).expect("atomic engine replacement");
    assert!(!closure.is_current_with_executable(&engine).await);
    assert_ne!(
        engine.digest,
        EncodedExecutable::capture_at(path)
            .await
            .expect("new capture")
            .digest
    );
}

#[tokio::test]
async fn encoded_engine_refuses_dependency_replacement_and_isolates_processes() {
    let base = crate::test_tempdir().expect("engine closure");
    let dependency = base.path().join("libcodec");
    tokio::fs::write(&dependency, b"codec-a")
        .await
        .expect("dependency");
    let first = EncodedEngine::capture_test_objects(std::slice::from_ref(&dependency), "process-a")
        .await
        .expect("first engine");
    let other_process =
        EncodedEngine::capture_test_objects(std::slice::from_ref(&dependency), "process-b")
            .await
            .expect("other process");
    assert_ne!(first.digest, other_process.digest);
    assert!(first.is_current().await);

    let replacement = base.path().join("replacement");
    tokio::fs::write(&replacement, b"codec-b")
        .await
        .expect("replacement dependency");
    std::fs::rename(replacement, dependency).expect("replace dependency");
    assert!(!first.is_current().await);
}

#[tokio::test]
async fn a_touched_engine_object_is_not_current_after_the_move() {
    let base = crate::test_tempdir().expect("engine closure");
    let first = base.path().join("libcodec-a");
    let second = base.path().join("libcodec-b");
    tokio::fs::write(&first, b"codec-a").await.expect("first");
    tokio::fs::write(&second, b"codec-b").await.expect("second");
    let engine = EncodedEngine::capture_test_objects(&[first.clone(), second], "process-a")
        .await
        .expect("engine");
    assert!(engine.is_current().await);

    std::fs::File::options()
        .write(true)
        .open(first)
        .expect("open first")
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
        )
        .expect("touch first");
    assert!(!engine.is_current().await);
}

#[test]
fn a_current_engine_is_current_on_the_blocking_pool() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .max_blocking_threads(1)
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let base = crate::test_tempdir().expect("engine closure");
        let dependency = base.path().join("libcodec");
        tokio::fs::write(&dependency, b"codec")
            .await
            .expect("dependency");
        let engine =
            EncodedEngine::capture_test_objects(std::slice::from_ref(&dependency), "process-a")
                .await
                .expect("engine");

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            started_tx.send(()).expect("announce blocking task");
            release_rx.recv().expect("release blocking task");
        });
        started_rx.recv().expect("blocking task started");
        let check = tokio::spawn(async move { engine.is_current().await });
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(
            !check.is_finished(),
            "the currentness check must queue behind the occupied blocking pool"
        );
        release_tx.send(()).expect("release blocker");
        blocker.await.expect("blocking task");
        assert!(tokio::time::timeout(Duration::from_secs(1), check)
            .await
            .expect("currentness deadline")
            .expect("currentness task"));
    });
}

/// A stale encoder executable must not be detectable without the
/// blocking pool. While the pool is occupied the whole attestation —
/// executable included — has to queue; an inline `std::fs::metadata` on
/// the executable would answer `false` from the runtime thread and the
/// check would finish immediately.
#[test]
fn a_stale_executable_is_detected_on_the_blocking_pool() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .max_blocking_threads(1)
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let base = crate::test_tempdir().expect("engine identity");
        let path = base.path().join("encoder");
        tokio::fs::write(&path, b"encoder-a")
            .await
            .expect("first engine");
        let executable = EncodedExecutable::capture_at(path.clone())
            .await
            .expect("capture engine");
        let closure = EncodedEngine::capture_test_objects(&[], "process-a")
            .await
            .expect("engine closure");
        tokio::fs::write(&path, b"encoder-b-longer")
            .await
            .expect("replace engine");

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            started_tx.send(()).expect("announce blocking task");
            release_rx.recv().expect("release blocking task");
        });
        started_rx.recv().expect("blocking task started");
        let check =
            tokio::spawn(async move { closure.is_current_with_executable(&executable).await });
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(
            !check.is_finished(),
            "the executable must be attested on the blocking pool, not inline on a runtime worker"
        );
        release_tx.send(()).expect("release blocker");
        blocker.await.expect("blocking task");
        assert!(
            !tokio::time::timeout(Duration::from_secs(1), check)
                .await
                .expect("currentness deadline")
                .expect("currentness task"),
            "a replaced executable must withdraw the recipe"
        );
    });
}

/// One attestation charges each series once, and a burn recipe's media
/// dependency closure is charged under `media` even though the recipe is
/// a font one. Observing per internal batch made the burn path emit three
/// `font,stat` observations per check and none under `media`.
#[tokio::test]
async fn one_attestation_charges_each_series_once_by_what_it_stats() {
    let base = crate::test_tempdir().expect("engine closure");
    let dependency = base.path().join("libcodec");
    let font = base.path().join("font-a");
    tokio::fs::write(&dependency, b"codec")
        .await
        .expect("dependency");
    tokio::fs::write(&font, b"font").await.expect("font");

    let media = EncodedEngine::capture_test_objects(std::slice::from_ref(&dependency), "p")
        .await
        .expect("media engine");
    assert_eq!(
        media.is_current_charged(None).await.1.charged(),
        vec![("media", "stat", 1)],
        "a non-burn check stats the dependency closure and nothing else"
    );

    // The burn half — one `font,stat` observation and no `font,spawn`
    // per check — needs a real frozen environment and is pinned by
    // `fontenv::tests::a_new_system_font_does_not_withdraw_a_frozen_recipe`.
    let _ = font;
}

#[tokio::test]
async fn font_object_versions_are_computed_in_one_blocking_task() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let base = crate::test_tempdir().expect("font closure");
    let paths = [base.path().join("font-a"), base.path().join("font-b")];
    for path in &paths {
        tokio::fs::write(path, b"font").await.expect("font object");
    }
    let tasks = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&tasks);
    let (versions, _elapsed) =
        font_object_versions_observed(paths.into_iter().collect(), move || {
            observed.fetch_add(1, Ordering::Relaxed);
        })
        .await;
    assert!(versions.errors.is_empty(), "{:?}", versions.errors);
    assert_eq!(versions.objects.len(), 2);
    assert_eq!(tasks.load(Ordering::Relaxed), 1);
}

#[test]
fn engine_attestation_metrics_render_all_four_series() {
    let rendered = engine_attestation_prometheus();
    assert!(rendered.contains("# TYPE plurx_engine_attestation_seconds histogram"));
    for kind in ["media", "font"] {
        for phase in ["spawn", "stat"] {
            assert!(rendered.contains(&format!(
                "plurx_engine_attestation_seconds_count{{kind=\"{kind}\",phase=\"{phase}\"}}"
            )));
        }
    }
}

#[test]
fn font_inventory_keeps_a_load_tolerant_probe_budget() {
    assert!(FONT_ENGINE_PROBE_TIMEOUT >= Duration::from_secs(30));
    assert!(FONT_ENGINE_PROBE_TIMEOUT > ENGINE_PROBE_TIMEOUT);
}

#[test]
fn font_identity_ignores_inactive_configuration_inventory() {
    fn identity(configuration: &[u8]) -> Vec<u8> {
        let mut digest = Sha256::new();
        hash_active_font_configuration(&mut digest, configuration);
        digest.finalize().to_vec()
    }
    let consumed = b"+ /fonts/conf.d/10-hinting.conf: Hinting\n+ /fonts/fonts.conf: Root\n";
    let with_available = b"- /build/conf.avail/00-unused.conf: Unused\n+ /fonts/conf.d/10-hinting.conf: Hinting\n- /other/99-unused.conf: Other\n+ /fonts/fonts.conf: Root\n";
    assert_eq!(identity(consumed), identity(with_available));
    assert_ne!(
        identity(consumed),
        identity(b"+ /fonts/conf.d/11-hinting.conf: Hinting\n+ /fonts/fonts.conf: Root\n")
    );
    assert_ne!(
        identity(consumed),
        identity(b"+ /fonts/fonts.conf: Root\n+ /fonts/conf.d/10-hinting.conf: Hinting\n")
    );
}

#[tokio::test]
async fn text_renderer_attests_active_font_rules_and_files() {
    plurx_core::testfixtures::require_ffmpeg();
    let runtime_cache = crate::test_tempdir().expect("runtime cache");
    let engine = EncodedEngine::capture(Some(runtime_cache.path()))
        .await
        .expect("text renderer attestation");
    assert!(engine.is_current().await);
}

#[test]
fn held_probe_comparison_accepts_empty_chapters_but_detects_chapter_changes() {
    // Production file 1323 has no chapters. Its older scan omitted the
    // chapters key, while the held-source probe explicitly reports [].
    let scanned = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1920}],"format":{"duration":"3660.0"}}"#;
    let mut held: serde_json::Value = serde_json::from_str(scanned).expect("fixture");
    held["chapters"] = serde_json::json!([]);
    let without_chapters = held.to_string();
    assert!(probes_describe_same_input(scanned, &held.to_string()).expect("empty chapters"));
    assert!(probes_describe_same_input(&held.to_string(), scanned).expect("omitted chapters"));

    held["chapters"] = serde_json::json!([{
        "id": 0, "time_base": "1/1000", "start": 0, "end": 60000
    }]);
    let with_chapter = held.to_string();
    // Production file 2 has 24 chapters, but its old scan measured none.
    assert!(probes_describe_same_input(scanned, &with_chapter).expect("unmeasured chapters"));
    assert!(probes_describe_same_input(&with_chapter, scanned).expect("unmeasured chapters"));
    assert!(!probes_describe_same_input(&without_chapters, &with_chapter).expect("added chapter"));
    assert!(
        !probes_describe_same_input(&with_chapter, &without_chapters).expect("removed chapter")
    );
    held["chapters"][0]["end"] = serde_json::json!(61000);
    assert!(
        !probes_describe_same_input(&with_chapter, &held.to_string())
            .expect("changed chapter timing")
    );
    held["chapters"] = serde_json::json!(null);
    assert!(!probes_describe_same_input(scanned, &held.to_string()).expect("malformed chapters"));
}

#[cfg(unix)]
#[tokio::test]
async fn completed_mp4_probe_retains_private_named_file_until_probe_finishes() {
    let directory = crate::test_tempdir().expect("private completed output");
    let bytes = include_bytes!("../../../plurx-core/tests/fixtures/dv-runtime/authored.mp4");
    let held = stage_completed_probe_file(bytes, directory.path()).expect("stage completed MP4");
    let path = held.path().to_owned();
    assert!(path.exists());
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        held.as_file()
            .metadata()
            .expect("private file")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let document = held_source_probe_json(
        held.as_file(),
        crate::process_control::ChildWork::realtime("test completed output probe"),
    )
    .await
    .expect("actual held completed MP4 probe");
    let document: serde_json::Value = serde_json::from_str(&document).expect("probe JSON");
    let streams = document["streams"].as_array().expect("probed streams");
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0]["codec_name"], "hevc");
    assert_eq!(streams[0]["profile"], "Main 10");
    assert_eq!(streams[0]["width"], 64);
    assert_eq!(streams[0]["height"], 64);
    drop(held);
    assert!(
        !path.exists(),
        "private completed bytes are automatically removed"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn legacy_scan_without_chapters_matches_the_real_held_source_probe() {
    plurx_core::testfixtures::require_ffmpeg();
    let directory = crate::test_tempdir().expect("source fixture");
    let path = directory.path().join("no-chapters.wav");
    // One second of mono 8 kHz PCM, with no chapter metadata.
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&16036u32.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&8000u32.to_le_bytes());
    wav.extend_from_slice(&16000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&16000u32.to_le_bytes());
    wav.resize(16044, 0);
    std::fs::write(&path, wav).expect("write source");
    let metadata = directory.path().join("chapters.txt");
    std::fs::write(
        &metadata,
        ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=1000\ntitle=Chapter one\n",
    )
    .expect("chapter metadata");
    let chaptered = directory.path().join("with-chapters.mka");
    let mut muxer = tokio::process::Command::new(ffmpeg_bin());
    muxer
        .args(["-v", "error", "-i"])
        .arg(&path)
        .arg("-i")
        .arg(&metadata)
        .args(["-map_metadata", "1", "-c", "copy"])
        .arg(&chaptered);
    bounded_command_output(muxer)
        .await
        .expect("chapter fixture");
    for (path, chapter_count) in [(&path, 0), (&chaptered, 1)] {
        let mut scanner = tokio::process::Command::new(ffprobe_bin());
        scanner
            .args([
                "-v",
                "error",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
            ])
            .arg(path);
        let scanned = bounded_command_output(scanner)
            .await
            .expect("legacy scanner probe");
        let scanned = String::from_utf8(scanned.stdout).expect("probe JSON");
        let stored: serde_json::Value = serde_json::from_str(&scanned).expect("stored probe");
        assert!(stored.get("chapters").is_none());
        let source = std::fs::File::open(path).expect("hold source");
        let held = held_source_probe_json(
            &source,
            crate::process_control::ChildWork::background("test fixture probe"),
        )
        .await
        .expect("descriptor-bound probe");
        let current: serde_json::Value = serde_json::from_str(&held).expect("held probe");
        assert_eq!(
            current["chapters"].as_array().expect("chapter array").len(),
            chapter_count
        );
        assert!(probes_describe_same_input(&scanned, &held).expect("legacy source accepted"));
    }
}

#[test]
fn held_probe_comparison_ignores_descriptor_and_optional_schema_drift() {
    let scanned = r#"{"streams":[{"codec_type":"video","width":1920,"closed_captions":0,"film_grain":0,"refs":1}],"format":{"filename":"/media/a.mkv","duration":"60.0"}}"#;
    let held = r#"{"streams":[{"codec_type":"video","width":1920}],"format":{"filename":"/dev/fd/3","duration":"60.0"}}"#;
    assert!(probes_describe_same_input(scanned, held).expect("compare probes"));
    let replacement = held.replace("1920", "1280");
    assert!(!probes_describe_same_input(scanned, &replacement).expect("detect stale probe"));
    let reported_change = scanned.replace("\"refs\":1", "\"refs\":2");
    assert!(!probes_describe_same_input(scanned, &reported_change)
        .expect("compare reported codec facts"));
}

#[test]
fn channel_playback_repair_ignores_measured_optional_audio_report_omissions() {
    let scanned = r#"{
          "streams":[
            {"index":0,"codec_type":"video","codec_name":"hevc","width":3840,"height":2160},
            {"index":1,"codec_type":"audio","codec_name":"eac3","channels":6,
             "dmix_mode":"ltrt","loro_cmixlev":"-3.000000","loro_surmixlev":"-3.000000",
             "ltrt_cmixlev":"-3.000000","ltrt_surmixlev":"-3.000000",
             "mime_codec_string":"ec-3"}
          ],
          "format":{"filename":"/media/leave-the-world-behind.mkv","duration":"8460.000000"}
        }"#;
    let held = r#"{
          "streams":[
            {"index":0,"codec_type":"video","codec_name":"hevc","width":3840,"height":2160},
            {"index":1,"codec_type":"audio","codec_name":"eac3","channels":6}
          ],
          "format":{"filename":"/dev/fd/3","duration":"8460.000000"}
        }"#;

    assert!(
        probes_describe_same_input(scanned, held).expect("compare the measured schema drift"),
        "optional FFmpeg report availability is not a source replacement"
    );
    assert!(
        !probes_describe_same_input(scanned, &held.replace("3840", "1920"))
            .expect("changed geometry remains significant")
    );
    assert!(
        !probes_describe_same_input(scanned, &held.replace("\"index\":1", "\"index\":2"))
            .expect("changed stream identity remains significant")
    );
    let video_report = scanned.replace(
        "\"height\":2160",
        "\"height\":2160,\"mime_codec_string\":\"hvc1.2.4.L153.B0\"",
    );
    assert!(probes_describe_same_input(&video_report, held)
        .expect("MIME codec labels are optional for video as well as audio"));
}

#[test]
fn held_probe_comparison_accepts_added_video_mime_label_but_detects_media_changes() {
    // File 5405 was scanned before FFmpeg reported a video MIME label.
    // The held-descriptor probe adds avc1.640029 to the same H.264 bytes.
    let scanned = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","profile":"High","width":1920,"height":1080,"level":41}],"format":{"duration":"5542.000000"}}"#;
    let held = scanned.replace(
        "\"level\":41",
        "\"level\":41,\"mime_codec_string\":\"avc1.640029\"",
    );
    assert!(probes_describe_same_input(scanned, &held).expect("compare probes"));
    assert!(probes_describe_same_input(&held, scanned).expect("compare probes"));
    for replacement in [
        held.replace("h264", "hevc"),
        held.replace("1920", "1280"),
        held.replace("\"index\":0", "\"index\":1"),
        held.replace("\"level\":41", "\"level\":40"),
        held.replace("5542.000000", "5543.000000"),
    ] {
        assert!(!probes_describe_same_input(scanned, &replacement).expect("compare probes"));
    }
    assert!(
        !probes_describe_same_input(&held, &held.replace("avc1.640029", "avc1.640028"))
            .expect("compare probes")
    );
}

#[test]
fn held_probe_comparison_accepts_measured_ffprobe_defaults_but_not_nondefaults() {
    let scanned = serde_json::json!({
        "format": {"duration": "7920.0"},
        "streams": [
            {"index": 0, "codec_type": "video", "codec_name": "hevc", "disposition": {"default": 1},
             "side_data_list": [{"side_data_type": "DOVI configuration record", "dv_profile": 8}]},
            {"index": 1, "codec_type": "audio", "codec_name": "truehd", "channels": 8, "disposition": {"default": 1}}
        ]
    });
    let mut held = scanned.clone();
    held["format"]["nb_stream_groups"] = serde_json::json!(0);
    held["streams"][0]["view_ids_available"] = serde_json::json!("");
    held["streams"][0]["view_pos_available"] = serde_json::json!("");
    held["streams"][0]["side_data_list"][0]["dv_md_compression"] = serde_json::json!("none");
    held["streams"][1]["initial_padding"] = serde_json::json!(0);
    held["streams"][1]["profile"] = serde_json::json!("Dolby TrueHD + Dolby Atmos");
    for stream in held["streams"].as_array_mut().expect("streams") {
        stream["disposition"]["multilayer"] = serde_json::json!(0);
        stream["disposition"]["non_diegetic"] = serde_json::json!(0);
    }
    let scanned_json = scanned.to_string();
    let held_json = held.to_string();
    assert!(probes_describe_same_input(&scanned_json, &held_json).expect("new probe schema"));
    assert!(probes_describe_same_input(&held_json, &scanned_json).expect("old probe schema"));
    for (pointer, value) in [
        ("/format/nb_stream_groups", serde_json::json!(1)),
        ("/streams/0/view_ids_available", serde_json::json!("1")),
        ("/streams/0/disposition/multilayer", serde_json::json!(1)),
        ("/streams/0/disposition/non_diegetic", serde_json::json!(1)),
        (
            "/streams/0/side_data_list/0/dv_md_compression",
            serde_json::json!("limited"),
        ),
        (
            "/streams/0/side_data_list/0/dv_profile",
            serde_json::json!(5),
        ),
        ("/streams/1/initial_padding", serde_json::json!(1024)),
        (
            "/streams/1/profile",
            serde_json::json!("unrecognized profile"),
        ),
        ("/streams/1/channels", serde_json::json!(6)),
    ] {
        let mut changed = held.clone();
        *changed.pointer_mut(pointer).expect("measured field") = value;
        assert!(
            !probes_describe_same_input(&scanned_json, &changed.to_string())
                .expect("changed media"),
            "{pointer}"
        );
    }
}

/// Owned-lab receipt 2026-10-02, defect 3: every QSV VOD generation logged
/// `WARN VOD producer diagnostic ... libva info: VA-API version 1.24.0`.
/// libva's start-up lines are informational; FFmpeg's own diagnostics and
/// `libva error:` lines stay actionable, in order.
#[test]
fn libva_info_lines_are_informational_not_producer_diagnostics() {
    let qsv_start = "libva info: VA-API version 1.24.0\n\
                         libva info: Trying to open /usr/lib/jellyfin-ffmpeg/lib/dri/iHD_drv_video.so\n\
                         libva info: Found init function __vaDriverInit_1_24\n\
                         libva info: va_openDriver() returns 0\n";
    let healthy = classify_diagnostic(qsv_start);
    assert_eq!(healthy.actionable, "");
    assert_eq!(healthy.informational.lines().count(), 4);
    assert!(healthy
        .informational
        .starts_with("libva info: VA-API version 1.24.0"));

    let decoder = "[h264 @ 0x5f5b2ef0c780] number of reference frames (0+5) exceeds max (4; probably corrupt input), discarding one";
    let driver = "libva error: /usr/lib/jellyfin-ffmpeg/lib/dri/iHD_drv_video.so init failed";
    let mixed = classify_diagnostic(&format!("{qsv_start}{decoder}\r\n\n{driver}\n"));
    assert_eq!(mixed.actionable, format!("{decoder}\n{driver}"));
    assert_eq!(mixed.informational, healthy.informational);

    // A recognised prefix later in a line is not the line's start.
    assert!(is_informational_diagnostic(
        "  libva info: VA-API version 1.24.0"
    ));
    assert!(!is_informational_diagnostic(driver));
    let quoted = "[mov @ 0x1] could not open 'libva info: x.mkv'";
    assert_eq!(classify_diagnostic(quoted).actionable, quoted);
    assert_eq!(classify_diagnostic(" \n\n"), ProducerDiagnostic::default());
}

#[tokio::test]
async fn producer_diagnostics_drain_but_retain_only_the_bounded_tail() {
    let input = [vec![b'x'; 24 * 1024], b"terminal filter error".to_vec()].concat();
    let tail = drain_diagnostics(input.as_slice()).await;
    assert_eq!(tail.len(), 8 * 1024);
    assert!(tail.ends_with("terminal filter error"));
}

#[cfg(unix)]
#[tokio::test]
async fn cooperative_sample_cancellation_reaps_before_returning_capacity() {
    for pipe in [false, true] {
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "exec sleep 60"]);
        let work = crate::process_control::ChildWork::background("sample cancellation regression");
        let mut owner = if pipe {
            BoundedDiagnosticChild::spawn_piped_output(&mut command, work)
        } else {
            BoundedDiagnosticChild::spawn(&mut command, work)
        }
        .expect("fixture succeeds");
        let (sent, mut received) = tokio::sync::oneshot::channel();
        owner.reaped = Some(sent);
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        let directory = tempfile::tempdir().expect("fixture succeeds");
        if pipe {
            assert!(owner
                .output_to_bounded_file_cancellable(&directory.path().join("sample"), 1024, &cancel)
                .await
                .is_err());
        } else {
            assert!(!owner
                .output_cancellable(&cancel)
                .await
                .expect("fixture succeeds")
                .0
                .success());
        }
        assert!(
            received.try_recv().is_ok(),
            "caller capacity is held until child has reaped"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn bounded_extraction_drains_noisy_child_and_reaps_nonzero_exit() {
    let mut command = tokio::process::Command::new("/bin/sh");
    command.args([
            "-c",
            "i=0; while [ $i -lt 4096 ]; do printf '0123456789abcdef0123456789abcdef' >&2; i=$((i + 1)); done; printf 'terminal extractor error' >&2; printf 'discarded stdout'; exit 7",
        ]);
    let mut owner = BoundedDiagnosticChild::spawn(
        &mut command,
        crate::process_control::ChildWork::background("test"),
    )
    .expect("noisy child");
    let (reaped_tx, reaped_rx) = tokio::sync::oneshot::channel();
    owner.reaped = Some(reaped_tx);
    let (status, tail) = tokio::time::timeout(Duration::from_secs(5), owner.output())
        .await
        .expect("fully drain without a stderr pipe deadlock")
        .expect("wait for noisy child");
    assert_eq!(status.code(), Some(7));
    assert_eq!(tail.len(), 8 * 1024);
    assert!(tail.ends_with("terminal extractor error"));
    reaped_rx.await.expect("nonzero exit still reaped");
}

#[cfg(unix)]
#[tokio::test]
async fn bounded_output_kills_and_reaps_at_the_physical_disk_cap() {
    let base = crate::test_tempdir().expect("bounded output");
    let output = base.path().join("sidecar");
    let mut command = tokio::process::Command::new("/bin/sh");
    command.args([
        "-c",
        "while :; do printf '0123456789abcdef'; printf 'extracting' >&2; done",
    ]);
    let mut owner = BoundedDiagnosticChild::spawn_piped_output(
        &mut command,
        crate::process_control::ChildWork::background("test"),
    )
    .expect("piped child");
    let (reaped_tx, reaped_rx) = tokio::sync::oneshot::channel();
    owner.reaped = Some(reaped_tx);
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        owner.output_to_bounded_file(&output, 4_096),
    )
    .await
    .expect("cap must stop the child")
    .expect_err("oversized output");
    assert!(error.to_string().contains("disk bound"), "{error}");
    assert_eq!(
        std::fs::metadata(output).expect("bounded file").len(),
        4_096
    );
    reaped_rx.await.expect("oversized child reaped");
}

#[cfg(unix)]
#[tokio::test]
async fn bounded_output_reaps_the_child_when_the_destination_cannot_be_created() {
    let base = crate::test_tempdir().expect("bounded output");
    let output = base.path().join("existing-sidecar");
    tokio::fs::write(&output, b"owned by another extractor")
        .await
        .expect("pre-existing sidecar");
    let mut command = tokio::process::Command::new("/bin/sh");
    command.args([
        "-c",
        "while :; do printf 'blocked stdout'; printf 'extracting' >&2; done",
    ]);
    let mut owner = BoundedDiagnosticChild::spawn_piped_output(
        &mut command,
        crate::process_control::ChildWork::background("test"),
    )
    .expect("piped child");
    let (reaped_tx, reaped_rx) = tokio::sync::oneshot::channel();
    owner.reaped = Some(reaped_tx);
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        owner.output_to_bounded_file(&output, 4_096),
    )
    .await
    .expect("destination error must stop the child")
    .expect_err("create_new refuses a pre-existing sidecar");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    reaped_rx.await.expect("failed output child reaped");
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_bounded_extraction_transfers_exact_child_to_reaper() {
    let mut command = tokio::process::Command::new("/bin/sh");
    command.args(["-c", "while :; do printf 'waiting extractor' >&2; done"]);
    let mut owner = BoundedDiagnosticChild::spawn(
        &mut command,
        crate::process_control::ChildWork::background("test"),
    )
    .expect("noisy pending child");
    let pid = owner
        .child
        .as_ref()
        .expect("owned child")
        .id()
        .expect("PID");
    let (reaped_tx, reaped_rx) = tokio::sync::oneshot::channel();
    owner.reaped = Some(reaped_tx);
    let output = tokio::spawn(owner.output());
    tokio::task::yield_now().await;
    output.abort();
    let _ = output.await;
    tokio::time::timeout(Duration::from_secs(5), reaped_rx)
        .await
        .expect("cancellation reaper settles")
        .expect("successful exact child wait");
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

/// A Compose file with an unset variable hands the process `PLURX_FFMPEG=`,
/// and spawning a binary named "" fails with an ENOENT that names nothing.
#[test]
fn an_empty_override_is_not_a_binary_name() {
    assert_eq!(resolve_bin(None, "ffmpeg"), "ffmpeg");
    assert_eq!(resolve_bin(Some(String::new()), "ffprobe"), "ffprobe");
    assert_eq!(
        resolve_bin(Some("/opt/jellyfin-ffmpeg/ffmpeg".to_owned()), "ffmpeg"),
        "/opt/jellyfin-ffmpeg/ffmpeg"
    );
}

#[test]
fn package_siblings_supply_defaults_without_overriding_operator_paths() {
    let package = crate::test_tempdir().expect("package");
    #[cfg(windows)]
    let name = "ffprobe.exe";
    #[cfg(not(windows))]
    let name = "ffprobe";
    let binary = package.path().join(name);
    std::fs::write(&binary, b"packaged probe").expect("package artifact");
    let bundled = plurx_core::process::resolve_media_tool(None, "ffprobe", Some(package.path()));
    assert_eq!(bundled, binary.to_string_lossy());
    assert_eq!(
        resolve_bin(Some("/operator/ffprobe".into()), &bundled),
        "/operator/ffprobe"
    );
    assert_eq!(resolve_bin(None, &bundled), bundled);
    assert_eq!(
        plurx_core::process::resolve_media_tool(None, "ffmpeg", Some(package.path())),
        "ffmpeg"
    );
}

/// Older builds print help and listings to stderr, so a probe that read
/// stdout alone would report every capability as absent on them.
#[test]
fn a_probe_reads_both_streams() {
    assert_eq!(merged_output(b"out\n", b"err\n"), "out\nerr\n");
    // Invalid UTF-8 is replaced rather than dropped: a lossy byte must not
    // take the rest of the listing with it.
    assert!(merged_output(&[0xff, b'\n'], b"dovi_rpu\n").ends_with("dovi_rpu\n"));
}

/// `-bsfs` lists exactly one filter per line. A substring search would
/// claim `dovi_rpu` on any build whose listing merely mentions it — and
/// then every Dolby Vision remux would fail at session start.
#[test]
fn a_bitstream_filter_is_matched_on_a_whole_line() {
    let listing = "Bitstream filters:\n  h264_mp4toannexb\n  dovi_rpu\n  hevc_metadata\n";
    assert!(declares_bsf(listing, "dovi_rpu"));
    assert!(!declares_bsf(listing, "av1_metadata"));
    // The failure the whole-line rule exists for.
    assert!(!declares_bsf(
        "  hevc_metadata (see also dovi_rpu)\n",
        "dovi_rpu"
    ));
}

/// A build that could not be asked must be classified exactly like one
/// that answered "no". Reporting the capability on a failed probe would
/// send every DV remux at a bitstream filter that is not there.
#[test]
fn an_unprobeable_ffmpeg_has_no_dovi_filter() {
    assert!(dovi_from_probe(Ok("  dovi_rpu\n".to_owned())));
    assert!(!dovi_from_probe(Ok("  hevc_metadata\n".to_owned())));
    assert!(!dovi_from_probe(Err(
        "No such file or directory (os error 2)".to_owned()
    )));
    // Truncated output — the spawn succeeded but the listing was cut off
    // mid-name. Half a filter name is not a filter.
    assert!(!dovi_from_probe(Ok(
        "Bitstream filters:\n  dovi_r".to_owned()
    )));
}

#[test]
fn a_dolby_vision_renderer_option_is_matched_as_a_declaration() {
    assert!(declares_filter_option(
            "   apply_dovi <boolean> ..FV....... Apply Dolby Vision metadata if possible (default true)\n",
            "apply_dovi"
        ));
    assert!(!declares_filter_option(
        "   tonemap <int> ..FV....... used with apply_dovi\n",
        "apply_dovi"
    ));
    assert!(!declares_filter_option(
        "   apply_dovi_legacy <boolean> ..FV.......\n",
        "apply_dovi"
    ));
}

/// Same rule for pacing: an ffmpeg that could not be probed gets the
/// pre-5.1 answer, because passing `-readrate` to a build without it is a
/// hard exit rather than a warning.
#[test]
fn an_unprobeable_ffmpeg_gets_the_conservative_pacing_answer() {
    let modern = pacing_from_probe(Ok("  -readrate x\n  -readrate_initial_burst y\n".to_owned()));
    assert!(modern.readrate && modern.initial_burst);

    let failed = pacing_from_probe(Err("Permission denied (os error 13)".to_owned()));
    assert!(!failed.readrate, "a failed probe must not claim -readrate");
    assert!(!failed.initial_burst);
    // And the conservative answer must survive into the flags: nothing is
    // emitted for a transcode, `-re` for the copy path.
    assert!(failed.resolve(2.0, 90.0, false).args().is_empty());
}

/// The 5.1–6.0 middle build, classified through the probe rather than the
/// parser: it keeps `-readrate` and loses only the burst clause. Worth
/// pinning separately because this is the build where the publish gate
/// fills at the paced rate — the flags must still carry the rate, since
/// degrading the whole thing to `-re` would unpace every session on it.
#[test]
fn a_build_with_readrate_but_no_burst_keeps_its_rate() {
    let caps = pacing_from_probe(Ok(
        "  -readrate speed     read input at specified rate\n".to_owned()
    ));
    assert!(caps.readrate, "5.1+ declares -readrate");
    assert!(!caps.initial_burst, "the burst clause arrives in 6.1");
    assert_eq!(
        caps.resolve(2.0, 90.0, true).args(),
        vec!["-readrate", "2.00"],
        "the rate survives; only the burst is dropped"
    );
}

#[test]
fn pacing_caps_come_from_the_help_text() {
    // ffmpeg 6.1+: both flags.
    let modern = "  -re                 read input at native frame rate\n  \
                      -readrate speed     read input at specified rate\n  \
                      -readrate_initial_burst seconds  initial burst\n";
    let caps = parse_pacing_caps(modern);
    assert!(caps.readrate);
    assert!(caps.initial_burst);

    // ffmpeg 5.1–6.0: rate limiting but no burst.
    let caps = parse_pacing_caps("  -readrate speed     read input at specified rate\n");
    assert!(caps.readrate);
    assert!(!caps.initial_burst);

    // Older: neither. Must not be fooled by the substring in -re's help.
    let caps = parse_pacing_caps(
        "  -re                 read input at native frame rate; equivalent to -readrate 1\n",
    );
    assert!(!caps.readrate);
    assert!(!caps.initial_burst);
}

#[test]
fn burst_probe_classifies_honoured_inert_and_failed_measurements() {
    let declared = PacingCaps {
        readrate: true,
        initial_burst: true,
    };
    let honoured = classify_burst(declared, Ok(Duration::from_millis(599)));
    assert!(honoured.initial_burst);
    assert_eq!(
        honoured.resolve(2.0, 90.0, true).args(),
        vec!["-readrate_initial_burst", "90.0", "-readrate", "2.00"]
    );
    for probe in [
        Ok(Duration::from_millis(600)),
        Ok(Duration::from_secs(1)),
        Err("probe failed".to_owned()),
    ] {
        let corrected = classify_burst(declared, probe);
        assert!(!corrected.initial_burst);
        assert_eq!(
            corrected.resolve(2.0, 90.0, true).args(),
            vec!["-readrate", "2.00"]
        );
    }
}

#[test]
fn burst_probe_applies_pacing_to_the_input() {
    let args = burst_probe_args();
    let input = args
        .iter()
        .position(|arg| *arg == "-i")
        .expect("the probe must declare its synthetic input");
    for option in ["-readrate_initial_burst", "-readrate"] {
        let option_index = args
            .iter()
            .position(|arg| *arg == option)
            .unwrap_or_else(|| panic!("the probe must pass {option}"));
        assert!(
            option_index < input,
            "{option} is an input option and must precede -i: {args:?}"
        );
    }
}

#[test]
fn resolve_matches_the_build_and_the_caller() {
    let modern = PacingCaps {
        readrate: true,
        initial_burst: true,
    };
    assert_eq!(
        modern.resolve(2.0, 90.0, true).args(),
        vec!["-readrate_initial_burst", "90.0", "-readrate", "2.00"]
    );
    // Rate 0 means "unpaced" — emit nothing, whatever the build supports.
    assert!(modern.resolve(0.0, 90.0, true).args().is_empty());

    // 5.1–6.0: the rate lands, the burst clause is dropped.
    let rate_only = PacingCaps {
        readrate: true,
        initial_burst: false,
    };
    assert_eq!(
        rate_only.resolve(2.5, 90.0, true).args(),
        vec!["-readrate", "2.50"]
    );

    // Pre-5.1 splits by caller: copy degrades to realtime, transcode to
    // nothing (it was never paced, so `-re` would be a new cap).
    let ancient = PacingCaps::default();
    assert_eq!(ancient.resolve(2.0, 90.0, true).args(), vec!["-re"]);
    assert!(ancient.resolve(2.0, 90.0, false).args().is_empty());
}

#[test]
fn replacing_an_attested_engine_object_withdraws_the_identity() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("ffmpeg");
    let replacement = directory.path().join("replacement");
    std::fs::write(&path, b"engine-a").expect("write original");
    std::fs::write(&replacement, b"engine-b").expect("write replacement");
    let expected = engine_path_version(&path).expect("object version");
    let objects = vec![(path.clone(), expected)];
    assert!(engine_objects_are_current(&objects));
    std::fs::remove_file(&path).expect("unlink original");
    std::fs::rename(replacement, &path).expect("install replacement");
    assert!(!engine_objects_are_current(&objects));
}

#[tokio::test]
async fn process_fragment_engine_baseline_detects_an_actual_object_change() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("libavcodec");
    let replacement = directory.path().join("replacement");
    std::fs::write(&path, b"engine-baseline-a").expect("write baseline");
    std::fs::write(&replacement, b"engine-baseline-b").expect("write replacement");
    let expected = engine_path_version(&path).expect("object version");
    let baseline = FragmentIndexEngine {
        digest: "process-baseline".to_owned(),
        objects: vec![(path.clone(), expected)].into(),
        usable: true,
    };

    assert!(fragment_index_engine_snapshot_is_current(&baseline).await);
    std::fs::remove_file(&path).expect("unlink original");
    std::fs::rename(replacement, &path).expect("install replacement");
    assert!(!fragment_index_engine_snapshot_is_current(&baseline).await);
}

/// The pacing answer comes from `ffmpeg -h full`, and that listing has been
/// over a megabyte on every build since 6.1 (measured: 1,005,926 bytes on
/// 4.4.2, 1,165,847 on 6.1.1, more on 8.x). Read the size class that broke
/// through the real subprocess path, not through a hand-made string: with
/// the old 1 MiB bound this returns Err, `pacing_from_probe` maps Err to
/// the conservative answer, and every remux stream runs unpaced while HLS
/// falls back to realtime pacing — with nothing in the logs but "could not
/// probe ffmpeg".
#[tokio::test]
async fn a_help_listing_larger_than_a_megabyte_survives_the_probe_bound() {
    let mut command = tokio::process::Command::new("sh");
    command.arg("-c").arg(
        "yes '  -pad <int>  ..FV....... filler line standing in for a real option' \
             | head -n 70000; \
             printf '  -readrate speed\\n  -readrate_initial_burst seconds\\n'",
    );
    let out = bounded_command_output(command)
        .await
        .expect("a help listing of the real size must not trip the bound");
    assert!(
        out.stdout.len() as u64 > 1024 * 1024,
        "the fixture has to be the size class that broke: {} bytes",
        out.stdout.len()
    );

    let caps = pacing_from_probe(Ok(merged_output(&out.stdout, &out.stderr)));
    assert!(
        caps.readrate,
        "the declaration is in the listing; only our own bound could hide it"
    );
    assert!(caps.initial_burst);
}

/// The bound still bites — it is a runaway guard, and raising it must not
/// have quietly turned it off. One byte over is refused, and the refusal
/// says what it refused against.
#[tokio::test]
async fn output_past_the_bound_is_still_refused_and_names_the_bound() {
    let over = tokio::io::repeat(b'x').take(ENGINE_PROBE_MAX_BYTES + 1);
    let error = read_bounded(over)
        .await
        .expect_err("one byte past the bound is refused");
    assert!(
        error.contains(&ENGINE_PROBE_MAX_BYTES.to_string()),
        "the error must name the bound it hit: {error}"
    );

    let exact = read_bounded(tokio::io::repeat(b'x').take(ENGINE_PROBE_MAX_BYTES))
        .await
        .expect("the bound itself is fine");
    assert_eq!(exact.len() as u64, ENGINE_PROBE_MAX_BYTES);
}

#[tokio::test]
#[ignore = "nightly runner capability contract"]
async fn nightly_runner_has_ffmpeg_readrate() {
    let caps = pacing_caps().await;
    eprintln!(
        "nightly ffmpeg capability: binary={} readrate={} initial_burst={}",
        ffmpeg_bin(),
        caps.readrate,
        caps.initial_burst,
    );
    assert!(
        caps.readrate,
        "nightly arbitration coverage requires ffmpeg 5.1+ (-readrate)"
    );
}

/// A failed VA-API Main10 decode probe (a device still settling at boot) is
/// not remembered forever: it is retried on later need after a bounded
/// backoff. The probe decodes a shipped clip, so it needs no encoder.
#[test]
fn a_failed_main10_decode_probe_is_retried_after_a_bounded_backoff() {
    use std::time::Duration;
    assert_eq!(super::main10_probe_backoff(1), Duration::from_secs(30));
    assert_eq!(super::main10_probe_backoff(2), Duration::from_secs(60));
    assert_eq!(super::main10_probe_backoff(5), Duration::from_secs(480));
    assert_eq!(super::main10_probe_backoff(40), Duration::from_secs(3600));
    // Matroska (EBML magic) carrying the two-picture Main10 clip.
    assert!(super::HEVC_MAIN10_PROBE_CLIP.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]));
    assert!(super::HEVC_MAIN10_PROBE_CLIP.len() < 16 * 1024);
}
