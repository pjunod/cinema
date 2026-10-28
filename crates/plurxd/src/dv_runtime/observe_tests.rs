use super::*;
use plurx_core::transcode::{
    dv_processing::{DvBaseMetadata, DvOperation, DvProcessingMode, DvSourceOrigin, DvTimestamp},
    DecodeBackend, VodFrameGrid, VodSourceClock,
};
use serde_json::{json, Value};

struct Fixture {
    private: PrivateWindow,
    directory: PathBuf,
    window: crate::dv_segment::SegmentWindow,
    origin: DvSourceOrigin,
    mode: DvProcessingMode,
    events: Vec<Value>,
}

impl Fixture {
    async fn new(mode: DvProcessingMode) -> Self {
        // These are retained parser fixtures, not claims of GPU execution.
        let raw: &[u8] = if mode == DvProcessingMode::Fel {
            include_bytes!("../../../plurx-core/tests/fixtures/dv-runtime/frame-000.nal")
        } else {
            include_bytes!("../../../plurx-core/tests/fixtures/dv-runtime/p8-piecewise.nal")
        };
        let directory = crate::test_tempdir().expect("owned renderer evidence");
        let path = directory.path().to_owned();
        std::fs::create_dir(path.join("rpus")).expect("RPU directory");
        std::fs::write(path.join("rpus/frame-000.nal"), raw).expect("actual retained RPU bytes");
        std::fs::write(path.join("timing.tsv"), b"0/24\t1/24\n").expect("timing");
        let origin = DvSourceOrigin::observed_common(
            DvTimestamp::new(0, 1).expect("origin"),
            VodSourceClock::ReportedOrigin(0),
        )
        .expect("held common origin");
        let fel = mode == DvProcessingMode::Fel;
        let mut pair = json!({
            "kind": if fel { "accepted_source_pair" } else { "accepted_source_base" },
            "frame":0,"pts":"0/24","duration":"1/24",
            "bl_sha256":"a".repeat(64),"el_sha256":if fel { Some("b".repeat(64)) } else { None },
            "rpu_sha256":hash(raw),"bl_width":64,"bl_height":64,
            "el_width":if fel {64} else {0},"el_height":if fel {64} else {0},
            "profile":7,"fel_contributed":fel
        });
        let mut rendered = json!({
            "kind":"rendered_frame","frame":0,"pts":"0/24","duration":"1/24",
            "rgb_sha256":"c".repeat(64),"yuv10_sha256":"d".repeat(64),
            "width":64,"height":64,"el_bound":fel,"nlq_active":fel,"render_errors":0,
            "profile":7,"fel_contributed":fel,"creative_trims_applied":false,
            "polynomial_segments":3,"mmr_segments":0
        });
        let mut events = vec![
            json!({"kind":"gpu_runtime","api_version":1,"vendor_id":1,"device_id":2,
                "driver_version":3,"device_type":1,"device_uuid":"a".repeat(32),"driver_uuid":"b".repeat(32)}),
            json!({"kind":"decoder","mode":"software","reason":"no-decode-device"}),
            // A 1/24000 source tick is finer than half of a 24fps output
            // frame. A whole-frame source tick cannot prove an unambiguous
            // source/output mapping and is deliberately rejected.
            json!({"kind":"coded_window_input","pts_ticks":0,"duration_ticks":1000,
                "packet_sha256":"e".repeat(64),"rpu_sha256":hash(raw)}),
            json!({"kind":"window_complete","requested_start":"0/24","requested_end":"1/24",
                "source_origin":"0/1","origin_provenance":"observed_held_format_and_selected_video_probe",
                "frames":1,"preroll_pairs":0,"paired_pictures":1,"boundary_pts_ticks":1000,
                "boundary_observed":true,"source_eof_observed":false,"first_emitted_pts_ticks":0,
                "last_emitted_end_ticks":1000,"source_time_base":"1/24000","source_bytes_read":4096,
                "membership":"frame_pts_in_half_open_window_durations_unclipped"}),
            json!({"kind":"segment_complete","frames":1,"output_policy":"bt2020-pq-master-clip"}),
        ];
        if !fel {
            let metadata = DvBaseMetadata::from_raw_rpu(raw, (64, 64)).expect("actual base RPU");
            let mut operations = metadata.applied_operations;
            operations.retain(|operation| {
                !matches!(
                    operation,
                    DvOperation::Bt2020NclConversion | DvOperation::Main10Encoding
                )
            });
            pair["profile"] = metadata.profile.into();
            rendered["profile"] = metadata.profile.into();
            rendered["polynomial_segments"] = metadata.polynomial_segments.into();
            rendered["mmr_segments"] = metadata.mmr_segments.into();
            rendered["applied_operations"] = serde_json::to_value(operations).expect("operations");
            events.push(
                json!({"kind":"base_processing_complete","frames":1,"decoded_layers":1,
                "el_bound":false,"fel_contributed":false,"profile":metadata.profile}),
            );
        }
        events.extend([pair, rendered]);

        // Obtain a genuine, fully reaped producer registration. Observation
        // retains its identity, without retaining a child or resource permit.
        let slot = crate::prodrun::ProducerSlot::new();
        let child = tokio::process::Command::new("/bin/true")
            .spawn()
            .expect("producer");
        let job = crate::process_control::ChildJob::attach(&child).expect("child job");
        let (registration, writers) = slot.attach_registered_job_owned(child, job, 0, None).await;
        writers.settled();
        slot.request_registered_retirement(&registration)
            .await
            .expect("retirement");
        registration.wait_confirmed_reap().await;
        let private = PrivateWindow {
            encoded: Vec::new(),
            evidence: crate::dv_segment::SegmentEvidence::from_test_directory(directory)
                .expect("held evidence"),
            registration,
            display_target: None,
        };
        Self {
            private,
            directory: path,
            window: crate::dv_segment::SegmentWindow {
                start: (0, 24),
                end: (1, 24),
                max_preroll: 1,
                source_origin: Some(origin),
            },
            origin,
            mode,
            events,
        }
    }

