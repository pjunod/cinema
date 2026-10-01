
    #[tokio::test]
    async fn the_published_identity_reaches_the_plan_and_renames_the_artifact() {
        use plurx_core::transcode::{
            ArtifactQualification, HEALTH_QUALIFIED_ARTIFACT_NAMESPACE,
            UNQUALIFIED_ARTIFACT_NAMESPACE,
        };

        let (unqualified, mgr, _work, _cache) =
            plan_under(ArtifactQualification::Unqualified).await;
        assert_eq!(
            unqualified.artifact_namespace(),
            UNQUALIFIED_ARTIFACT_NAMESPACE
        );
        assert!(!unqualified.enforces_receipt());

        let (qualified, other, _work2, _cache2) =
            plan_under(ArtifactQualification::HealthQualified).await;
        assert_eq!(
            qualified.artifact_namespace(),
            HEALTH_QUALIFIED_ARTIFACT_NAMESPACE
        );
        assert!(qualified.enforces_receipt());

        // Two identities, two artifact names: nothing produced under an
        // enforced receipt contract can land on a key an unenforced production
        // could also compute.
        let digest = mgr.digest().expect("cache configured");
        let other_digest = other.digest().expect("cache configured");
        assert_ne!(
            mgr.effective_recipe(&digest, &unqualified, false).hash(),
            other
                .effective_recipe(&other_digest, &qualified, false)
                .hash()
        );
    }

    #[tokio::test]
    async fn the_unqualified_identity_asks_a_generation_for_nothing() {
        use plurx_core::transcode::ArtifactQualification;

        let (plan, _mgr, _work, _cache) = plan_under(ArtifactQualification::Unqualified).await;
        // Every shape, including the ones the qualified identity refuses.
        assert!(generation_permits_reuse(&plan, None));
        assert!(generation_permits_reuse(
            &plan,
            Some(&generation_manifest_with(None))
        ));
        assert!(generation_permits_reuse(
            &plan,
            Some(&generation_manifest_with(Some(health_receipt(
                crate::decoder_health::Qualification::Rejected
            ))))
        ));
    }

    #[tokio::test]
    async fn the_qualified_identity_keeps_only_what_a_written_receipt_permits() {
        use plurx_core::transcode::ArtifactQualification;

        let (plan, _mgr, _work, _cache) = plan_under(ArtifactQualification::HealthQualified).await;

        // No manifest at all. This is the case the paths that publish none —
        // speculative warming, offline preparation — land in, and it has to be
        // a refusal: a receipt this process never wrote down cannot certify
        // the bytes to whatever reads them next.
        assert!(!generation_permits_reuse(&plan, None));

        // A manifest that carries no receipt. Every generation published
        // before receipts existed looks like this, which is exactly why the
        // qualified identity is a separate key space rather than a flag.
        assert!(!generation_permits_reuse(
            &plan,
            Some(&generation_manifest_with(None))
        ));

        for refused in [
            crate::decoder_health::Qualification::Rejected,
            crate::decoder_health::Qualification::Unqualified,
        ] {
            assert!(
                !generation_permits_reuse(
                    &plan,
                    Some(&generation_manifest_with(Some(health_receipt(refused))))
                ),
                "{refused:?} must not be kept"
            );
        }

        assert!(generation_permits_reuse(
            &plan,
            Some(&generation_manifest_with(Some(health_receipt(
                crate::decoder_health::Qualification::Qualified
            ))))
        ));
    }

    #[tokio::test]
    async fn a_receipt_from_a_version_this_build_does_not_know_is_not_permission() {
        use plurx_core::transcode::ArtifactQualification;

        let (plan, _mgr, _work, _cache) = plan_under(ArtifactQualification::HealthQualified).await;
        let mut future = health_receipt(crate::decoder_health::Qualification::Qualified);
        future.receipt_version = crate::decoder_health::PRODUCER_HEALTH_RECEIPT_VERSION + 1;
        assert!(!generation_permits_reuse(
            &plan,
            Some(&generation_manifest_with(Some(future)))
        ));
    }

    /// The retention rule, driven through a real production.
    ///
    /// Every other test of it calls the predicate. This one runs the producer:
    /// a real ffmpeg makes a real generation, and the qualified identity
    /// refuses to keep it — now for the reason that will still be true on a
    /// deployed node. The path publishes a manifest, the manifest carries the
    /// receipt, and the receipt does not permit reuse, because no diagnostic
    /// contract covers this host's build so nothing it printed is evidence.
    /// The two things that must be true afterwards are checked: nothing is
    /// left claimed, and nothing is left served.
    ///
    /// The claim half is the one worth having. A refusal that quarantined the
    /// bytes and left the `complete = 0` row behind would deadlock the recipe:
    /// the next request cannot claim it, finds no staging tree to resume, and
    /// stands down — while the reaper refuses to collect a claim whose package
    /// is still queued.
    #[tokio::test]
    async fn a_refused_generation_is_neither_kept_nor_left_claimed() {
        super::require_ffmpeg();
        use plurx_core::store::SqliteStore;
        use plurx_core::transcode::ArtifactQualification;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let media = crate::test_tempdir().expect("media");
        let source = media.path().join("Refused.mkv");
        write_real_video(&source, 2);
        let file_id = seed_real_file(&store, &source).await;
        let (mgr, _work, cache) = cached_manager(&store);
        mgr.test_publish_artifact_qualification(ArtifactQualification::HealthQualified);
        let mgr = Arc::new(mgr);
        let file = store.get_file(file_id).await.expect("get").expect("file");

        let hash = recipe_hash_for(&mgr, &file, 240).await;
        assert!(
            mgr.produce(&file, 240, Instant::now() + Duration::from_secs(120))
                .await
                .expect("produce")
                .is_none(),
            "a generation with no written receipt must not be reported as produced"
        );

        assert!(
            store
                .cache_hit(&hash, NODE)
                .await
                .expect("cache lookup")
                .is_none(),
            "a refused generation must not be reusable"
        );
        assert!(
            !cache.path().join(&hash[..2]).join(&hash).exists(),
            "a refused generation must not be left on disk"
        );
        // And the claim is released, so the next request for this recipe can
        // take it rather than standing down against a row nothing will reap.
        assert!(
            mgr.store
                .claim_cache_entry(
                    &hash,
                    file.id,
                    CACHE_RECIPE_VERSION,
                    NODE,
                    &format!("{}/{hash}", &hash[..2]),
                )
                .await
                .expect("claim after a refusal"),
            "a refused production must not leave its claim behind"
        );

        // The same producer under the identity that asks for nothing keeps its
        // work, so the assertion above is about the receipt contract and not
        // about the fixture failing to encode.
        let (lenient, _work2, lenient_cache) = cached_manager(&store);
        let lenient = Arc::new(lenient);
        let lenient_hash = recipe_hash_for(&lenient, &file, 240).await;
        assert!(
            lenient
                .produce(&file, 240, Instant::now() + Duration::from_secs(120))
                .await
                .expect("produce")
                .is_some(),
            "the unqualified identity keeps what it made"
        );
        assert!(lenient_cache
            .path()
            .join(&lenient_hash[..2])
            .join(&lenient_hash)
            .exists());
    }

    /// A measured decoder name reaches the plan only under the qualified
    /// identity, and it reaches it as the *implementation*, not the family.
    ///
    /// Both halves matter. Naming a decoder changes an artifact's name, so
    /// doing it unconditionally would rotate every deployed node's cache for a
    /// value nothing there enforces. Not doing it under the qualified identity
    /// would be worse: a diagnostic contract is qualified against a named
    /// decoder, so a plan that names none can never be matched to one, and an
    /// attempt nothing can classify can never be certified — the qualified
    /// namespace would be a key space in which nothing is ever qualified.
    #[tokio::test]
    async fn a_measured_decoder_names_the_plan_only_where_it_is_enforced() {
        use plurx_core::store::SqliteStore;
        use plurx_core::transcode::decoder_inventory::MeasuredDecoders;
        use plurx_core::transcode::ArtifactQualification;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let file = store.get_file(file_id).await.expect("get").expect("file");

        // The case the inventory exists for, stated as a fixture: the family
        // is one name and the decoder that runs is another.
        let measured = MeasuredDecoders::from_measured(&[(
            "hevc",
            plurx_core::transcode::DecodeBackend::Software,
            "hevc_special",
        )]);
        let build = |qualification| {
            let (mgr, work, cache) = cached_manager(&store);
            let mgr = mgr
                .with_decoders(vec!["hevc".to_owned()])
                .with_measured_decoders(measured.clone());
            mgr.test_publish_artifact_qualification(qualification);
            (mgr, work, cache)
        };

        let (unqualified, _w1, _c1) = build(ArtifactQualification::Unqualified);
        let (qualified, _w2, _c2) = build(ArtifactQualification::HealthQualified);
        let opts = unqualified.options_for_tone_map(
            Encoder::Software,
            &file,
            720,
            0.0,
            None,
            None,
            None,
            ToneMap::Zscale,
            OutputGrade::Sdr,
        );

        let plain = unqualified
            .resolve_movie_plan(&file, &opts, Encoder::Software)
            .await
            .expect("unqualified plan");
        assert_eq!(
            plain.decode().software_decoder(),
            None,
            "an unqualified plan must name no decoder, so no deployed cache moves"
        );

        let named = qualified
            .resolve_movie_plan(&file, &opts, Encoder::Software)
            .await
            .expect("qualified plan");
        assert_eq!(
            named.decode().software_decoder(),
            Some("hevc_special"),
            "a qualified plan names the decoder that will actually run"
        );

        // And the name is what moved the artifact, not merely the identity
        // the two plans were resolved under: a third plan, qualified with no
        // measurement, names nothing and lands somewhere else again.
        let (unmeasured, _w3, _c3) = {
            let (mgr, work, cache) = cached_manager(&store);
            mgr.test_publish_artifact_qualification(ArtifactQualification::HealthQualified);
            (mgr, work, cache)
        };
        let anonymous = unmeasured
            .resolve_movie_plan(&file, &opts, Encoder::Software)
            .await
            .expect("qualified plan with nothing measured");
        assert_eq!(anonymous.decode().software_decoder(), None);
        assert_ne!(
            anonymous.plan_digest(),
            named.plan_digest(),
            "the measured decoder is what names the artifact, not the identity alone"
        );
        assert_ne!(plain.plan_digest(), named.plan_digest());
    }

    #[tokio::test]
    async fn an_unmeasured_codec_leaves_even_a_qualified_plan_unnamed() {
        use plurx_core::store::SqliteStore;
        use plurx_core::transcode::decoder_inventory::MeasuredDecoders;
        use plurx_core::transcode::ArtifactQualification;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let (mgr, _work, _cache) = cached_manager(&store);
        // Measured, but for a different codec than this file's.
        let mgr = mgr.with_measured_decoders(MeasuredDecoders::from_measured(&[(
            "av1",
            plurx_core::transcode::DecodeBackend::Software,
            "libdav1d",
        )]));
        mgr.test_publish_artifact_qualification(ArtifactQualification::HealthQualified);
        let opts = mgr.options_for_tone_map(
            Encoder::Software,
            &file,
            720,
            0.0,
            None,
            None,
            None,
            ToneMap::Zscale,
            OutputGrade::Sdr,
        );
        let plan = mgr
            .resolve_movie_plan(&file, &opts, Encoder::Software)
            .await
            .expect("qualified plan");
        assert_eq!(
            plan.decode().software_decoder(),
            None,
            "an unmeasured codec must stay unnamed rather than fall back to its family"
        );
    }

    /// A manifest is published off the queue too, once the identity enforces a
    /// receipt — and only then.
    ///
    /// Without it, speculative warming and offline preparation settle a
    /// perfectly clean receipt and then have the generation refused for having
    /// written it nowhere, because the retention rule reads the manifest. With
    /// it under the *unqualified* identity, every deployed speculative and
    /// offline row would newly gain a digest, and with it shared-cluster
    /// fanout, cluster placement offers, integrity scrubbing, and offline
    /// segment serving that fails hard where it used to degrade. Four
    /// behaviour changes to live rows in exchange for a receipt nothing there
    /// reads.
    #[tokio::test]
    async fn a_manifest_is_written_off_the_queue_only_where_a_receipt_is_enforced() {
        super::require_ffmpeg();
        use plurx_core::store::SqliteStore;
        use plurx_core::transcode::ArtifactQualification;

        for (qualification, wants_manifest) in [
            (ArtifactQualification::Unqualified, false),
            (ArtifactQualification::HealthQualified, true),
        ] {
            let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
            let media = crate::test_tempdir().expect("media");
            let source = media.path().join("Manifested.mkv");
            write_real_video(&source, 2);
            let file_id = seed_real_file(&store, &source).await;
            let (mgr, _work, cache) = cached_manager(&store);
            mgr.test_publish_artifact_qualification(qualification);
            let mgr = Arc::new(mgr);
            let file = store.get_file(file_id).await.expect("get").expect("file");
            let hash = recipe_hash_for(&mgr, &file, 240).await;

            let produced = mgr
                .produce(&file, 240, Instant::now() + Duration::from_secs(120))
                .await
                .expect("produce");

            if wants_manifest {
                // A manifest *was* written, and the counter is the only thing
                // that can say so: the refusal below quarantines the
                // generation, taking the manifest with it, so every on-disk
                // observation afterwards is identical to the one a build
                // without this milestone would leave. Delete
                // `|| plan.enforces_receipt()` from the publication condition
                // and this assertion is what fails.
                assert_eq!(
                    mgr.test_manifests_published(),
                    1,
                    "the qualified identity publishes a manifest off the queue"
                );
                // And it is still refused, for the reason a deployed node will
                // give: no diagnostic contract covers this host's build, so
                // nothing FFmpeg printed is evidence and the receipt does not
                // permit reuse. Which branch of `generation_permits_reuse`
                // refuses has changed — from manifest-absent to
                // receipt-refuses, a branch that off the queue was previously
                // unreachable — and the outcome cannot show that, so the
                // counter above carries the change and this carries the
                // invariant it must not break.
                assert!(produced.is_none(), "an unqualified receipt is not kept");
                assert!(!cache.path().join(&hash[..2]).join(&hash).exists());
            } else {
                assert_eq!(
                    mgr.test_manifests_published(),
                    0,
                    "the unqualified identity publishes none off the queue"
                );
                let produced = produced.expect("the unqualified identity keeps its work");
                let dir = cache
                    .path()
                    .join(&produced.recipe[..2])
                    .join(&produced.recipe);
                assert!(dir.exists(), "the generation is on disk");
                assert!(
                    !dir.join(plurx_core::transcode::manifest::MANIFEST_FILE)
                        .exists(),
                    "and carries no manifest, so no deployed row gains one"
                );
                let row = store
                    .cache_hit(&produced.recipe, NODE)
                    .await
                    .expect("cache lookup")
                    .expect("a completed row");
                assert_eq!(
                    row.manifest_digest, None,
                    "an unqualified row records no digest, so nothing that keys on one changes"
                );
            }
        }
    }

    /// A request is not a switch: the node intersects it with what it
    /// measured about itself.
    ///
    /// Honouring a request this node cannot serve is not a degraded mode. The
    /// identity names a content-addressed key space, so the node would rotate
    /// its whole transcode cache to keys whose every generation is then
    /// refused — re-encoding each title once per request, forever, while every
    /// counter reads healthy. Each of the three prerequisites is checked here
    /// on its own, because a refusal an operator cannot act on is the same as
    /// no refusal at all.
    #[test]
    fn a_request_is_enabled_while_coverage_remains_advisory() {
        use plurx_core::transcode::decoder_inventory::MeasuredDecoders;
        use plurx_core::transcode::ArtifactQualification;

        let contract =
            |input_codec: &str, decoder: &str| crate::decoder_health::DiagnosticContract {
                id: format!("fixture-{input_codec}-{decoder}"),
                host: "workstation".to_owned(),
                ffmpeg_version: "ffmpeg version 9.0.1".to_owned(),
                binary_sha256: "b".repeat(64),
                buildconf_sha256: "c".repeat(64),
                stderr_mode: crate::decoder_health::QUALIFIED_STDERR_MODE.to_owned(),
                input_codec: input_codec.to_owned(),
                decoder: decoder.to_owned(),
                decode_backend: plurx_core::transcode::DecodeBackend::Software
                    .name()
                    .to_owned(),
                require_context_addresses: true,
                primary_message: "Decoding error:".to_owned(),
                subordinate_message: None,
                attributes_every_failure: false,
                error_detail: "corrupt input packet".to_owned(),
                fixture: "f".to_owned(),
                fixture_sha256: "d".repeat(64),
                scope: "workstation".to_owned(),
                backend_fault_detail: None,
            };
        let build = || crate::decoder_health::MeasuredBuild {
            ffmpeg_version: "ffmpeg version 9.0.1".to_owned(),
            binary_sha256: "b".repeat(64),
            buildconf_sha256: "c".repeat(64),
        };
        let covered = crate::decoder_health::DiagnosticPolicy::new(
            Some(build()),
            vec![contract("h264", "h264")],
        );
        let measured = MeasuredDecoders::from_measured(&[
            (
                "h264",
                plurx_core::transcode::DecodeBackend::Software,
                "h264",
            ),
            (
                "hevc",
                plurx_core::transcode::DecodeBackend::Software,
                "hevc",
            ),
        ]);
        let selectable = vec![
            (
                "h264".to_owned(),
                plurx_core::transcode::DecodeBackend::Software,
            ),
            (
                "hevc".to_owned(),
                plurx_core::transcode::DecodeBackend::Software,
            ),
        ];

        // Nobody asked. Every deployed node, and the reason it is a separate
        // refusal rather than an absent one: the surface says so out loud.
        let idle = artifact_qualification_readiness(false, &covered, &measured, &selectable);
        assert_eq!(idle.effective, ArtifactQualification::Unqualified);
        assert_eq!(idle.refusal, Some(QualificationRefusal::NotRequested));
        assert!(
            idle.eligible(),
            "a node that could honour a request reports so before one is made, \
             so an operator can see the answer before paying for it"
        );

        // Asked, and enabled. Only the covered pair is listed: hevc was
        // measured and is not covered, so the surface must call the node
        // partially covered rather than imply every plan is verified.
        let asked = artifact_qualification_readiness(true, &covered, &measured, &selectable);
        assert_eq!(asked.effective, ArtifactQualification::Unqualified);
        assert_eq!(
            asked.refusal,
            Some(QualificationRefusal::IncompleteCoverage)
        );
        assert_eq!(
            asked.covered_decoders,
            vec![(
                "h264".to_owned(),
                plurx_core::transcode::DecodeBackend::Software,
                "h264".to_owned(),
            )]
        );
        assert_eq!(asked.measured_decoders.len(), 2);

        // No build measured: nothing this node prints can be matched to a
        // contract, so nothing it prints is evidence.
        let unmeasured =
            crate::decoder_health::DiagnosticPolicy::new(None, vec![contract("h264", "h264")]);
        let readiness = artifact_qualification_readiness(true, &unmeasured, &measured, &selectable);
        assert_eq!(
            readiness.effective,
            ArtifactQualification::Unqualified,
            "the legacy whole-node projection stays conservative without disabling the request"
        );
        assert_eq!(
            readiness.refusal,
            Some(QualificationRefusal::BuildUnmeasured)
        );
        assert!(
            !readiness.eligible() && readiness.covered_decoders.is_empty(),
            "coverage is about a build, so a node that identified none has none"
        );

        // No decoder named. A contract is qualified against a *named* decoder,
        // so a plan that names none can never be matched to one.
        let readiness = artifact_qualification_readiness(
            true,
            &covered,
            &MeasuredDecoders::default(),
            &selectable,
        );
        assert_eq!(readiness.effective, ArtifactQualification::Unqualified);
        assert_eq!(
            readiness.refusal,
            Some(QualificationRefusal::NoDecoderMeasured)
        );

        // Build measured, decoders named, and no contract covers it. This is
        // the fleet's state today and the only refusal that takes real work to
        // leave: it needs a capture from this build.
        let uncovered = crate::decoder_health::DiagnosticPolicy::new(Some(build()), Vec::new());
        let readiness = artifact_qualification_readiness(true, &uncovered, &measured, &selectable);
        assert_eq!(readiness.effective, ArtifactQualification::Unqualified);
        assert_eq!(
            readiness.refusal,
            Some(QualificationRefusal::NoContractCoversThisBuild)
        );
        assert!(!readiness.eligible());
        assert_eq!(
            readiness.measured_decoders.len(),
            2,
            "what it did measure is still reported; a refusal that hides the \
             evidence leaves an operator with nothing to fix"
        );

        // A contract for a decoder this build does not select covers nothing.
        // The measurement is what the plan will name, so a contract qualified
        // against `libdav1d` says nothing about a node whose probe named
        // `av1` — and the family name is exactly the substitution M3d exists
        // to refuse.
        let wrong_decoder = crate::decoder_health::DiagnosticPolicy::new(
            Some(build()),
            vec![contract("av1", "libdav1d")],
        );
        let readiness = artifact_qualification_readiness(
            true,
            &wrong_decoder,
            &MeasuredDecoders::from_measured(&[(
                "av1",
                plurx_core::transcode::DecodeBackend::Software,
                "av1",
            )]),
            &[(
                "av1".to_owned(),
                plurx_core::transcode::DecodeBackend::Software,
            )],
        );
        assert_eq!(
            readiness.refusal,
            Some(QualificationRefusal::NoContractCoversThisBuild)
        );
    }

    #[test]
    fn readiness_matches_each_measurement_on_its_own_backend() {
        use plurx_core::transcode::decoder_inventory::MeasuredDecoders;
        use plurx_core::transcode::DecodeBackend;

        let contract = decode_contract_fixture(DecodeBackend::VideoToolbox);
        let policy = crate::decoder_health::DiagnosticPolicy::new(
            Some(crate::decoder_health::MeasuredBuild {
                ffmpeg_version: contract.ffmpeg_version.clone(),
                binary_sha256: contract.binary_sha256.clone(),
                buildconf_sha256: contract.buildconf_sha256.clone(),
            }),
            vec![contract],
        );
        let measured = MeasuredDecoders::from_measured(&[
            ("h264", DecodeBackend::Software, "h264"),
            ("h264", DecodeBackend::VideoToolbox, "h264"),
        ]);

        let readiness = artifact_qualification_readiness(
            true,
            &policy,
            &measured,
            &[
                ("h264".to_owned(), DecodeBackend::Software),
                ("h264".to_owned(), DecodeBackend::VideoToolbox),
            ],
        );
        assert_eq!(
            readiness.covered_decoders,
            vec![(
                "h264".to_owned(),
                DecodeBackend::VideoToolbox,
                "h264".to_owned(),
            )],
            "the identical software decoder name does not inherit the hardware contract"
        );
        assert_eq!(readiness.measured_decoders.len(), 2);
        assert_eq!(
            readiness.refusal,
            Some(QualificationRefusal::IncompleteCoverage)
        );
    }

    #[tokio::test]
    async fn an_enabled_partial_policy_qualifies_only_its_exact_decode_path() {
        use plurx_core::store::keys::DECODER_HEALTH_QUALIFIED_ARTIFACTS;
        use plurx_core::store::SqliteStore;
        use plurx_core::transcode::decoder_inventory::MeasuredDecoders;
        use plurx_core::transcode::DecodeBackend;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let mut contract = decode_contract_fixture(DecodeBackend::VideoToolbox);
        contract.input_codec = "hevc".to_owned();
        contract.decoder = "hevc".to_owned();
        let policy = crate::decoder_health::DiagnosticPolicy::new(
            Some(crate::decoder_health::MeasuredBuild {
                ffmpeg_version: contract.ffmpeg_version.clone(),
                binary_sha256: contract.binary_sha256.clone(),
                buildconf_sha256: contract.buildconf_sha256.clone(),
            }),
            vec![contract],
        );
        let measured = MeasuredDecoders::from_measured(&[
            ("hevc", DecodeBackend::Software, "hevc"),
            ("hevc", DecodeBackend::VideoToolbox, "hevc"),
        ]);
        let (mgr, _work, _cache) = cached_manager(&store);
        let mgr = mgr
            .with_measured_decoders(measured)
            .with_diagnostic_policy(Arc::new(policy));
        store
            .put_setting(DECODER_HEALTH_QUALIFIED_ARTIFACTS, "1")
            .await
            .expect("enable requested policy");
        let published = mgr.publish_artifact_qualification().await;
        assert_eq!(
            published.refusal,
            Some(QualificationRefusal::IncompleteCoverage),
            "hardware-only coverage is visible as unsafe for automatic recovery"
        );

        let hardware_options = mgr.options_for_tone_map(
            Encoder::Software,
            &file,
            720,
            0.0,
            None,
            None,
            None,
            ToneMap::Zscale,
            OutputGrade::Sdr,
        );
        let hardware = mgr
            .resolve_movie_plan(&file, &hardware_options, Encoder::VideoToolbox)
            .await
            .expect("hardware plan");
        assert_eq!(hardware.decode().backend(), DecodeBackend::VideoToolbox);
        assert!(hardware.enforces_receipt());

        let software_options = mgr.options_for_tone_map(
            Encoder::Software,
            &file,
            720,
            0.0,
            None,
            None,
            None,
            ToneMap::Zscale,
            OutputGrade::Sdr,
        );
        let software = mgr
            .resolve_movie_plan(&file, &software_options, Encoder::Software)
            .await
            .expect("software plan");
        assert_eq!(software.decode().backend(), DecodeBackend::Software);
        assert!(
            !software.enforces_receipt(),
            "one hardware contract must not rotate an uncovered software path into a namespace it cannot certify"
        );
    }

    /// A successful probe is evidence about that path, not evidence that an
    /// advertised path which failed to produce a probe no longer exists.
    #[tokio::test]
    async fn legacy_whole_node_state_includes_plan_capable_unmeasured_codecs() {
        use plurx_core::domain::{ItemKind, NewItem, ProbeResult};
        use plurx_core::store::keys::DECODER_HEALTH_QUALIFIED_ARTIFACTS;
        use plurx_core::store::SqliteStore;
        use plurx_core::transcode::decoder_inventory::MeasuredDecoders;
        use plurx_core::transcode::{ArtifactQualification, DecodeBackend};

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let h264_id = seed_file_with_probe_at(
            &store,
            "/media/H264.mkv",
            ProbeResult {
                duration_ms: Some(60_000),
                container: Some("mkv".into()),
                video_codec: Some("h264".into()),
                width: Some(1920),
                height: Some(1080),
                ..Default::default()
            },
        )
        .await;
        let library_id = store
            .list_libraries()
            .await
            .expect("libraries")
            .into_iter()
            .next()
            .expect("seed library")
            .id;
        let hevc_item = store
            .insert_item(&NewItem {
                library_id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "HEVC".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("hevc item");
        let hevc_id = store
            .upsert_file(
                hevc_item,
                "/media/HEVC.mkv",
                1,
                1,
                &ProbeResult {
                    duration_ms: Some(60_000),
                    container: Some("mkv".into()),
                    video_codec: Some("hevc".into()),
                    width: Some(3840),
                    height: Some(2160),
                    ..Default::default()
                },
            )
            .await
            .expect("hevc file");
        let h264 = store.get_file(h264_id).await.expect("get").expect("h264");
        let hevc = store.get_file(hevc_id).await.expect("get").expect("hevc");
        let contract = decode_contract_fixture(DecodeBackend::Software);
        let policy = crate::decoder_health::DiagnosticPolicy::new(
            Some(crate::decoder_health::MeasuredBuild {
                ffmpeg_version: contract.ffmpeg_version.clone(),
                binary_sha256: contract.binary_sha256.clone(),
                buildconf_sha256: contract.buildconf_sha256.clone(),
            }),
            vec![contract],
        );
        let (mgr, _work, _cache) = cached_manager(&store);
        let mgr = mgr
            .with_decoders(vec!["h264".to_owned(), "hevc".to_owned()])
            .with_measured_decoders(MeasuredDecoders::from_measured(&[(
                "h264",
                DecodeBackend::Software,
                "h264",
            )]))
            .with_diagnostic_policy(Arc::new(policy));
        store
            .put_setting(DECODER_HEALTH_QUALIFIED_ARTIFACTS, "1")
            .await
            .expect("enable request");
        let published = mgr.publish_artifact_qualification().await;
        assert!(published.requested, "prerequisites do not gate the setting");
        assert_eq!(
            published.refusal,
            Some(QualificationRefusal::IncompleteCoverage)
        );
        assert_eq!(
            published.effective,
            ArtifactQualification::Unqualified,
            "legacy whole-node state cannot ignore the advertised unmeasured codec"
        );
        let explanation = published
            .refusal
            .expect("selectable HEVC is not measured")
            .explanation();
        assert!(
            explanation.contains("selectable decode paths"),
            "the guidance must name the completeness denominator: {explanation}"
        );
        assert!(
            explanation.contains("missing measurements or contracts"),
            "the guidance must distinguish an unmeasured selectable path from a missing contract: {explanation}"
        );
        assert!(
            !explanation.contains("Only some measured decode paths"),
            "every measured path in this case is covered: {explanation}"
        );

        let options = |file: &plurx_core::domain::MediaFile| {
            mgr.options_for_tone_map(
                Encoder::Software,
                file,
                720,
                0.0,
                None,
                None,
                None,
                ToneMap::Zscale,
                OutputGrade::Sdr,
            )
        };
        let h264_plan = mgr
            .resolve_movie_plan(&h264, &options(&h264), Encoder::Software)
            .await
            .expect("h264 plan");
        let hevc_plan = mgr
            .resolve_movie_plan(&hevc, &options(&hevc), Encoder::Software)
            .await
            .expect("hevc plan");
        assert!(
            h264_plan.enforces_receipt(),
            "the covered exact path still qualifies"
        );
        assert!(
            !hevc_plan.enforces_receipt(),
            "the plan-capable but unmeasured codec keeps its stable identity"
        );
    }

    /// The publisher writes the requested policy, and a covered path's
    /// identity moves its cache key.
    ///
    /// The free-function test above proves the rule. This proves the two
    /// things only the manager can: that the published value is what planning
    /// actually reads, and that it is part of the recipe hash. Without the
    /// second, every claim in this milestone about renaming a node's cache is
    /// an assertion about code nobody ran.
    #[tokio::test]
    async fn the_published_identity_is_the_request_this_node_can_honour_and_moves_its_cache_keys() {
        use plurx_core::store::keys::DECODER_HEALTH_QUALIFIED_ARTIFACTS;
        use plurx_core::store::SqliteStore;
        use plurx_core::transcode::decoder_inventory::MeasuredDecoders;
        use plurx_core::transcode::ArtifactQualification;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let (mgr, _work, _cache) = cached_manager(&store);
        // A contract covering exactly what this manager measured, on the build
        // it was told it is running. Nothing else can produce a qualified
        // node, which is why the policy is handed to the manager rather than
        // reached for.
        let contract = crate::decoder_health::DiagnosticContract {
            id: "fixture-hevc".to_owned(),
            host: "workstation".to_owned(),
            ffmpeg_version: "ffmpeg version 9.0.1".to_owned(),
            binary_sha256: "b".repeat(64),
            buildconf_sha256: "c".repeat(64),
            stderr_mode: crate::decoder_health::QUALIFIED_STDERR_MODE.to_owned(),
            input_codec: "hevc".to_owned(),
            decoder: "hevc".to_owned(),
            decode_backend: plurx_core::transcode::DecodeBackend::Software
                .name()
                .to_owned(),
            require_context_addresses: true,
            primary_message: "Decoding error:".to_owned(),
            subordinate_message: None,
            attributes_every_failure: false,
            error_detail: "corrupt input packet".to_owned(),
            fixture: "f".to_owned(),
            fixture_sha256: "d".repeat(64),
            scope: "workstation".to_owned(),
            backend_fault_detail: None,
        };
        let mgr = mgr
            .with_decoders(vec!["hevc".to_owned()])
            .with_measured_decoders(MeasuredDecoders::from_measured(&[(
                "hevc",
                plurx_core::transcode::DecodeBackend::Software,
                "hevc",
            )]))
            .with_diagnostic_policy(Arc::new(crate::decoder_health::DiagnosticPolicy::new(
                Some(crate::decoder_health::MeasuredBuild {
                    ffmpeg_version: "ffmpeg version 9.0.1".to_owned(),
                    binary_sha256: "b".repeat(64),
                    buildconf_sha256: "c".repeat(64),
                }),
                vec![contract],
            )));
        let file = store.get_file(file_id).await.expect("get").expect("file");

        // Nothing asked, so nothing moves — and the published answer says so
        // rather than being an absence.
        let published = mgr.publish_artifact_qualification().await;
        assert_eq!(published.effective, ArtifactQualification::Unqualified);
        assert_eq!(published.refusal, Some(QualificationRefusal::NotRequested));
        assert!(published.eligible(), "this node could honour a request");
        assert_eq!(
            mgr.artifact_qualification(),
            ArtifactQualification::Unqualified,
            "planning reads the published value"
        );
        let unqualified_hash = recipe_hash_for(&mgr, &file, 1080).await;

        // Asked, and honoured.
        store
            .put_setting(DECODER_HEALTH_QUALIFIED_ARTIFACTS, "1")
            .await
            .expect("request");
        let published = mgr.publish_artifact_qualification().await;
        assert_eq!(published.effective, ArtifactQualification::HealthQualified);
        assert_eq!(published.refusal, None);
        assert!(published.requested);
        assert_eq!(
            mgr.artifact_qualification(),
            ArtifactQualification::HealthQualified
        );
        assert_eq!(
            mgr.published_artifact_qualification(),
            published,
            "the surface reports what was published, not a recomputation"
        );
        let qualified_hash = recipe_hash_for(&mgr, &file, 1080).await;
        assert_ne!(
            unqualified_hash, qualified_hash,
            "the identity is part of the cache key: turning this on renames \
             every transcode the node caches, which is the cost the settings \
             card states and the reason this is not applied to a live node"
        );

        // And back. The rename is paid a second time, which is worth knowing
        // before turning it off as casually as it was turned on.
        store
            .put_setting(DECODER_HEALTH_QUALIFIED_ARTIFACTS, "0")
            .await
            .expect("withdraw");
        let published = mgr.publish_artifact_qualification().await;
        assert_eq!(published.effective, ArtifactQualification::Unqualified);
        assert_eq!(published.refusal, Some(QualificationRefusal::NotRequested));
        assert_eq!(
            recipe_hash_for(&mgr, &file, 1080).await,
            unqualified_hash,
            "withdrawing the request returns the node to its own earlier keys"
        );
    }

    /// Two contracts covering one build is a different problem from none, and
    /// the fix for one makes the other worse.
    ///
    /// `contract_for` refuses ambiguity by returning no contract at all, so
    /// without this it reads downstream as "capture one" to an operator who
    /// has captured two.
    #[test]
    fn ambiguous_coverage_is_not_reported_as_missing_coverage() {
        use plurx_core::transcode::decoder_inventory::MeasuredDecoders;

        let contract = |id: &str, codec: &str| crate::decoder_health::DiagnosticContract {
            id: id.to_owned(),
            host: "workstation".to_owned(),
            ffmpeg_version: "ffmpeg version 9.0.1".to_owned(),
            binary_sha256: "b".repeat(64),
            buildconf_sha256: "c".repeat(64),
            stderr_mode: crate::decoder_health::QUALIFIED_STDERR_MODE.to_owned(),
            input_codec: codec.to_owned(),
            decoder: codec.to_owned(),
            decode_backend: plurx_core::transcode::DecodeBackend::Software
                .name()
                .to_owned(),
            require_context_addresses: true,
            primary_message: "Decoding error:".to_owned(),
            subordinate_message: None,
            attributes_every_failure: false,
            error_detail: "corrupt input packet".to_owned(),
            fixture: "f".to_owned(),
            fixture_sha256: "d".repeat(64),
            scope: "workstation".to_owned(),
            backend_fault_detail: None,
        };
        let policy = crate::decoder_health::DiagnosticPolicy::new(
            Some(crate::decoder_health::MeasuredBuild {
                ffmpeg_version: "ffmpeg version 9.0.1".to_owned(),
                binary_sha256: "b".repeat(64),
                buildconf_sha256: "c".repeat(64),
            }),
            vec![
                contract("unique", "h264"),
                contract("first", "hevc"),
                contract("second", "hevc"),
            ],
        );
        let readiness = artifact_qualification_readiness(
            true,
            &policy,
            &MeasuredDecoders::from_measured(&[
                (
                    "h264",
                    plurx_core::transcode::DecodeBackend::Software,
                    "h264",
                ),
                (
                    "hevc",
                    plurx_core::transcode::DecodeBackend::Software,
                    "hevc",
                ),
            ]),
            &[
                (
                    "h264".to_owned(),
                    plurx_core::transcode::DecodeBackend::Software,
                ),
                (
                    "hevc".to_owned(),
                    plurx_core::transcode::DecodeBackend::Software,
                ),
            ],
        );
        assert_eq!(
            readiness.refusal,
            Some(QualificationRefusal::AmbiguousContract)
        );
        assert_eq!(readiness.covered_decoders.len(), 1);
        assert_eq!(
            readiness.covered_decoders[0].0, "h264",
            "the unique path remains visible while the mixed ambiguous path is called out"
        );
        assert!(
            readiness.eligible(),
            "the unique path remains usable; ambiguity on another path remains an advisory"
        );
        assert!(
            readiness
                .refusal
                .expect("refusal")
                .explanation()
                .contains("Remove the duplicate"),
            "the instruction must not be the one that makes it worse"
        );
    }

    /// Every refusal has a distinct name and a sentence someone can act on.
    ///
    /// A control whose only feedback is "still off" tells the person who
    /// turned it on nothing, and this one is off by default on every node.
    #[test]
    fn every_qualification_refusal_says_what_to_do_about_it() {
        let all = [
            QualificationRefusal::NotRequested,
            QualificationRefusal::BuildUnmeasured,
            QualificationRefusal::NoDecoderMeasured,
            QualificationRefusal::NoContractCoversThisBuild,
            QualificationRefusal::IncompleteCoverage,
            QualificationRefusal::AmbiguousContract,
            QualificationRefusal::SettingUnreadable,
        ];
        let names = all.map(QualificationRefusal::name);
        let mut unique = names.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), all.len(), "each refusal is distinguishable");
        for refusal in all {
            let explanation = refusal.explanation();
            assert!(explanation.ends_with('.'), "{}", refusal.name());
            assert!(
                explanation.len() > 24,
                "{} explains nothing",
                refusal.name()
            );
            assert!(
                !refusal.name().contains(' ') && refusal.name() == refusal.name().to_lowercase(),
                "a machine-readable name"
            );
        }
    }

    /// The generation id off the queue is stable across the resume passes of
    /// one production.
    ///
    /// It is the digest checkpoint's key. A value that changed per pass would
    /// throw away the checkpoint every time and rehash the whole film, which
    /// on a long title is the difference between finishing and never
    /// finishing.
    /// The anime rule's two halves, on the shape that broke each of them.
    ///
    /// `select_tracks_with_prefs` is the one place the server chooses a burn
    /// nobody asked for, and it had no HDR guard at all: a dual-audio HDR
    /// release with a default English PGS track was tone-mapped to draw a
    /// subtitle the viewer never selected and could not turn off.
    ///
    /// The fix must not take REQ-SUB-2 with it. A standard fansub or BD remux
    /// carries exactly one ASS track, and `deliverable_as_default` — correctly
    /// — says a viewer cannot see an ASS track *without* a burn. Handing that
    /// predicate to this pick would make the anime rule select Japanese audio
    /// and no subtitles at all, silently, on every such file.
    #[test]
    fn the_implicit_anime_burn_survives_ass_and_stops_at_an_hdr_delivery() {
        fn anime(codec: &str, hdr: Option<&str>) -> plurx_core::domain::MediaFile {
            plurx_core::domain::MediaFile {
                downloaded_subtitles: Vec::new(),
                id: 4_242,
                item_id: 1,
                path: "/media/Ash.Season.S01E01.mkv".into(),
                size: 1,
                mtime: 1,
                duration_ms: Some(1_440_000),
                container: Some("mkv".into()),
                video_codec: Some("hevc".into()),
                video_codec_tag: None,
                field_order: None,
                video_profile: None,
                width: Some(1920),
                height: Some(1080),
                bit_depth: Some(if hdr.is_some() { 10 } else { 8 }),
                hdr: hdr.map(str::to_owned),
                hdr_format: hdr.map(str::to_owned),
                max_cll: None,
                max_fall: None,
                mastering_max_luminance: None,
                luminance_source: None,
                bitrate: Some(8_000_000),
                audio_streams: vec![
                    plurx_core::domain::AudioStream {
                        index: 0,
                        codec: "aac".into(),
                        channels: Some(2),
                        language: Some("eng".into()),
                        default: true,
                        ..Default::default()
                    },
                    plurx_core::domain::AudioStream {
                        index: 1,
                        codec: "aac".into(),
                        channels: Some(2),
                        language: Some("jpn".into()),
                        ..Default::default()
                    },
                ],
                subtitle_streams: vec![plurx_core::domain::SubtitleStream {
                    index: 0,
                    codec: codec.into(),
                    language: Some("eng".into()),
                    default: true,
                    ..Default::default()
                }],
                scanned_at: 0,
                audio_offset_ms: 0,
                probed: true,
                dolby_vision: Default::default(),
            }
        }
        let prefs = plurx_core::tracks::LangPrefs::default();

        // The regression the predicate would have caused: one ASS track, SDR.
        let ass = anime("ass", None);
        let picked = TranscodeManager::select_tracks_with_prefs(&ass, None, None, &prefs, false);
        assert_eq!(picked.audio_index, Some(1), "Japanese audio, not the dub");
        assert_eq!(
            picked
                .subtitle_burn
                .as_ref()
                .map(|burn| burn.subtitle_index),
            Some(0),
            "REQ-SUB-2: the one ASS track is what the viewer is here for"
        );

        // The defect: the same rule on an HDR delivery, where the burn costs
        // the grade and nobody asked for it. No subtitle, never a downgrade.
        let hdr = anime("hdmv_pgs_subtitle", Some("dolby_vision"));
        let refused = TranscodeManager::select_tracks_with_prefs(&hdr, None, None, &prefs, true);
        assert_eq!(refused.audio_index, Some(1));
        assert!(
            refused.subtitle_burn.is_none(),
            "an implicit burn may not spend the delivered grade"
        );
        // The same file on an SDR base keeps the old behaviour exactly.
        let allowed = TranscodeManager::select_tracks_with_prefs(&hdr, None, None, &prefs, false);
        assert_eq!(
            allowed
                .subtitle_burn
                .as_ref()
                .map(|burn| burn.subtitle_index),
            Some(0)
        );

        // And an explicit choice is untouched by the guard — that one the
        // viewer did ask for, and session creation is what judges it.
        let explicit =
            TranscodeManager::select_tracks_with_prefs(&hdr, None, Some(0), &prefs, true);
        assert_eq!(
            explicit
                .subtitle_burn
                .as_ref()
                .map(|burn| burn.subtitle_index),
            Some(0),
            "an override is the viewer's own decision, not the server's guess"
        );
    }

    #[test]
    fn the_generation_identity_is_the_final_directory_and_survives_a_fence() {
        // The three shapes `produce_normalized` builds, spelled the way it
        // spells them, so a change to that formatting fails here.
        let hash = "a".repeat(64);
        let unfenced = format!("{}/{hash}", &hash[..2]);
        let publication_fenced = format!("{}/{hash}-f7", &hash[..2]);
        let queue_owned = format!("{}/{hash}-j3-f9", &hash[..2]);
        assert_eq!(identity_for(&unfenced).expect("identity"), hash);
        assert_eq!(
            identity_for(&publication_fenced).expect("identity"),
            format!("{hash}-f7"),
            "a publication fence renames the staging tree with it, so the \
             checkpoint a fence bump orphans is one this pass cannot see"
        );
        assert_eq!(
            identity_for(&queue_owned).expect("identity"),
            format!("{hash}-j3-f9")
        );
        // Every shape is one `publish_controlled_directory` will accept.
        // Deriving a name it rejects would fail the production as a retryable
        // error, so the title would retry on a backoff forever and be reported
        // as an encoder fault.
        for relative in [&unfenced, &publication_fenced, &queue_owned] {
            assert!(plurx_core::transcode::manifest::is_safe_generation_id(
                identity_for(relative).expect("identity")
            ));
        }
        assert!(identity_for("").is_err(), "no shard prefix");
        assert!(identity_for("abcdef").is_err(), "no shard prefix");
        assert!(identity_for("ab/").is_err(), "no identity");
        assert!(identity_for("/abcdef").is_err(), "no shard");
        assert!(
            identity_for("ab/what a name").is_err(),
            "a name publication would reject is rejected where it is derived"
        );
        assert!(
            identity_for(&format!("ab/{}", "z".repeat(257))).is_err(),
            "and so is one publication would reject for length"
        );
    }

    // S-14 M8: the position of every awaited Session hook point. Each test
    // holds a point and checks that the step before it has happened and the
    // step after it has not, or that a path refused by the step before it
    // never reaches the point; the plan's execution log records the mutation
    // that moves each point to either side of its step and the test it fails.

    /// The path-owner point sits between the replacement-marker check and the
    /// attempt sample: a reader that sees the marker answers without reaching
    /// it, and a reader held there samples the attempts a successor installs
    /// while it waits.
    #[tokio::test]
    async fn path_owner_sample_point_follows_the_marker_check_and_precedes_the_sample() {
        let dir = crate::test_tempdir().expect("tempdir");
        seeded_session_dir(dir.path(), 2, 2.0).await;
        let session = Arc::new(test_session(dir.path().to_path_buf()));

        let point = session
            .test_hooks()
            .path_owner_sample
            .arm("path_owner_sample");
        session.replacing_child.store(true, Release);
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(2),
                session.coherent_path_producer_attempt()
            )
            .await
            .expect("a reader that sees the marker answers without waiting"),
            None
        );
        assert!(!point.was_reached(), "the marker check comes before the point");
        session.replacing_child.store(false, Release);

        let point = session
            .test_hooks()
            .path_owner_sample
            .arm("path_owner_sample");
        let reader = tokio::spawn({
            let session = Arc::clone(&session);
            async move { session.coherent_path_producer_attempt().await }
        });
        let held = point.reached().await;
        let successor = session
            .control
            .begin_producer_attempt()
            .await
            .expect("successor attempt");
        session.reset_compatibility_delivery(successor).await;
        held.release();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), reader)
                .await
                .expect("the reader answers")
                .expect("reader task"),
            Some(successor),
            "the attempts are sampled after the point"
        );
    }

    /// Compatibility publication's point sits between its first deadline
    /// check and the store: an expired publication answers without reaching
    /// it, and one held there has not published yet.
    #[tokio::test]
    async fn playlist_publication_point_follows_the_deadline_check_and_precedes_the_store() {
        let dir = crate::test_tempdir().expect("tempdir");
        seeded_session_dir(dir.path(), 2, 2.0).await;
        let session = Arc::new(test_session(dir.path().to_path_buf()));
        let attempt = session.compatibility_producer_attempt();

        let point = session
            .test_hooks()
            .playlist_publication
            .arm("playlist_publication");
        assert!(
            !tokio::time::timeout(
                Duration::from_secs(2),
                session.publish_compatibility_playlist(attempt, std::time::Instant::now()),
            )
            .await
            .expect("an expired publication answers without waiting"),
            "an expired publication does not publish"
        );
        assert!(!point.was_reached(), "the deadline check comes before the point");

        let point = session
            .test_hooks()
            .playlist_publication
            .arm("playlist_publication");
        let publish = tokio::spawn({
            let session = Arc::clone(&session);
            async move {
                session
                    .publish_compatibility_playlist(
                        attempt,
                        std::time::Instant::now() + Duration::from_secs(30),
                    )
                    .await
            }
        });
        let held = point.reached().await;
        assert_eq!(
            session.compatibility_playlist_published(attempt),
            Some(false),
            "the store comes after the point"
        );
        held.release();
        assert!(tokio::time::timeout(Duration::from_secs(5), publish)
            .await
            .expect("the publication answers")
            .expect("publication task"));
        assert_eq!(session.compatibility_playlist_published(attempt), Some(true));
    }

    /// A playlist response's point sits between the first-playlist gate and
    /// the actor observation: a playlist below the start cushion waits without
    /// reaching it, and a response held there has not reported the playlist
    /// to the actor.
    #[tokio::test]
    async fn playlist_response_point_follows_the_first_playlist_gate_and_precedes_the_observation(
    ) {
        use plurx_core::store::SqliteStore;

        let dir = crate::test_tempdir().expect("tempdir");
        let manager_dir = crate::test_tempdir().expect("manager tempdir");
        // One segment is below the first-playlist gate.
        seeded_session_dir(dir.path(), 1, 4.0).await;
        let session = Arc::new(test_session(dir.path().to_path_buf()));
        session.publication_worker_started.store(true, Release);
        install_seeded_served_playlist_snapshot(&session, 0).await;
        let point = session
            .test_hooks()
            .playlist_publication
            .arm("playlist_publication");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let mgr = Arc::new(TranscodeManager::new(
            store,
            manager_dir.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        mgr.playlist_wait_override_ms.store(5_000, Relaxed);
        mgr.sessions
            .lock()
            .await
            .insert("gate-order".into(), Arc::clone(&session));

        let playlist = tokio::spawn({
            let mgr = Arc::clone(&mgr);
            async move { mgr.playlist("gate-order").await }
        });
        tokio::time::sleep(PLAYLIST_WAIT_POLL * 2).await;
        assert!(!playlist.is_finished(), "a playlist below the cushion waits");
        assert!(
            !point.was_reached(),
            "the first-playlist gate comes before the point"
        );

        seeded_session_dir(dir.path(), 12, 4.0).await;
        install_seeded_served_playlist_snapshot(&session, 0).await;
        let held = point.reached().await;
        assert!(
            !session
                .control
                .snapshot()
                .await
                .expect("rolling actor")
                .delivery
                .playlist_ready,
            "the actor observes the playlist after the point"
        );
        held.release();
        let bytes = tokio::time::timeout(Duration::from_secs(5), playlist)
            .await
            .expect("the playlist answers")
            .expect("playlist task")
            .expect("playlist");
        assert!(String::from_utf8(bytes)
            .expect("playlist text")
            .contains("seg00011.ts"));
        assert!(
            session
                .control
                .snapshot()
                .await
                .expect("rolling actor")
                .delivery
                .playlist_ready
        );
    }

    /// Install a stand-in producer through the prepublication install path,
    /// the pipe variant when `pipe` is set.
    async fn install_stand_in_producer(
        session: &Session,
        producer_attempt: u64,
        pipe: bool,
    ) -> Result<(), String> {
        if pipe {
            session
                .spawn_and_install_prepublication_pipe_child(producer_attempt, || {
                    let mut child = tokio::process::Command::new("sleep")
                        .arg("60")
                        .stdout(std::process::Stdio::piped())
                        .kill_on_drop(true)
                        .spawn()
                        .map_err(|error| error.to_string())?;
                    let stdout = child.stdout.take().expect("piped stdout");
                    Ok((ObservedFfmpeg::for_test(child), stdout))
                })
                .await
                .map(drop)
        } else {
            session
                .spawn_and_install_prepublication_child(producer_attempt, || {
                    Ok(ObservedFfmpeg::for_test(long_running_child()))
                })
                .await
        }
    }

    /// Every producer install path's point sits between the actor's install
    /// authorization and the final install fence: a retired session's install
    /// is refused without reaching it, and retirement while an install is
    /// held there is refused by the fence.
    #[tokio::test]
    async fn producer_install_points_follow_authorization_and_precede_the_final_fence() {
        let dir = crate::test_tempdir().expect("dir");
        for pipe in [false, true] {
            let session = watchdog_session(dir.path(), None, false);
            let (mut replacement, attempt) = session
                .kill_child_for_replacement()
                .await
                .expect("admit a producer attempt");
            session.control.end().await.expect("end verdict");
            let point = session
                .test_hooks()
                .producer_install
                .arm("producer_install");
            assert!(
                tokio::time::timeout(
                    Duration::from_secs(5),
                    install_stand_in_producer(&session, attempt, pipe),
                )
                .await
                .expect("a refused authorization answers without waiting")
                .is_err(),
                "a retired session's install is refused (pipe: {pipe})"
            );
            assert!(
                !point.was_reached(),
                "authorization comes before the point (pipe: {pipe})"
            );
            replacement.settle_terminal_rejection();

            let session = watchdog_session(dir.path(), None, false);
            let (mut replacement, attempt) = session
                .kill_child_for_replacement()
                .await
                .expect("admit a producer attempt");
            let point = session
                .test_hooks()
                .producer_install
                .arm("producer_install");
            let install = install_stand_in_producer(&session, attempt, pipe);
            let retire = async {
                let held = point.reached().await;
                session.control.end().await.expect("end verdict");
                held.release();
            };
            let (result, ()) = tokio::join!(install, retire);
            assert!(
                result.is_err(),
                "the final fence comes after the point (pipe: {pipe})"
            );
            replacement.settle_terminal_rejection();
        }

        let session = watchdog_session(dir.path(), Some(long_running_child()), false);
        let (mut replacement, attempt) = session
            .kill_child_for_replacement()
            .await
            .expect("admit a replacement");
        session.control.end().await.expect("end verdict");
        let point = session
            .test_hooks()
            .producer_install
            .arm("producer_install");
        assert!(
            tokio::time::timeout(
                Duration::from_secs(5),
                session.install_replacement_child(attempt, long_running_child()),
            )
            .await
            .expect("a refused authorization answers without waiting")
            .is_err(),
            "a retired session's replacement install is refused"
        );
        assert!(
            !point.was_reached(),
            "authorization comes before the replacement install's point"
        );
        replacement.settle_terminal_rejection();
    }

    /// A refresh's point follows its playlist read: a playlist rewritten while
    /// the refresh is held there is not what it merges.
    #[tokio::test]
    async fn refresh_point_follows_the_playlist_read() {
        let dir = crate::test_tempdir().expect("tempdir");
        seeded_session_dir(dir.path(), 2, 2.0).await;
        let session = Arc::new(test_session(dir.path().to_path_buf()));
        let point = session
            .test_hooks()
            .refresh_after_read
            .arm("refresh_after_read");
        let refresh = tokio::spawn({
            let session = Arc::clone(&session);
            async move { session.refresh_segments().await }
        });
        let held = point.reached().await;
        seeded_session_dir(dir.path(), 3, 2.0).await;
        held.release();
        tokio::time::timeout(Duration::from_secs(5), refresh)
            .await
            .expect("the refresh answers")
            .expect("refresh task");
        assert_eq!(
            session.segments.lock().await.segs.len(),
            2,
            "the refresh merges the bytes it read before the point"
        );
    }

    /// An activity read's point sits after the actor snapshot and before the
    /// segment index is read: an index refreshed while the read is held there
    /// is the one it reports.
    #[tokio::test]
    async fn activity_point_precedes_the_index_read() {
        let dir = crate::test_tempdir().expect("tempdir");
        seeded_session_dir(dir.path(), 2, 2.0).await;
        let session = Arc::new(test_session(dir.path().to_path_buf()));
        let point = session.test_hooks().activity_detail.arm("activity_detail");
        let reader = tokio::spawn({
            let session = Arc::clone(&session);
            async move {
                session_info(
                    "activity-order",
                    &session,
                    AheadLimits {
                        max_secs: 0,
                        max_bytes: 0,
                        global_max_bytes: 0,
                    },
                    0,
                    0,
                )
                .await
            }
        });
        let held = point.reached().await;
        session.refresh_segments().await;
        held.release();
        let info = tokio::time::timeout(Duration::from_secs(5), reader)
            .await
            .expect("the activity read answers")
            .expect("activity task");
        assert_eq!(
            info.produced_end_ms,
            Some(4_000),
            "the index is read after the point"
        );
    }

    /// The first-media owner point follows an accepted settlement: a refused
    /// settlement never reaches it.
    #[tokio::test]
    async fn first_media_owner_point_follows_an_accepted_settlement() {
        let root = crate::test_tempdir().expect("first-media root");
        let scratch = root.path().join("scratch");
        tokio::fs::create_dir_all(&scratch)
            .await
            .expect("create scratch");
        let session =
            watchdog_session_with_publication(&scratch, Some(long_running_child()), false, true);
        let point = session
            .test_hooks()
            .first_media_owner_claim
            .arm("first_media_owner_claim");
        let (handoff, applied) =
            begin_first_media_publication_handoff(&session, "first-media-refused")
                .await
                .expect("first-media settlement capacity");
        handoff.settle_for_test(false);
        assert!(
            !tokio::time::timeout(Duration::from_secs(2), applied)
                .await
                .expect("a refused settlement answers without waiting")
                .expect("first-media handoff application"),
            "a refused settlement is not applied"
        );
        assert!(
            !point.was_reached(),
            "only an accepted settlement reaches the point"
        );
        assert!(!session.first_media_handoff_applied.load(Acquire));
        session.fail(PlaylistError::SessionFailed("test complete".into()));
    }

    /// Both scratch owners' point sits after the scratch release began and
    /// before the directory is removed.
    #[tokio::test]
    async fn scratch_cleanup_points_follow_the_release_and_precede_the_removal() {
        let root = crate::test_tempdir().expect("scratch root");
        let ledger = crate::scratch_ledger::ScratchLedger::new();
        let scratch_session = |name: &'static str| {
            let dir = root.path().join(name);
            std::fs::create_dir_all(&dir).expect("create scratch");
            std::fs::write(dir.join("seg00000.ts"), b"scratch").expect("seed scratch");
            let mut session = watchdog_session(&dir, None, false);
            Arc::get_mut(&mut session)
                .expect("unshared scratch fixture")
                .scratch = Some(
                ledger
                    .reserve(0, HLS_SCRATCH_MAX_BYTES_DEFAULT)
                    .expect("fixture admission")
                    .bound_to(name, 0),
            );
            (session, dir)
        };

        let (session, dir) = scratch_session("rolling-owner");
        let key = session.scratch.as_ref().expect("scratch permit").key();
        let point = session.test_hooks().scratch_cleanup.arm("scratch_cleanup");
        spawn_rolling_scratch_cleanup_owner("rolling-owner".to_owned(), &session);
        let held = point.reached().await;
        assert_eq!(
            ledger.lifecycle_of(key),
            Some(crate::scratch_ledger::ScratchLifecycle::Releasing),
            "the release begins before the point"
        );
        assert!(dir.exists(), "the directory is removed after the point");
        held.release();
        await_scratch_removed(&dir).await;

        let (session, dir) = scratch_session("retired-owner");
        let key = session.scratch.as_ref().expect("scratch permit").key();
        let point = session.test_hooks().scratch_cleanup.arm("scratch_cleanup");
        spawn_retired_presentation_cleanup_owner(
            Arc::new(Mutex::new(HashMap::new())),
            "retired-owner".to_owned(),
            RetiredPresentation {
                session: Arc::clone(&session),
                producer_attempt: 0,
                serve_until: Instant::now(),
            },
        );
        let held = point.reached().await;
        assert_eq!(
            ledger.lifecycle_of(key),
            Some(crate::scratch_ledger::ScratchLifecycle::Releasing),
            "the retired owner begins the release before the point"
        );
        assert!(dir.exists(), "the retired owner removes the directory after the point");
        held.release();
        await_scratch_removed(&dir).await;
    }

    /// The production hook slot of `manager`, which must still hold
    /// [`NoopTranscodeManagerHooks`] `when`.
    fn assert_manager_on_noop_hooks(manager: &TranscodeManager, when: &str) {
        let installed: &dyn std::any::Any = manager.hooks();
        assert!(
            installed.is::<NoopTranscodeManagerHooks>(),
            "the manager's slot holds the no-op hooks {when}"
        );
    }

    /// M8's shipped-shape test for the transcode manager (§3.9, §5.9): the
    /// production constructor leaves its hook slot on
    /// [`NoopTranscodeManagerHooks`], and the migrated manager paths run
    /// through that no-op to their production answers:
    ///
    /// - the real artifact-qualification publisher (the record point);
    /// - plan resolution under an enabled, partially covered policy (the
    ///   `forces_artifact_qualification` override): the one covered hardware
    ///   path is qualified and the uncovered software path is not, which a
    ///   plan forced onto the published identity would not produce;
    /// - a held-source plan through `decode_fact_source` (the plumbing
    ///   point), bound to the held descriptor;
    /// - an offline production with nothing scripted (the
    ///   `scripted_offline_outcome` override), which really enters
    ///   `produce_normalized` and pins its recipe;
    /// - a VOD publication through the serving admission and its point.
    ///
    /// The subtitle playlist, an HTTP path, is driven by the companion
    /// `transcode_manager_shipped_shape_subtitle_playlist` in `http::hls`,
    /// which the same test filter selects. No path fills the slot.
    #[tokio::test]
    async fn transcode_manager_shipped_shape() {
        use plurx_core::domain::{NewOfflinePackage, OfflineCreateOutcome};
        use plurx_core::store::keys::DECODER_HEALTH_QUALIFIED_ARTIFACTS;
        use plurx_core::store::SqliteStore;
        use plurx_core::transcode::decoder_inventory::MeasuredDecoders;
        use plurx_core::transcode::DecodeBackend;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let mut contract = decode_contract_fixture(DecodeBackend::VideoToolbox);
        contract.input_codec = "hevc".to_owned();
        contract.decoder = "hevc".to_owned();
        let policy = crate::decoder_health::DiagnosticPolicy::new(
            Some(crate::decoder_health::MeasuredBuild {
                ffmpeg_version: contract.ffmpeg_version.clone(),
                binary_sha256: contract.binary_sha256.clone(),
                buildconf_sha256: contract.buildconf_sha256.clone(),
            }),
            vec![contract],
        );
        let (manager, _work, _cache) = cached_manager(&store);
        let manager = Arc::new(
            manager
                .with_measured_decoders(MeasuredDecoders::from_measured(&[
                    ("hevc", DecodeBackend::Software, "hevc"),
                    ("hevc", DecodeBackend::VideoToolbox, "hevc"),
                ]))
                .with_diagnostic_policy(Arc::new(policy)),
        );
        assert_manager_on_noop_hooks(&manager, "after the production constructor");

        // The record point: the real publisher, with the policy requested.
        store
            .put_setting(DECODER_HEALTH_QUALIFIED_ARTIFACTS, "1")
            .await
            .expect("request the policy");
        let readiness = tokio::time::timeout(
            Duration::from_secs(5),
            manager.publish_artifact_qualification(),
        )
        .await
        .expect("the publisher answers");
        assert!(readiness.requested);
        assert_eq!(
            readiness.effective,
            plurx_core::transcode::ArtifactQualification::Unqualified,
            "one hardware contract does not qualify the whole node"
        );

        // The override point: per-path selection, never the forced identity.
        let options = manager.options_for_tone_map(
            Encoder::Software,
            &file,
            720,
            0.0,
            None,
            None,
            None,
            ToneMap::Zscale,
            OutputGrade::Sdr,
        );
        let hardware = manager
            .resolve_movie_plan(&file, &options, Encoder::VideoToolbox)
            .await
            .expect("hardware plan");
        assert_eq!(hardware.decode().backend(), DecodeBackend::VideoToolbox);
        assert!(
            hardware.enforces_receipt(),
            "the covered hardware path resolves under the qualified identity"
        );
        let software = manager
            .resolve_movie_plan(&file, &options, Encoder::Software)
            .await
            .expect("software plan");
        assert_eq!(software.decode().backend(), DecodeBackend::Software);
        assert!(
            !software.enforces_receipt(),
            "the uncovered software path keeps the unqualified identity"
        );
        assert_manager_on_noop_hooks(&manager, "after plan resolution");

        // The two override/fault points of an offline production: nothing is
        // scripted, so the recipe is really produced (and, at an expired
        // deadline, yields after pinning its recipe).
        let user = store.create_user("paul", "hash", true).await.expect("user");
        let package_id = "offline-shipped-shape";
        let requested = NewOfflinePackage {
            audio_recipe: None,
            id: package_id.to_owned(),
            request_id: "offline-shipped-shape-request".to_owned(),
            user_id: user.id,
            file_id,
            node_id: NODE.to_owned(),
            source_path: file.path.to_string_lossy().into_owned(),
            source_size: file.size,
            source_mtime: file.mtime,
            effective_rate_control: "vbr".to_owned(),
            target_height: 720,
            output_width: Some(1280),
            output_height: Some(720),
            audio_index: None,
            audio_offset_ms: 0,
            subtitle_index: None,
            subtitle_language: None,
            subtitle_mode: "none".to_owned(),
            estimated_bytes: 1_000_000,
            reserved_bytes: 1_100_000,
            expires_at: i64::MAX,
        };
        assert!(matches!(
            store
                .create_offline_package(&requested, 10, 10_000_000, 20_000_000)
                .await
                .expect("create package"),
            OfflineCreateOutcome::Created(_)
        ));
        let claimed = store
            .claim_next_offline_package(NODE)
            .await
            .expect("claim")
            .expect("queued package");
        let spec = OfflineSpec {
            audio_delivery: None,
            target_height: 720,
            audio_index: None,
            subtitle: OfflineSubtitle::None,
            effective_rate_control: EffectiveRateControl::Vbr,
        };
        let produced = tokio::time::timeout(
            Duration::from_secs(30),
            manager.ensure_offline(
                &claimed,
                &file,
                &spec,
                Instant::now(),
                &tokio_util::sync::CancellationToken::new(),
            ),
        )
        .await
        .expect("the offline pass answers")
        .expect("offline pass");
        assert!(
            matches!(produced, OfflineProduceOutcome::Yielded),
            "an unscripted production reaches the real producer and yields at its deadline"
        );
        assert!(
            store
                .offline_package_for_user(package_id, user.id)
                .await
                .expect("read package")
                .expect("package")
                .recipe_hash
                .is_some(),
            "the real production pinned its recipe"
        );
        assert_manager_on_noop_hooks(&manager, "after an offline production");

        // The VOD publication point, behind the serving admission.
        let root = crate::test_tempdir().expect("VOD root");
        manager
            .install_vod_http_test_session("vod-shipped-shape", file_id, root.path())
            .await;
        let publication = manager
            .vod_playlist("vod-shipped-shape")
            .await
            .expect("VOD publication fixture");
        assert!(
            tokio::time::timeout(
                Duration::from_secs(5),
                manager.authorize_response_publication(
                    "vod-shipped-shape",
                    &publication.owner,
                    MediaResponsePublication::attempt_media("playlist", Some("index.m3u8")),
                    Instant::now() + Duration::from_secs(1),
                ),
            )
            .await
            .expect("the VOD publication answers")
            .is_ok(),
            "the VOD publication is admitted through the no-op point"
        );
        assert_manager_on_noop_hooks(&manager, "after a VOD publication");

        // The plumbing point: a held-source plan probes through the source
        // it was handed.
        #[cfg(unix)]
        {
            let media = crate::test_tempdir().expect("media");
            let source_path = media.path().join("shipped-shape.mkv");
            std::fs::write(&source_path, b"held source").expect("source fixture");
            // Its own store: the held file is seeded into a library of its own.
            let held_store: Arc<dyn Store> =
                Arc::new(SqliteStore::open_in_memory().expect("held store"));
            let held_id = seed_real_file(&held_store, &source_path).await;
            let held_file = held_store
                .get_file(held_id)
                .await
                .expect("get held file")
                .expect("held file");
            let probe_path = media.path().join("ffprobe-shipped-shape");
            crate::write_test_executable(
                &probe_path,
                "#!/bin/sh\nif [ \"$1\" = \"-version\" ]; then printf '%s\\n' 'ffprobe version shipped-shape'; exit 0; fi\nprintf '%s\\n' '{\"streams\":[{\"index\":0,\"codec_type\":\"video\",\"codec_name\":\"h264\",\"profile\":\"High\",\"pix_fmt\":\"yuv420p\",\"width\":160,\"height\":120,\"avg_frame_rate\":\"24/1\",\"r_frame_rate\":\"24/1\",\"color_transfer\":\"bt709\",\"disposition\":{\"attached_pic\":0}}]}'\n",
                0o755,
            );
            let probe = crate::decode_facts::DecodeProbeIdentity::discover_fixture(
                probe_path.to_str().expect("probe path"),
            )
            .await
            .expect("probe identity");
            let (held_manager, _held_work, _held_cache) = cached_manager(&held_store);
            let held_manager = held_manager.with_decode_probe(Some(probe));
            let held_options = held_manager.options_for_tone_map(
                Encoder::Software,
                &held_file,
                120,
                0.0,
                None,
                None,
                Some(1),
                ToneMap::None,
                OutputGrade::Sdr,
            );
            let held = held_manager
                .resolve_held_movie_plan(
                    &held_file,
                    &held_options,
                    Encoder::Software,
                    BoundPlanCaller::Vod.decode_fact_source(
                        Arc::new(std::fs::File::open(&source_path).expect("open held source")),
                        Arc::new(tokio::sync::Semaphore::new(1)),
                    ),
                    Instant::now() + Duration::from_secs(5),
                    None,
                )
                .await
                .expect("held plan");
            assert_eq!(
                held.source_binding(),
                plurx_core::transcode::PlanSourceBinding::DescriptorBound,
                "the plan's facts were read through the held source"
            );
            assert_manager_on_noop_hooks(&held_manager, "after a held-source plan");
        }

        let waker = futures_util::task::noop_waker();
        let mut context = std::task::Context::from_waker(&waker);
        let hooks = manager.hooks();
        for (point, mut hook) in [
            (
                "before_subtitle_playlist_commit",
                hooks.before_subtitle_playlist_commit(),
            ),
            (
                "after_vod_publication_admission",
                hooks.after_vod_publication_admission(),
            ),
        ] {
            assert!(
                hook.as_mut().poll(&mut context).is_ready(),
                "the production {point} point is ready at its first poll"
            );
        }
    }
    #[test]
    fn actual_audio_delivery_changes_manifest_codec_and_rung_budget() {
        use plurx_core::playback::audio::{AudioAction, AudioDelivery};
        let mut audio = AudioDelivery {
            action: AudioAction::Encode { codec: "eac3".into(), channels: 6, layout: Some("5.1".into()), bitrate_kbps: 640, sample_rate: 48_000 },
            downmix: None,
            reason: "actual producer audio".into(),
        };
        assert_eq!(super::ladder::audio_delivery_hls_codecs("avc1.640028,mp4a.40.2".into(), Some(&audio)), "avc1.640028,ec-3");
        let legacy = super::ladder::advertised_ladder(Some(1080), 1080);
        let rungs = super::ladder::advertised_ladder_with_audio(Some(1080), 1080, Some(&audio));
        for (old, new) in legacy.iter().zip(&rungs) {
            assert_eq!(new.total_kbps, old.total_kbps + 480);
            assert_eq!(new.peak_kbps, old.peak_kbps + 480);
        }
        assert_eq!(super::ladder::advertised_ladder_with_audio(Some(1080), 1080, None), legacy);
        audio.action = AudioAction::None;
        assert_eq!(super::ladder::audio_delivery_hls_codecs("avc1.640028,mp4a.40.2".into(), Some(&audio)), "avc1.640028");
    }

    #[test]
    fn audio_intent_fingerprint_keys_the_claim_not_a_refreshed_server_answer() {
        use plurx_core::playback::audio::{AudioAction, AudioClaim, AudioDelivery, AudioSink};
        let mut request = SessionRequest {
            control_sequence: None, file_id: 1, playback_id: "player".into(), request_id: None,
            automatic: false, previous_session_id: None, reopen_reason: None,
            kind: SessionKind::Transcode { height: 720 }, start_seconds: 0.0,
            audio_index: None, audio_delivery: None, audio_claim: None,
            subtitle_burn: None, audio_offset_ms: 0, hdr10: false,
            presentation: Default::default(), block_budget_secs: None, transport: None,
        };
        let legacy = request.intent_fingerprint("paul");
        assert_eq!(legacy, r#"["paul",1,"player",0,"t720","0.000",null,null,0,null,null]"#);
        request.audio_delivery = Some(AudioDelivery { action: AudioAction::None, downmix: None, reason: "server source refresh".into() });
        assert_eq!(request.intent_fingerprint("paul"), legacy);
        request.audio_claim = Some(AudioClaim { decoders: vec!["aac".into()], sinks: vec![AudioSink { codec: "aac".into(), max_channels: 6, passthrough: false, sample_rates_hz: vec![48_000] }] });
        let claimed = request.intent_fingerprint("paul");
        assert_ne!(claimed, legacy);
        request.audio_delivery.as_mut().expect("audio answer").reason = "new explanation".into();
        assert_eq!(request.intent_fingerprint("paul"), claimed);
        request.audio_claim.as_mut().expect("sink claim").sinks[0].max_channels = 2;
        assert_ne!(request.intent_fingerprint("paul"), claimed);
    }
