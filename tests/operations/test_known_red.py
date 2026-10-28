import datetime as dt
import pathlib
import tempfile
import unittest

from validation.known_red import (
    IgnoredTest,
    KnownRedError,
    ignored_tests,
    load_entries,
    load_opt_in_fixtures,
    validate_opt_in_fixtures,
    validate_entries,
    validate_listed_tests,
)


ROOT = pathlib.Path(__file__).resolve().parents[2]


class KnownRedContractTest(unittest.TestCase):
    def _crate(self, root: pathlib.Path, lib: str) -> pathlib.Path:
        crate = root / "crates/example"
        (crate / "src").mkdir(parents=True)
        (crate / "Cargo.toml").write_text(
            '[package]\nname = "example"\nversion = "0.1.0"\nedition = "2021"\n',
            encoding="utf-8",
        )
        source = crate / "src/lib.rs"
        source.write_text(lib, encoding="utf-8")
        return crate

    @staticmethod
    def _entry(identity: str) -> dict[str, object]:
        return {
            "suite": "rust",
            "test": identity,
            "owner": "ci",
            "reason": "repair pending",
            "expires": dt.date(2026, 10, 1),
        }

    def test_every_checked_in_entry_is_current_and_ignored(self):
        ignored = ignored_tests(ROOT)
        # S-01 adds two reasoned ffmpeg fixture ignores to the P-01 baseline.
        # S-08 adds two more of the same shape: the deinterlace argv fixture
        # matrix and the descriptor-bound idet pass, each of which needs the
        # shipped ffmpeg or ffprobe named in its own ignore reason.
        # L-03 M3 adds two operator-run caption audits: the per-node hardware
        # audit and the broadcast-capture summary, each driven by
        # scripts/live-tv-caption-audit with the hardware or capture its
        # ignore reason names.
        # K-05 M0 adds the operator-run Hiqlite statement capture,
        # k05_capture_hiqlite_statements, which writes the file named by
        # K05_HIQLITE_CAPTURE for the query-plan evidence.
        # K-08 M4 adds the tokenizer regex-backend equivalence check, which
        # needs the pinned model files its ignore reason names.
        # K-08 M5 adds embed_thread_scaling, the inference thread-count
        # measurement behind EMBED_THREADS, which needs the same model files.
        fixtures = load_opt_in_fixtures()
        validate_opt_in_fixtures(fixtures, ignored, load_entries())
        fixture_ids = {fixture["test"] for fixture in fixtures}
        self.assertEqual(fixture_ids, {
            "crates/plurxd/src/transcoder_optimization/comparison.rs::transcoder_optimization::comparison::tests::real_media_production_tools_comparison",
            "crates/plurxd/src/pipeprobe.rs::pipeprobe::tests::real_neutral_peak_gate_rejects_old_metal_units",
            "crates/plurxd/src/subtitle_transcription.rs::subtitle_transcription::tests::managed_real_engine_held_fd_clean_environment_recognizes_fixture",
            "crates/plurxd/src/http/shared_receiver_forwarding_fixture.rs::http::shared_receiver_fixture::forwarding_fixture::sharing_receiver_nonowner_signed_http_resource_and_end_require_actual_driver_closure",
            "crates/plurxd/src/http/shared_receiver_fixture.rs::http::shared_receiver_fixture::sharing_receiver_real_pinned_source_h1_b_h1_h2_start_resources_and_confirmed_end",
            "crates/plurxd/src/http/shared_receiver_fixture.rs::http::shared_receiver_fixture::sharing_receiver_real_pinned_source_encoded_and_native_lanes_through_b",
            "crates/plurxd/src/http/shared_receiver_fixture.rs::http::shared_receiver_fixture::sharing_receiver_real_pinned_source_direct_range_head_through_b",
            "crates/plurxd/src/http/shared_receiver_fixture.rs::http::shared_receiver_fixture::sharing_receiver_real_pinned_quality_reopen_preserves_position_and_releases_slot",
            "crates/plurxd/src/http/shared_receiver_fixture.rs::http::shared_receiver_fixture::sharing_receiver_real_pinned_prepared_handoff_commit_and_abort",
            "crates/plurxd/src/http/tests.rs::http::tests::sharing_pinned_transport_recovers_committed_claim_and_rotation_after_restart",
            "crates/plurxd/tests/sharing_daemon_restart.rs::sharing_separate_daemons_preserve_pending_pairing_and_rotation_across_restart",
            "crates/plurxd/src/http/sharing_start_decode.rs::http::sharing_start_decode::tests::sharing_start_transport_pinned_source_h1_and_receiver_h1_h2_preserve_raw_envelope",
            "crates/plurxd/src/http/shared_library/tests.rs::http::shared_library::tests::sharing_receiver_pinned_decision_http1_http2_revalidates_file_assignment_and_login",
            "crates/plurxd/src/http/shared_library/tests.rs::http::shared_library::tests::sharing_receiver_pinned_source_blocked_http1_http2_revalidate_current_scope",
            "crates/plurxd/src/http/shared_library/tests.rs::http::shared_library::tests::sharing_admin_pinned_library_read_bootstraps_empty_matrix_and_fences_connection",
            "crates/plurxd/src/subtitle_text_translation/engine.rs::subtitle_text_translation::engine::qualification::pinned_publisher_bundle_passes_production_multilingual_self_test",
            "crates/plurxd/src/subtitle_translation/installer.rs::subtitle_translation::installer::qualification::pinned_whisper_install_and_verify_qualify_both_speech_tasks",
            "crates/plurxd/src/subtitle_translation/installer.rs::subtitle_translation::installer::qualification::pinned_progressive_worker_commits_both_tasks_for_selected_audio_two",
            "crates/plurxd/src/subtitle_translation/installer.rs::subtitle_translation::installer::qualification::pinned_progressive_worker_covers_advancing_playback_for_both_tasks",
        })
        # #811 adds three ffmpeg 8 fixture checks of the repeated-HEVC
        # description collapse (generations, output, copy pipe), each needing
        # the captured ffmpeg 8 media its ignore reason names.
        # S11's operator-only capture is not a known-red product regression.
        # It requires an owned manifest, real browser and an explicit ignored
        # invocation; its module is feature/Unix-gated, absent in default lists.
        capture_identity = (
            "crates/plurxd/src/transcode/tests/rolling_grid_campaign.rs::"
            "transcode::tests::rolling_grid_campaign::owned_real_rolling_cell"
        )
        captures = tuple(item for item in ignored if item.identity == capture_identity)
        self.assertEqual(len(captures), 1)
        capture = captures[0]
        self.assertEqual(capture.reason, "explicit owned rolling campaign only; requires validated manifest and real browser")
        self.assertEqual(capture.cargo_name, "transcode::tests::rolling_grid_campaign::owned_real_rolling_cell")
        # S10 is also an explicitly admitted acquisition, not a known-red
        # product regression. Unlike S11, its source is not feature-gated:
        # it must resolve once in an ordinary Cargo listing, never be absent.
        public_wire_identity = (
            "crates/plurxd/src/http/tests/public_copy_wire.rs::"
            "http::tests::public_copy_wire::"
            "public_copy_new_retained_attachment_freezes_measured_master_and_exact_mux_wire"
        )
        public_wires = tuple(item for item in ignored if item.identity == public_wire_identity)
        self.assertEqual(len(public_wires), 1)
        public_wire = public_wires[0]
        self.assertEqual(public_wire.reason, "requires explicit frozen-source and externally bounded runtime admission")
        self.assertEqual(
            public_wire.cargo_name,
            "http::tests::public_copy_wire::public_copy_new_retained_attachment_freezes_measured_master_and_exact_mux_wire",
        )
        subtitle_identity = (
            "crates/plurxd/src/subtitle_transcription.rs::subtitle_transcription::tests::"
            "managed_real_engine_held_fd_clean_environment_recognizes_fixture"
        )
        subtitle_fixtures = tuple(item for item in ignored if item.identity == subtitle_identity)
        self.assertEqual(len(subtitle_fixtures), 1)
        subtitle_fixture = subtitle_fixtures[0]
        self.assertEqual(
            subtitle_fixture.reason,
            "requires PLURX_SUBTITLE_ENGINE, PLURX_TEST_SUBTITLE_MODEL and packaged ffmpeg",
        )
        validate_listed_tests(subtitle_fixtures, (subtitle_fixture.cargo_name,))
        with self.assertRaisesRegex(KnownRedError, "absent"):
            validate_listed_tests(subtitle_fixtures, ())
        # Native desktop harnesses are exact opt-in fixtures, not known-red
        # product tests. The child entry is invoked by its parent regression;
        # privileged fixtures are executed only in a disposable installation.
        desktop_harness_reasons = {
            "crates/plurx-desktop-control/tests/linux_launch_lock.rs::"
            "linux_launch_lock_allows_only_the_service_group_and_root":
                "requires disposable Linux root and explicit service UID/GID and unrelated UID fixtures",
            "crates/plurx-tray/src/tests.rs::tests::process::"
            "real_controller_runner_writes_effective_xdg_and_refuses_invalid_values":
                "requires an explicitly supplied real controller and root-owned disposable installation record",
            "crates/plurx-tray/src/tests.rs::tests::process::xdg_runner_child":
                "subprocess entry used by the XDG runner regressions",
        }
        desktop_harnesses = tuple(
            item for item in ignored if item.identity in desktop_harness_reasons
        )
        self.assertEqual(
            {item.identity: item.reason for item in desktop_harnesses},
            desktop_harness_reasons,
        )
        admitted_identities = (
            {capture_identity, public_wire_identity}
            | fixture_ids
            | desktop_harness_reasons.keys()
        )
        # Main's native FFmpeg/libvmaf qualification remains independently
        # admitted; keep it in addition to effort's two acquisition identities.
        # #811 adds three operator-run ffmpeg 8 fixture checks (captured
        # generations, a captured output and a captured copy pipe), each
        # needing the capture its ignore reason names: 21 -> 24.
        # Main #859 adds three explicit Linux namespace controls; Cinema B04
        # adds a loopback-only synthetic acceptance server. Bind their identities
        # and environment reasons rather than silently admitting unknown ignores.
        additional_fixture_reasons = {
            "crates/plurx-core/src/process/fixed_probe/tests.rs::process::fixed_probe::tests::" + name:
                "requires a compiled daemon in PLURX_TEST_NAMESPACE_BOOTSTRAP and unprivileged user namespaces"
            for name in (
                "namespace_probe_executes_bound_source_and_denies_secondary_images",
                "namespace_probe_hides_parent_files_and_reaps_descendants",
                "namespace_probe_cancellation_kills_started_descendants",
            )
        }
        additional_fixture_reasons[
            "crates/plurxd/src/http/remote/live_fixture.rs::http::remote::live_fixture::"
            "remote_live_server_fixture"
        ] = "explicit loopback-only synthetic acceptance server; stops by file, Ctrl-C or one hour"
        additional_fixture_reasons[
            "crates/plurx-core/src/transcode/encoder.rs::transcode::encoder::tests::"
            "benchmark_real_cpu_emits_a_complete_positive_measurement"
        ] = "requires a real FFmpeg with libx264"
        # The DV processing effort (#968) adds one operator-run probe of the
        # installed FEL helper against a bounded timestamped source.
        additional_fixture_reasons[
            "crates/plurxd/src/dv_segment.rs::dv_segment::tests::"
            "actual_segment_helper_nut_encoder_preserves_owned_custody"
        ] = "requires reviewed Linux FEL helper and bounded timestamped source"
        # The tone-map census (#1019) adds one operator-run smoke of a real
        # census on the build host through the production spawn seam.
        additional_fixture_reasons[
            "crates/plurxd/src/pipeprobe/census.rs::pipeprobe::census::tests::"
            "census_smoke_on_this_host"
        ] = "needs real ffmpeg and, for a hardware encoder, a GPU"
        # Shared-read indexing adds one operator-run differential over a real
        # source on the target storage, named by PLURX_SHARED_READ_SOURCE.
        additional_fixture_reasons[
            "crates/plurxd/src/fragindex/tests.rs::fragindex::tests::"
            "real_source_shared_read_differential"
        ] = "reads a whole real source several times; run by hand on the target storage"
        # DV repair R02 exercises actual source probing, native helper windows,
        # AAC cadence and VAAPI output; it requires the installed HDR graph.
        additional_fixture_reasons[
            "crates/plurxd/src/vodencode_manager_tests.rs::transcode::tests::"
            "actual_processed_hdr_windows_verify_audio_cadence_native_shapes_and_seek"
        ] = "requires an actual HDR source, reviewed helper bundle and proved VAAPI device"
        # DV Profile 5 completeness uses private, bounded FFmpeg captures;
        # keep these operator-run checks distinct from known-red tests.
        for identity in (
            "crates/plurx-core/src/fmp4/dvh1_tests.rs::fmp4::tests::"
            "captured_dvh1_completeness_preserves_dolby_vision_and_publishes",
            "crates/plurxd/src/copyseg.rs::copyseg::tests::"
            "captured_dvh1_rolling_options_publish_and_reject_unproven_samples",
            "crates/plurxd/src/fragindex/tests.rs::fragindex::tests::"
            "captured_dvh1_completeness_builds_a_constant_index",
        ):
            additional_fixture_reasons[identity] = (
                "needs a private FFmpeg dvh1 capture; PLURX_DVH1_CAPTURE"
            )
        # Mac package repair (#1239) executes actual Metal/Vulkan graphs with
        # an explicit complete package; ordinary Linux unit runners lack it.
        additional_fixture_reasons[
            "crates/plurxd/src/ffmpeg_support.rs::ffmpeg_support::tests::"
            "complete_macos_package_runs_metal_and_vulkan"
        ] = "requires PLURX_TEST_FFMPEG pointing at a complete native Mac package"
        # Profile 7 FEL qualification (#1290) runs the deployed helper on a
        # radeonsi VA-API device against a real P7 FEL title.
        additional_fixture_reasons[
            "crates/plurxd/src/source_probe/title_work_tests.rs::source_probe::title_work::tests::"
            "physical_title_job_qualifies_a_p7_fel_title_and_stores_its_verdict"
        ] = "physical: the deployed processing helper, a radeonsi VA-API device and a P7 FEL title"
        # The title/index parity check (#1301) needs a real title, the
        # processing bundle and its standalone aggregate; ordinary unit
        # runners do not provide those operator-owned inputs.
        additional_fixture_reasons[
            "crates/plurxd/src/fragindex/tests.rs::fragindex::tests::dv_title_companion::"
            "physical_the_index_pass_reads_the_standalone_title_aggregate"
        ] = "physical: a processing bundle (PLURX_DV_PROCESSING_DIR), a Dolby Vision title and its standalone aggregate"
        additional_fixtures = tuple(
            item for item in ignored if item.identity in additional_fixture_reasons
        )
        self.assertEqual(
            {item.identity: item.reason for item in additional_fixtures},
            additional_fixture_reasons,
        )
        self.assertEqual(len(additional_fixtures), 15)
        # HDR/DV and the explicit Mac graph check are operator-run fixtures.
        self.assertEqual(len(tuple(item for item in ignored if item.identity not in admitted_identities)), 39)
        validate_listed_tests(public_wires, (public_wire.cargo_name,))
        with self.assertRaisesRegex(KnownRedError, "absent"):
            validate_listed_tests(public_wires, ())
        # Optional absence is exact-identity only, and feature-enabled presence
        # must still resolve once. A similarly named unknown source refuses.
        validate_listed_tests(captures, ())
        validate_listed_tests(captures, (capture.cargo_name,))
        foreign = IgnoredTest(capture.identity + "_foreign", capture.cargo_name + "_foreign",
                              capture.reason, capture.path, capture.line)
        with self.assertRaisesRegex(KnownRedError, "absent"):
            validate_listed_tests((foreign,), ())
        self.assertTrue(all(item.reason for item in ignored))
        self.assertTrue(all(item.path in item.identity for item in ignored))
        self.assertTrue(all(item.cargo_name in item.identity for item in ignored))
        validate_entries(load_entries(), ignored, dt.date(2026, 9, 20))

    def test_opt_in_fixture_is_explicit_and_separate_from_known_red(self):
        ignored = (IgnoredTest("crates/example/src/lib.rs::held_case", "held_case", "namespace", "crates/example/src/lib.rs", 1),)
        fixture = {"test": ignored[0].identity, "owner": "sharing", "reason": "namespace", "requires": "disposable network", "command": "cargo test -p example held_case -- --ignored --exact"}
        validate_opt_in_fixtures((fixture,), ignored)
        for invalid in (
            {**fixture, "test": "held_case"},
            {**fixture, "requires": ""},
            {**fixture, "command": "cargo test -p example held_case"},
        ):
            with self.assertRaises(KnownRedError):
                validate_opt_in_fixtures((invalid,), ignored)
        with self.assertRaises(KnownRedError):
            validate_opt_in_fixtures((fixture, fixture), ignored)
        with self.assertRaisesRegex(KnownRedError, "known-red debt"):
            validate_opt_in_fixtures((fixture,), ignored, (self._entry(ignored[0].identity),))

    def test_comments_and_raw_text_cannot_create_ignore_attributes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self._crate(
                root,
                '''
// #[test]\n// #[ignore = "comment"]\n// fn commented() {}\n
/* #[ignore = "block"] fn blocked() {} */
const EXAMPLE: &str = r###"#[ignore = "raw"] fn raw_text() {}"###;

#[test]
#[ignore = r"owned reason"]
fn held_case() {}
''',
            )
            ignored = ignored_tests(root)
            self.assertEqual(len(ignored), 1)
            self.assertEqual(ignored[0].cargo_name, "held_case")
            self.assertEqual(ignored[0].reason, "owned reason")

    def test_literal_delimiters_cannot_unbalance_a_module_or_attribute(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self._crate(
                root,
                r'''
mod suite {
    const OPEN: &[u8] = b"{";
    const CLOSE: &str = r#"}"#;

    #[test]
    #[ignore = "owned, reason"]
    fn held_case() {}
}
''',
            )
            ignored = ignored_tests(root)
            self.assertEqual(len(ignored), 1)
            self.assertEqual(ignored[0].cargo_name, "suite::held_case")
            self.assertEqual(ignored[0].reason, "owned, reason")

    def test_bare_or_empty_ignore_reason_fails_closed(self):
        for attribute in (
            "#[ignore]",
            '#[ignore = ""]',
            '#[ignore = b"bytes are not a Rust ignore reason"]',
            "#[cfg_attr(test, ignore)]",
            '#[cfg_attr(test, ignore = "")]',
        ):
            with self.subTest(attribute=attribute), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                self._crate(root, f"#[test]\n{attribute}\nfn held_case() {{}}\n")
                with self.assertRaisesRegex(KnownRedError, "ignore"):
                    ignored_tests(root)

    def test_detached_ignore_does_not_attach_across_an_unrelated_item(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self._crate(
                root,
                '''
#[ignore = "owned reason"]
const DETACHED: () = ();

#[test]
fn later_test() {}
''',
            )
            with self.assertRaisesRegex(KnownRedError, "detached"):
                ignored_tests(root)

    def test_ignore_must_share_the_attribute_bundle_of_a_test(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self._crate(root, '#[ignore = "owned reason"]\nfn helper() {}\n')
            with self.assertRaisesRegex(KnownRedError, "not attached to a test"):
                ignored_tests(root)

    def test_reasoned_ignore_has_a_stable_source_and_module_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self._crate(
                root,
                '''
mod suite {
    #[cfg_attr(test, tokio::test, ignore = "hardware owner: remove after lab qualification")]
    async fn held_case() {}
}
''',
            )
            ignored = ignored_tests(root)
            self.assertEqual(
                ignored,
                (
                    IgnoredTest(
                        identity="crates/example/src/lib.rs::suite::held_case",
                        cargo_name="suite::held_case",
                        reason="hardware owner: remove after lab qualification",
                        path="crates/example/src/lib.rs",
                        line=3,
                    ),
                ),
            )

    def test_same_function_name_across_modules_and_files_is_not_a_bare_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            crate = self._crate(
                root,
                '''
mod alpha {
    #[test]
    #[ignore = "alpha owner"]
    fn held_case() {}
}
mod beta;
''',
            )
            (crate / "src/beta.rs").write_text(
                '#[test]\n#[ignore = "beta owner"]\nfn held_case() {}\n',
                encoding="utf-8",
            )
            ignored = ignored_tests(root)
            self.assertEqual(
                {item.identity for item in ignored},
                {
                    "crates/example/src/lib.rs::alpha::held_case",
                    "crates/example/src/beta.rs::beta::held_case",
                },
            )
            with self.assertRaisesRegex(KnownRedError, "one ignored test identity"):
                validate_entries(
                    (self._entry("held_case"),), ignored, dt.date(2026, 9, 20)
                )
            validate_entries(
                (self._entry("crates/example/src/lib.rs::alpha::held_case"),),
                ignored,
                dt.date(2026, 9, 20),
            )
            validate_listed_tests(
                ignored, ("alpha::held_case", "beta::held_case")
            )

    def test_duplicate_catalog_and_cargo_identities_fail_closed(self):
        ignored = (
            IgnoredTest(
                "crates/example/src/lib.rs::suite::held_case",
                "suite::held_case",
                "reason",
                "crates/example/src/lib.rs",
                1,
            ),
        )
        entry = self._entry(ignored[0].identity)
        with self.assertRaisesRegex(KnownRedError, "duplicates"):
            validate_entries((entry, entry), ignored, dt.date(2026, 9, 20))
        with self.assertRaisesRegex(KnownRedError, "ambiguous"):
            validate_listed_tests(
                ignored, ("suite::held_case", "suite::held_case")
            )
        with self.assertRaisesRegex(KnownRedError, "absent"):
            validate_listed_tests(ignored, ("prefix::suite::held_case",))

    def test_nonignored_and_expired_entries_fail_closed(self):
        ignored = (
            IgnoredTest(
                "crates/example/src/lib.rs::held_case",
                "held_case",
                "reason",
                "crates/example/src/lib.rs",
                1,
            ),
        )
        base = self._entry(ignored[0].identity)
        validate_entries((base,), ignored, dt.date(2026, 9, 20))
        with self.assertRaises(KnownRedError):
            validate_entries(
                ({**base, "test": "crates/example/src/lib.rs::green_case"},),
                ignored,
                dt.date(2026, 9, 20),
            )
        with self.assertRaises(KnownRedError):
            validate_entries(
                ({**base, "expires": dt.date(2026, 9, 20)},),
                ignored,
                dt.date(2026, 9, 20),
            )


if __name__ == "__main__":
    unittest.main()