    fn observe(&self, events: &[Value]) -> Result<ObservedWindow, String> {
        let mut trace = Vec::new();
        for event in events {
            serde_json::to_writer(&mut trace, event).expect("renderer event");
            trace.push(b'\n');
        }
        std::fs::write(self.directory.join("renderer.jsonl"), trace).expect("held trace");
        self.private.observe(
            &self.window,
            self.origin,
            3,
            7,
            VodFrameGrid::new(24, 1).expect("grid"),
            0,
            1,
            self.mode,
            (64, 64),
        )
    }
}

fn event_mut<'a>(events: &'a mut [Value], kind: &str) -> &'a mut Value {
    events
        .iter_mut()
        .find(|event| event["kind"] == kind)
        .expect("fixture event")
}

#[tokio::test]
async fn held_renderer_observation_binds_actual_rpu_source_grid_and_runtime_trace() {
    for mode in [DvProcessingMode::Fel, DvProcessingMode::BaseRpu] {
        let fixture = Fixture::new(mode).await;
        let observed = fixture
            .observe(&fixture.events)
            .expect("complete retained observation");
        assert_eq!(observed.pictures.len(), 1);
        let picture = &observed.pictures[0];
        assert_eq!(picture.key.absolute_video_index, 3);
        assert_eq!(picture.key.continuity_epoch, 7);
        assert_eq!(picture.key.display_ordinal, 0);
        assert_eq!(picture.key.pts, DvTimestamp::new(0, 24).expect("PTS"));
        assert_eq!(observed.source_tick, (1, 24000));
        assert_eq!(observed.decoder, DecodeBackend::Software);
        assert!(observed.software_frame_workers);
        assert_eq!(picture.bl_shape, (64, 64));
        assert_eq!(
            picture.el_shape,
            if mode == DvProcessingMode::Fel {
                (64, 64)
            } else {
                (0, 0)
            }
        );
        assert!(
            !picture.rpu.is_empty(),
            "metadata is backed by held raw bytes"
        );

        let mut changed = fixture.events.clone();
        event_mut(&mut changed, "coded_window_input")["packet_sha256"] = "f".repeat(64).into();
        let changed = fixture
            .observe(&changed)
            .expect("another valid packet identity");
        assert_ne!(
            observed.source_trace, changed.source_trace,
            "the actual coded packet is bound to the source trace"
        );
    }
}

