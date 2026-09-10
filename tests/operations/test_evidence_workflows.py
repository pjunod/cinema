from __future__ import annotations

from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]


class EvidenceWorkflowCase(unittest.TestCase):
    def read(self, path: str) -> str:
        return (ROOT / path).read_text(encoding="utf-8")

    def test_fix_evidence_is_label_opt_in_and_report_only(self) -> None:
        workflow = self.read(".github/workflows/fix-evidence.yml")
        self.assertIn("'fixes-behavior'", workflow)
        self.assertIn("scripts/prove-fix", workflow)
        self.assertIn("continue-on-error: true", workflow)
        self.assertIn("This is report-only", workflow)
        self.assertIn("timeout-minutes: 60", workflow)

    def test_nightly_mutation_scope_is_bounded_and_artifacted(self) -> None:
        workflow = self.read(".github/workflows/validation-nightly.yml")
        self.assertIn("git log --since=7.days --name-only", workflow)
        self.assertIn("cargo mutants --timeout 300", workflow)
        self.assertIn("--file", workflow)
        self.assertIn("timeout-minutes: 60", workflow)
        self.assertIn("continue-on-error: true", workflow)
        self.assertIn("target/mutants", workflow)
        self.assertIn("GITHUB_STEP_SUMMARY", workflow)
        self.assertIn("name: report-only weekly mutation spot-check", workflow)
        self.assertIn("name: nightly-mutation-evidence", workflow)

    def test_pgs_fuzzer_is_bounded_seeded_artifacted_and_gating(self) -> None:
        workflow = self.read(".github/workflows/validation-nightly.yml")
        self.assertIn(
            "cargo +nightly-2026-08-01 fuzz run inspect_sup fuzz/corpus/inspect_sup",
            workflow,
        )
        self.assertIn("-max_total_time=900", workflow)
        self.assertIn("fuzz/artifacts", workflow)
        self.assertIn("steps.pgs_fuzz.outcome == 'failure'", workflow)
        self.assertIn("name: bounded PGS parser fuzz campaign", workflow)
        self.assertIn("name: nightly-pgs-fuzz-evidence", workflow)
        self.assertIn("target/validation/pgs-fuzz.log", workflow)
        self.assertIn("seed_pgs_crash:", workflow)
        self.assertIn("PLURX_FUZZ_SEEDED_CRASH", workflow)
        self.assertIn(
            'seeded_crash_enabled(std::env::var_os("PLURX_FUZZ_SEEDED_CRASH").as_deref())',
            self.read("fuzz/fuzz_targets/inspect_sup.rs"),
        )
        self.assertIn(
            "only_the_explicit_seed_enables_the_crash_proof",
            self.read("fuzz/src/lib.rs"),
        )
        self.assertIn("test --manifest-path fuzz/Cargo.toml --lib", workflow)
        self.assertIn("exit 1", workflow)
        self.assertTrue((ROOT / "fuzz/corpus/inspect_sup/minimal-header").is_file())

    def test_nightly_deep_fuzz_and_mutation_jobs_are_independent(self) -> None:
        workflow = self.read(".github/workflows/validation-nightly.yml")
        deep = workflow.split("\n  deep-validation:\n", 1)[1].split("\n  pgs-fuzz:\n", 1)[0]
        fuzz = workflow.split("\n  pgs-fuzz:\n", 1)[1].split("\n  mutation:\n", 1)[0]
        mutation = workflow.split("\n  mutation:\n", 1)[1]

        self.assertIn("run: make validate-nightly", deep)
        self.assertNotIn("cargo-fuzz", deep)
        self.assertNotIn("cargo-mutants", deep)
        self.assertIn("cargo-fuzz", fuzz)
        self.assertNotIn("needs:", fuzz)
        self.assertIn("cargo-mutants", mutation)
        self.assertNotIn("needs:", mutation)

    def test_nightly_runs_and_retains_the_auto_cliff_acceptance(self) -> None:
        makefile = self.read("Makefile")
        points = self.read("validation/points.toml")
        workflow = self.read(".github/workflows/validation-nightly.yml")
        action = self.read(".github/actions/playwright/action.yml")

        self.assertIn("playback-stall-recovery:", makefile)
        self.assertIn("--suite stall-recovery", makefile)
        self.assertIn("--network-profile 8mbps-to-1.5mbps", makefile)
        self.assertIn("target/playback-lab/acceptance/auto-cliff.json", makefile)
        self.assertIn('id = "playback-auto-cliff"', points)
        auto = points.split('id = "playback-auto-cliff"', 1)[1].split("[[checks]]", 1)[0]
        self.assertIn('profiles = ["nightly"]', auto)
        self.assertIn('missing = "fail"', auto)
        self.assertIn("make playback-stall-recovery", auto)
        self.assertIn("target/playback-lab", workflow)
        self.assertIn("PLURX_PLAYBACK_CHROME=$chromium", action)

    def test_pgs_periodic_refresh_and_completion_prune_remain_wired(self) -> None:
        apple = self.read("clients/apple/Sources/PlayerController.swift")
        server = self.read("crates/plurxd/src/pgs_overlay.rs")
        self.assertIn("PGSOverlayPolicy.periodicRefreshPosition", apple)
        self.assertIn("self.refreshPGSOverlayWindow(at: overlayPosition)", apple)
        self.assertEqual(server.count("prune(&root).await;"), 1)

    def test_media_origin_and_contract_routing_remain_wired(self) -> None:
        android = self.read("clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt")
        android_screen = self.read(
            "clients/android/app/src/main/java/tv/plurx/app/player/PlayerScreen.kt"
        )
        android_adapter = self.read(
            "clients/android/app/src/main/java/tv/plurx/app/player/PlayerKeyAdapter.kt"
        )
        android_policy = self.read(
            "clients/android/app/src/main/java/tv/plurx/app/player/PlayerInputPolicy.kt"
        )
        apple = self.read("clients/apple/Sources/PlayerController.swift")
        apple_view = self.read("clients/apple/Sources/PlayerView.swift")
        apple_adapter = self.read("clients/apple/Sources/PlayerRemoteAdapter.swift")
        apple_policy = self.read("clients/apple/Sources/PlayerInputRouting.swift")
        hls = self.read("crates/plurxd/src/http/hls.rs")

        self.assertIn("return realMediaPositionMs(", android)
        self.assertIn("val timeline = sessionPlaybackTimeline(hls, requestedStartMs = ms)", android)
        self.assertIn(".setTransferListener(progressiveMediaOrigin)", android)
        self.assertIn(".playerInputAdapter(", android_screen)
        self.assertIn("PlayerInputPolicy.route(surface, state(), input)", android_adapter)
        self.assertIn("PlayerInputState.Hidden ->", android_policy)
        self.assertNotIn("HiddenSeekAccumulator", android_screen)
        self.assertIn("nextBaseMs = Self.sessionMediaOriginMs(hls, requestedStartMs: startMs)", apple)

        self.assertIn(".playerRemoteAdapter(", apple_view)
        self.assertIn("PlayerInputRouting.route(", apple_adapter)
        hidden = apple_policy.split("case .hidden:", 1)[1].split("case .transport:", 1)[0]
        self.assertIn("case .left, .right, .up, .down, .select, .tapSurface: .reveal", hidden)
        self.assertNotIn("controller.skip", hidden)
        self.assertIn("skipped Apple HEVC tier normalization", hls)

    def test_prepared_switches_preserve_authority_and_real_frame_evidence(self) -> None:
        apple = self.read("clients/apple/Sources/PlayerController.swift")
        apple_commit = apple.split("func commitPreparedSuccessor(", 1)[1].split(
            "private func awaitPreparedFirstFrame", 1
        )[0]
        self.assertNotIn("release(session:", apple_commit)
        self.assertIn("guard let firstFrameUnixMs", apple_commit)

        android = self.read(
            "clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt"
        )
        android_poll = android.split("private fun pollPreparedReplacement()", 1)[1].split(
            "private fun commitPreparedReplacement", 1
        )[0]
        self.assertIn("preparedAlignedFilmMs", android_poll)
        self.assertIn("successor.seekTo(successorAttachPositionMs", android_poll)
        self.assertIn("val restored = rollbackSwitchedReplacement()", android_poll)
        self.assertIn("failSwitchedReplacement()", android_poll)
        self.assertIn('restartAt(realPosition(), "prepared successor rendered no frame")', android_poll)
        self.assertNotIn("settleCommitOnFirstFrame(System.currentTimeMillis())", android_poll)
        self.assertEqual(
            android.count("settleCommitOnFirstFrame(System.currentTimeMillis())"),
            1,
            "only Media3's real rendered-frame callback may publish a commit",
        )
        collect = android.split("fun collectRetiredPlayer()", 1)[1].split(
            "private var pendingAcknowledgement", 1
        )[0]
        self.assertIn("awaitingCommitFrameSinceMs != null", collect)
        rollback = android.split("private fun rollbackSwitchedReplacement()", 1)[1].split(
            "private fun failSwitchedReplacement()", 1
        )[0]
        self.assertIn("player = predecessor.player", rollback)
        self.assertIn("preparedRollbackReopen", rollback)
        self.assertIn("predecessor.player.playbackParameters", rollback)
        self.assertIn("predecessor.player.playWhenReady", rollback)
        commit = android.split("private fun commitPreparedReplacement", 1)[1].split(
            "fun collectRetiredPlayer()", 1
        )[0]
        self.assertIn("previous.playbackParameters", commit)
        self.assertIn("previous.playWhenReady", commit)
        self.assertIn("PREPARED_ALIGNMENT_SLACK_MS", commit)
        release = android.split("fun release()", 1)[1].split("fun switchAudio", 1)[0]
        self.assertIn(
            "endPlaybackControl(settling) { endingSession?.let(vm::endHlsSession) }",
            release,
        )
        self.assertNotIn("sessionId?.let { vm.endHlsSession(it) }", release)
        session = self.read(
            "clients/android/app/src/main/java/tv/plurx/app/player/PlaybackControlSession.kt"
        )
        finish = session.split("fun endAfterFinalExchange(", 1)[1].split(
            "private companion object", 1
        )[0]
        self.assertIn("subject.settle(outerScope, settling, ending)", finish)
        self.assertLess(finish.index("subject.stop()"), finish.rindex("afterFinalExchange()"))

    def test_native_sign_out_is_bounded_to_the_captured_session(self) -> None:
        apple_model = self.read("clients/apple/Sources/AppModel.swift")
        apple_api = self.read("clients/apple/Sources/PlurxAPI.swift")
        self.assertIn("let capturedOrigin = origin", apple_model)
        self.assertIn("Session.shared.token == token", apple_model)
        self.assertIn("where code == 401", apple_model)
        self.assertIn(
            "PlurxAPI(origin: capturedOrigin).logout(token: token)", apple_model
        )
        self.assertIn("configuration.timeoutIntervalForRequest = 5", apple_api)
        self.assertIn('req.setValue("Bearer \\(token)"', apple_api)

        android = self.read(
            "clients/android/app/src/main/java/tv/plurx/app/ui/AppViewModel.kt"
        )
        logout = android.split("private fun logout(removeDownloads: Boolean)", 1)[1].split(
            "fun changeServer()", 1
        )[0]
        self.assertIn("val capturedOrigin = Session.origin", logout)
        self.assertIn("val capturedToken = Session.token", logout)
        self.assertIn("withTimeoutOrNull(5_000L)", logout)
        self.assertIn("Net.profileClient(capturedToken)", logout)
        self.assertIn("withContext(NonCancellable)", logout)
        self.assertIn("Session.origin == capturedOrigin", logout)
        self.assertIn("Session.token == capturedToken", logout)
        self.assertIn("OfflineDownloads.removeProfile(instance, user)", logout)
        self.assertNotIn("error.code() == 401 || error.code() == 403", logout)

    def test_library_channel_collection_urls_match_router_and_fail_truthfully(self) -> None:
        http = self.read("crates/plurxd/src/http/mod.rs")
        routes = self.read("crates/plurxd/src/http/library_channels.rs")
        web = self.read("crates/plurxd/src/web/index.html")
        web_errors = self.read("crates/plurxd/src/web/library-channels.js")
        apple = self.read("clients/apple/Sources/PlurxAPI.swift")
        android = self.read(
            "clients/android/app/src/main/java/tv/plurx/app/data/PlurxApi.kt"
        )

        self.assertIn("library_channels::collection_router()", http)
        self.assertIn(
            "library_channel_collection_routes_accept_rollout_spellings_with_same_guards",
            http,
        )
        self.assertIn(
            '.route("/library-channels", get(list).post(create))', routes
        )
        self.assertIn(
            '.route("/library-channels/", get(list).post(create))', routes
        )
        self.assertEqual(routes.count("DefaultBodyLimit::max(64 * 1024)"), 2)
        self.assertIn('api(`/library-channels?${query}`)', web)
        self.assertIn('api("/library-channels",{method:"POST"', web)
        self.assertNotIn("/library-channels/?", web)
        self.assertIn('get("library-channels", query: query)', apple)
        self.assertIn('post("library-channels", body: definition)', apple)
        self.assertIn('@GET("library-channels")', android)
        self.assertIn('@POST("library-channels")', android)

        error_view = web_errors.split("function errorView(error, context)", 1)[1].split(
            "return {code, title: selected[0], detail: selected[1]};", 1
        )[0]
        self.assertIn(
            'status === 404 && collectionRoute ? "channel_route_unavailable"',
            error_view,
        )
        self.assertIn("known[code] || known.channel_request_failed", error_view)
        self.assertIn("channel_store_unavailable", error_view)
        self.assertIn(
            "LibraryChannelCore.errorView(error,{collection:true})", web
        )

    def test_android_final_alignment_completes_before_surface_transfer(self) -> None:
        controller = self.read(
            "clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt"
        )
        poll = controller.split("private fun pollPreparedReplacement()", 1)[1].split(
            "private fun commitPreparedReplacement", 1
        )[0]
        commit = controller.split("private fun commitPreparedReplacement", 1)[1].split(
            "private fun settleCommitOnFirstFrame", 1
        )[0]
        self.assertIn("preparedCommitAlignmentFilmMs", poll)
        self.assertIn("successor.seekTo", poll)
        self.assertIn("commitPreparedReplacement(incumbentFilmMs)", poll)
        self.assertIn("successorFilmMs - commitFilmMs", commit)
        self.assertIn("successorIsBuffered(bufferedThrough, commitFilmMs)", commit)
        self.assertNotIn("successor.seekTo", commit)

    def test_apple_prepared_enablement_is_visible_and_advisory(self) -> None:
        view = self.read("clients/apple/Sources/LiveTvDeveloperView.swift")
        section = view.split(
            'Section("Prepared quality handoff · advisory enablement")', 1
        )[1].split('Section("HDHomeRun Live TV · runtime enablement")', 1)[0]
        self.assertIn('Toggle("Enable two-player prepared handoff"', section)
        self.assertIn("Not met", section)
        self.assertIn("Checked during playback", section)
        self.assertNotIn(".disabled", section)


if __name__ == "__main__":
    unittest.main()