#[tokio::test]
async fn held_renderer_observation_refuses_duplicate_missing_and_malformed_evidence() {
    let fixture = Fixture::new(DvProcessingMode::Fel).await;
    fixture.observe(&fixture.events).expect("valid baseline");
    for kind in [
        "gpu_runtime",
        "decoder",
        "coded_window_input",
        "window_complete",
        "segment_complete",
        "accepted_source_pair",
        "rendered_frame",
    ] {
        let mut duplicate = fixture.events.clone();
        duplicate.push(
            duplicate
                .iter()
                .find(|event| event["kind"] == kind)
                .expect("event")
                .clone(),
        );
        assert!(fixture.observe(&duplicate).is_err(), "duplicate {kind}");
        let missing = fixture
            .events
            .iter()
            .filter(|event| event["kind"] != kind)
            .cloned()
            .collect::<Vec<_>>();
        assert!(fixture.observe(&missing).is_err(), "missing {kind}");
    }
    let mut missing_kind = fixture.events.clone();
    missing_kind.push(json!({"frames":1}));
    assert!(fixture
        .observe(&missing_kind)
        .err()
        .expect("event without kind")
        .contains("no kind"));
    for (kind, field, value) in [
        ("gpu_runtime", "device_uuid", json!("not-a-uuid")),
        ("gpu_runtime", "device_type", json!(5)),
        ("window_complete", "source_origin", json!("1/24")),
        (
            "window_complete",
            "origin_provenance",
            json!("producer_default_zero"),
        ),
        ("window_complete", "frames", json!(2)),
        ("window_complete", "preroll_pairs", json!(2)),
        (
            "window_complete",
            "source_bytes_read",
            json!(128 * 1024 * 1024 + 1_u64),
        ),
        ("window_complete", "boundary_pts_ticks", json!(0)),
        (
            "segment_complete",
            "output_policy",
            json!("some-other-grade"),
        ),
        ("rendered_frame", "width", json!(128)),
        ("rendered_frame", "render_errors", json!(1)),
        ("rendered_frame", "polynomial_segments", json!(4)),
        ("coded_window_input", "duration_ticks", json!(2)),
        ("coded_window_input", "packet_sha256", json!("short")),
        ("accepted_source_pair", "rpu_sha256", json!("f".repeat(64))),
        ("accepted_source_pair", "bl_sha256", json!("short")),
    ] {
        let mut changed = fixture.events.clone();
        event_mut(&mut changed, kind)[field] = value;
        assert!(fixture.observe(&changed).is_err(), "unbound {kind}.{field}");
    }
    let mut forged = fixture.events.clone();
    for kind in ["coded_window_input", "accepted_source_pair"] {
        event_mut(&mut forged, kind)["rpu_sha256"] = "f".repeat(64).into();
    }
    assert!(
        fixture
            .observe(&forged)
            .err()
            .expect("matching forged JSON cannot replace raw evidence")
            .contains("raw RPU differs"),
        "the held RPU bytes, rather than agreeing trace declarations, authorize the observation"
    );
    let mut coarse = fixture.events.clone();
    event_mut(&mut coarse, "coded_window_input")["duration_ticks"] = 1.into();
    let complete = event_mut(&mut coarse, "window_complete");
    complete["boundary_pts_ticks"] = 1.into();
    complete["last_emitted_end_ticks"] = 1.into();
    complete["source_time_base"] = "1/24".into();
    assert!(
        fixture
            .observe(&coarse)
            .err()
            .expect("a whole-frame tick cannot disambiguate the clock mapping")
            .contains("source/output clock mapping"),
        "matching timestamps do not rescue a source clock too coarse to bind pictures"
    );
    std::fs::write(fixture.directory.join("timing.tsv"), b"1/24\t1/24\n").expect("wrong timing");
    assert!(fixture
        .observe(&fixture.events)
        .err()
        .expect("timing mismatch")
        .contains("timing"));
}

#[tokio::test]
async fn base_renderer_observation_refuses_fel_claims_and_operations_not_in_raw_rpu() {
    let fixture = Fixture::new(DvProcessingMode::BaseRpu).await;
    fixture
        .observe(&fixture.events)
        .expect("valid base observation");
    for (kind, field, value) in [
        ("base_processing_complete", "decoded_layers", json!(2)),
        ("base_processing_complete", "fel_contributed", json!(true)),
        ("accepted_source_base", "profile", json!(5)),
        ("accepted_source_base", "el_width", json!(32)),
        ("rendered_frame", "creative_trims_applied", json!(true)),
        ("rendered_frame", "applied_operations", json!([])),
        ("rendered_frame", "mmr_segments", json!(999)),
    ] {
        let mut changed = fixture.events.clone();
        event_mut(&mut changed, kind)[field] = value;
        assert!(
            fixture.observe(&changed).is_err(),
            "unsupported base claim {kind}.{field}"
        );
    }
}
