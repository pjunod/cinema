"""The launcher against a fake plurx server and a fake mpv (plan M4 acceptance).

Each test runs the real launcher in-process on a virtual clock, so cadence is
exact: the fake server stamps every request with the virtual time.
"""

import json
import os
import signal
import subprocess
import sys
import time
import unittest
from unittest import mock

import support
from support import CODE, FILE_ID, ITEM_ID, TOKEN, LauncherCase, transcode_decision
from plurx_player import launcher as launcher_module
from plurx_player import playback as playback_module
from plurx_player.trust import TrustStore, zenity_confirm


def bearer(request):
    return request["headers"].get("Authorization")


class CallSequence(LauncherCase):
    def test_redeem_decide_create_play_then_release_and_revoke(self):
        self.close_at(12)
        self.assertEqual(self.run_launcher(), 0)
        s = self.server
        redeem = s.index("POST", r"/playback-handoffs/redeem$")
        decision = s.index("POST", rf"/files/{FILE_ID}/decision$")
        create = s.index("POST", rf"/files/{FILE_ID}/hls/sessions$")
        control = s.index("POST", r"/hls/[^/]+/control$")
        release = s.index("DELETE", r"^/api/v1/hls/[^/]+$")
        revoke = s.index("DELETE", r"/playback-handoffs/self$")
        self.assertTrue(0 == redeem < decision < create < control < release < revoke, s.requests)
        self.assertEqual(revoke, len(s.requests) - 1, "revoking the token is the last call")
        first = s.requests[redeem]
        self.assertIsNone(bearer(first), "the code is redeemed without a bearer")
        self.assertEqual(first["body"], {"code": CODE, "device": "plurx Player (Linux)"})
        self.assertEqual(s.requests[decision]["query"], {"force": ["auto"], "audio": ["1"]})
        for r in s.requests[1:]:
            capability = r["path"].startswith("/api/v1/hls/")
            self.assertEqual(bearer(r), None if capability else f"Bearer {TOKEN}", r["path"])
        create_body = s.requests[create]["body"]
        self.assertEqual(create_body["start"], 65.0)
        self.assertEqual(create_body["audio"], 1)
        self.assertTrue(create_body["quality_auto"])
        self.assertNotIn("height", create_body, "transcode Auto is the server's choice")
        self.assertTrue(create_body["playback_id"].startswith("plurx-player:"))
        self.assertEqual(create_body["caps"], s.requests[decision]["body"]["caps"])
        self.assertEqual(s.sessions[0]["session_id"], s.requests[release]["path"].rsplit("/", 1)[1])
        self.assertTrue(s.revoked)
        args = self.mpv.argv()
        for flag in ("--vo=gpu-next", "--gpu-api=vulkan", "--hwdec=auto-safe",
                     "--target-colorspace-hint=yes", "--idle=yes", "--keep-open=yes"):
            self.assertIn(flag, args)
        self.assertIn("--ytdl=no", args, "the ytdl hook never runs yt-dlp on a URL")
        self.assertIn("--osc=no", args, "only Cinema's controller draws playback controls")
        script = next(a.split("=", 1)[1] for a in args if a.startswith("--script="))
        self.assertTrue(os.path.isfile(script))
        self.assertTrue(script.endswith("plurx_player/scripts/cinema.lua"))
        self.assertIn(f"--config-dir={os.path.join(self.config, 'mpv')}", args,
                      "plurx Player's own mpv config, never ~/.config/mpv")
        self.assertNotIn("--no-config", args, "user mpv configuration remains supported")
        sock = next(a for a in args if a.startswith("--input-ipc-server="))
        self.assertTrue(sock.split("=", 1)[1].startswith(self.runtime))
        self.assertEqual(os.listdir(self.runtime), [], "the private socket directory is removed")

    def test_remux_asks_for_a_copy_session(self):
        self.server.decision = transcode_decision(method="remux", delivery={
            "mode": "remux", "url": f"/api/v1/files/{FILE_ID}/stream.mp4",
            "sessions_url": f"/api/v1/files/{FILE_ID}/hls/sessions", "aac": True,
            "preserve_dolby_vision": True})
        self.close_at(6)
        self.assertEqual(self.run_launcher(), 0)
        body = self.server.calls("POST", f"/api/v1/files/{FILE_ID}/hls/sessions")[0]["body"]
        self.assertEqual((body["copy"], body["aac"], body["preserve_dolby_vision"]), (True, True, True))
        control = self.server.calls("POST", pattern=r"/control$")[0]["body"]
        self.assertEqual(control["selection"]["quality"], {"mode": "original"})
        self.assertNotIn("supported_actions", control, "passive: the server may only answer none")


class Cadence(LauncherCase):
    def test_heartbeat_every_5_s_and_progress_every_10_s(self):
        self.close_at(31)
        self.assertEqual(self.run_launcher(), 0)
        controls = [r["at"] for r in self.server.calls("POST", pattern=r"/control$")]
        self.assertEqual([t for t in controls if t < 31], [0, 5, 10, 15, 20, 25, 30])
        progress = [r["at"] for r in self.server.calls("POST", f"/api/v1/items/{ITEM_ID}/progress")]
        self.assertEqual(progress[:3], [10, 20, 30])
        self.assertEqual(len(progress), 4, "and once more on exit")
        self.assertGreaterEqual(progress[3], 31)
        sequences = [r["body"]["sequence"] for r in self.server.calls("POST", pattern=r"/control$")]
        self.assertEqual(sequences, list(range(1, len(sequences) + 1)))

    def test_pause_sends_progress_and_holds_until_resumed(self):
        self.clock.at(12, lambda: self.mpv.emit({"event": "property-change", "name": "pause", "data": True}))
        self.clock.at(26, lambda: self.mpv.emit({"event": "property-change", "name": "pause", "data": False}))
        self.close_at(33)
        self.run_launcher()
        progress = [r["at"] for r in self.server.calls("POST", f"/api/v1/items/{ITEM_ID}/progress")]
        on_pause = [t for t in progress if 12 <= t < 14]
        self.assertEqual(len(on_pause), 1, progress)
        self.assertFalse([t for t in progress if 14 <= t < 26], "no beats while paused")
        self.assertIn(30, progress)
        held = [r["body"]["demand"] for r in self.server.calls("POST", pattern=r"/control$") if 15 <= r["at"] <= 25]
        self.assertEqual(set(held), {"hold"})


class Position(LauncherCase):
    def test_progress_and_control_are_mpv_time_plus_origin(self):
        self.mpv.props(time_pos=12.5)
        self.close_at(11)
        self.run_launcher()
        commands = self.mpv.commands()
        self.assertIn(["set_property", "start", "5.000"], commands, "65 s resume - 60 s origin")
        beat = self.server.calls("POST", f"/api/v1/items/{ITEM_ID}/progress")[0]["body"]
        self.assertEqual(beat["position_ms"], 72_500)
        self.assertEqual(beat["duration_ms"], 7_200_000)
        self.assertEqual(beat["method"], "transcode")
        control = self.server.calls("POST", pattern=r"/control$")[-1]["body"]
        self.assertEqual(control["position_ms"], 72_500)
        self.assertEqual(control["buffered_through_ms"], 80_500)
        self.assertEqual(control["render_state"], "rendering")


class ControlContract(LauncherCase):
    def test_the_first_control_exchange_is_the_one_the_server_validates(self):
        # tests/fixtures/control-request-v1.json is also parsed and validated by
        # the server's own ControlRequestV1 (crates/plurxd/src/playback_control/
        # tests.rs), so a body the server would refuse cannot pass here. The
        # first exchange must carry capabilities: without them the server
        # answers 400 and every later exchange 409 (the launcher shipped so).
        self.close_at(8)
        self.run_launcher()
        first = self.server.calls("POST", pattern=r"/control$")[0]["body"]
        with open(os.path.join(support.PACKAGE_ROOT, "tests", "fixtures", "control-request-v1.json")) as handle:
            fixture = json.load(handle)
        self.assertEqual(first["sequence"], 1)
        ids = {key: fixture[key] for key in ("client_instance_id", "generation")}  # per-run UUIDs
        # The first heartbeat races mpv's first playback-restart; the server
        # validates both states, and the fixture carries one of them.
        self.assertIn(first["render_state"], ("starting", "rendering"))
        ids["render_state"] = fixture["render_state"]
        self.maxDiff = None
        self.assertEqual({**first, **ids}, fixture)


class Seeking(LauncherCase):
    def test_seek_outside_the_range_recreates_the_session(self):
        self.server.origins = [60_000, 559_000]
        self.clock.at(6, lambda: self.mpv.seek(500.0))
        self.close_at(30)
        self.assertEqual(self.run_launcher(), 0)
        s = self.server
        first, second = s.sessions[0]["session_id"], s.sessions[1]["session_id"]
        release_first = s.index("DELETE", rf"/hls/{first}$")
        creates = s.calls("POST", f"/api/v1/files/{FILE_ID}/hls/sessions")
        self.assertEqual(len(creates), 2)
        self.assertLess(release_first, s.requests.index(creates[1]), "the old session goes first")
        self.assertEqual(creates[1]["body"]["start"], 560.0, "60 s origin + 500 s into the session")
        self.assertEqual(creates[1]["body"]["playback_id"], creates[0]["body"]["playback_id"])
        self.assertNotEqual(creates[1]["body"]["request_id"], creates[0]["body"]["request_id"])
        commands = self.mpv.commands()
        reload_at = commands.index(["loadfile", f"{s.origin}/api/v1/hls/{second}/index.m3u8", "replace"])
        self.assertEqual(commands[reload_at - 1], ["set_property", "start", "1.000"])
        self.assertIn(["set_property", "pause", True], commands[:reload_at])
        self.assertIn(["set_property", "pause", False], commands[reload_at:])
        after = [r for r in s.calls("POST", f"/api/v1/items/{ITEM_ID}/progress") if r["at"] >= 6]
        self.assertEqual(after[0]["body"]["position_ms"], 560_000, "the new origin is taken")
        later = [r for r in s.calls("POST", pattern=r"/control$") if r["at"] > 6]
        self.assertTrue(later and all(r["path"] == f"/api/v1/hls/{second}/control" for r in later))
        self.assertEqual(s.index("DELETE", rf"/hls/{second}$") > release_first, True)

    def test_seek_inside_the_range_stays_in_the_session(self):
        real_loop = playback_module.Playback.loop

        def loop(playback):
            def seek():
                # Exercise seek semantics on virtual time; the fake mpv's
                # separate wall-clock event producer is covered elsewhere.
                self.mpv.props(time_pos=20.0)
                self.assertEqual(playback.mpv.get("time-pos"), 20.0)
                self.assertTrue(playback.on_event({"event": "seek"}))
                self.assertTrue(playback.on_event({"event": "playback-restart"}))

            self.clock.at(6, seek)
            return real_loop(playback)

        self.close_at(14)
        with mock.patch.object(playback_module.Playback, "loop", loop):
            self.run_launcher()
        self.assertEqual(len(self.server.sessions), 1)
        self.assertEqual(len(self.server.calls("DELETE", "/api/v1/hls/")), 1, "only the final release")
        seek_beat = [r for r in self.server.calls("POST", f"/api/v1/items/{ITEM_ID}/progress") if 6 <= r["at"] < 8]
        self.assertEqual(len(seek_beat), 1, "one immediate progress report on seek")
        self.assertEqual(seek_beat[0]["body"]["position_ms"], 80_000, "progress on seek")


class ServerStop(LauncherCase):
    def check_stop(self, status):
        self.server.control_status = status
        self.server.control_error = {"code": "session_gone", "message": "Stopped from Activity"}
        self.assertEqual(self.run_launcher(), 0)
        commands = self.mpv.commands()
        osd = commands.index(["show-text", "Stopped from Activity", 3000])
        self.assertIn(["quit"], commands[osd:])
        self.assertLess(self.clock.now, 10, "stopped within one heartbeat, after the 3 s OSD")
        self.assertTrue(self.server.calls("DELETE", "/api/v1/hls/"))
        self.assertTrue(self.server.revoked)

    def test_control_410_stops_playback_with_the_reason(self):
        self.check_stop(410)

    def test_control_404_stops_playback_with_the_reason(self):
        self.check_stop(404)


class Teardown(LauncherCase):
    def assert_released(self):
        self.assertTrue(self.server.calls("DELETE", "/api/v1/hls/"), "session released")
        self.assertTrue(self.server.revoked, "handoff token revoked")
        self.assertEqual(os.listdir(self.runtime), [])

    def test_viewer_closing_mpv(self):
        self.close_at(4)
        self.assertEqual(self.run_launcher(), 0)
        self.assert_released()

    def test_mpv_crash(self):
        self.clock.at(12, self.mpv.crash)
        self.assertEqual(self.run_launcher(), 5)
        self.assert_released()
        failed = [r["body"] for r in self.server.calls("POST", "/api/v1/client-log")
                  if r["body"]["event"] == "playback_failed"]
        self.assertEqual(failed[0]["message"], "mpv exited with status 9")

    def test_launcher_crash(self):
        real = playback_module.Playback.heartbeat
        calls = []

        def exploding(self_):
            calls.append(1)
            if len(calls) == 2:
                raise RuntimeError("boom")
            return real(self_)

        with mock.patch.object(playback_module.Playback, "heartbeat", exploding):
            with self.assertRaises(RuntimeError):
                self.run_launcher()
        self.assert_released()
        self.assertIn(["quit"], self.mpv.commands())
        failed = [r["body"] for r in self.server.calls("POST", "/api/v1/client-log")
                  if r["body"]["event"] == "playback_failed"]
        self.assertEqual(failed[0]["message"], "plurx Player crashed: RuntimeError")


class Signals(LauncherCase):
    def test_sigterm_releases_the_session_and_revokes_the_token(self):
        env = dict(os.environ, XDG_CONFIG_HOME=os.path.dirname(self.config),
                   XDG_RUNTIME_DIR=self.runtime, PATH=self.bin + os.pathsep + os.environ.get("PATH", ""))
        launcher = os.path.join(support.PACKAGE_ROOT, "plurx-player")
        proc = subprocess.Popen([sys.executable, "-I", launcher, self.server.url(), "--mpv", self.wrapper],
                                env=env, stderr=subprocess.PIPE)
        try:
            deadline = time.monotonic() + 15
            while not self.server.calls("POST", pattern=r"/control$"):
                self.assertIsNone(proc.poll(), proc.stderr.read().decode() if proc.poll() is not None else "")
                self.assertLess(time.monotonic(), deadline, "the launcher never started playing")
                time.sleep(0.05)
            proc.send_signal(signal.SIGTERM)
            self.assertEqual(proc.wait(timeout=15), 128 + signal.SIGTERM)
        finally:
            if proc.poll() is None:
                proc.kill()
            proc.stderr.close()
        self.assertTrue(self.server.calls("DELETE", "/api/v1/hls/"))
        self.assertTrue(self.server.revoked)
        self.assertIn(["quit"], self.mpv.commands())
        self.assertEqual(os.listdir(self.runtime), [])
        # Run as installed, the mpv config directory follows XDG_CONFIG_HOME.
        self.assertIn(f"--config-dir={os.path.join(self.config, 'mpv')}", self.mpv.argv())


class Trust(LauncherCase):
    trusted = False

    def test_an_unknown_origin_is_not_redeemed_without_confirmation(self):
        self.assertEqual(self.run_launcher(), 3)
        self.assertEqual(self.confirms, [self.server.origin])
        self.assertEqual(self.server.requests, [], "nothing was contacted")
        self.assertFalse(TrustStore(self.config).is_trusted(self.server.origin))

    def test_a_confirmed_origin_plays_and_is_not_asked_again(self):
        self.close_at(3)
        self.assertEqual(self.run_launcher(confirm=lambda origin: True), 0)
        self.assertTrue(TrustStore(self.config).is_trusted(self.server.origin))
        self.server.redeemed, self.server.revoked = False, False
        self.mpv.reset()
        self.clock.at(self.clock.now + 3, self.mpv.close)
        self.assertEqual(self.run_launcher(confirm=lambda origin: self.fail("asked again")), 0)

    def test_the_zenity_question_names_the_origin_and_warns_about_http(self):
        log = os.path.join(self.tmp, "zenity.log")
        zenity = os.path.join(self.bin, "zenity")
        with open(zenity, "w", encoding="utf-8") as handle:
            handle.write(f'#!/bin/sh\nprintf "%s\\n" "$@" > "{log}"\nexit 1\n')
        os.chmod(zenity, 0o755)
        with mock.patch.dict(os.environ, {"PATH": self.bin + os.pathsep + os.environ.get("PATH", "")}):
            self.assertEqual(self.run_launcher(confirm=zenity_confirm), 3)
        with open(log, encoding="utf-8") as handle:
            asked = handle.read()
        self.assertIn("--question", asked)
        self.assertIn(self.server.origin, asked)
        self.assertIn("plain http", asked)
        self.assertEqual(self.server.requests, [])

    def test_malformed_urls_are_refused_before_anything_else(self):
        for url in ("plurx-player://play?server=http%3A%2F%2Fx&code=" + CODE + "&extra=1",
                    "https://play?server=x&code=" + CODE, "plurx-player://play?code=" + CODE):
            with self.subTest(url=url):
                self.assertEqual(self.run_launcher(url=url), 2)
        self.assertEqual(self.confirms, [])
        self.assertEqual(self.server.requests, [])
        self.assertEqual(len(self.errors), 3)


class Redeem(LauncherCase):
    def test_an_expired_or_used_code_stops_at_redeem(self):
        self.server.redeem_status = 410
        self.assertEqual(self.run_launcher(), 4)
        self.assertEqual([r["path"] for r in self.server.requests], ["/api/v1/playback-handoffs/redeem"])
        self.assertIn("expired or was already used", self.errors[0])

    def test_a_server_answering_as_another_origin_is_refused_and_revoked(self):
        self.server.redeem_body["origin"] = "http://elsewhere.example:8096"
        told = []
        self.assertEqual(self.run_launcher(error=lambda message: told.append(self.server.revoked)), 5)
        self.assertEqual(told, [True], "the token is revoked before the dialog waits on the viewer")
        self.assertFalse(self.server.calls("POST", f"/api/v1/files/{FILE_ID}/decision"))
        self.assertTrue(self.server.revoked)


class Delivery(LauncherCase):
    def test_direct_play_loads_the_file_with_the_bearer_over_ipc_only(self):
        self.server.decision = transcode_decision(method="direct_play", delivery={
            "mode": "direct", "url": f"/api/v1/files/{FILE_ID}/direct"})
        self.mpv.props(time_pos=70.0)
        self.close_at(11)
        self.assertEqual(self.run_launcher(), 0)
        commands = self.mpv.commands()
        self.assertIn(["set_property", "http-header-fields", [f"Authorization: Bearer {TOKEN}"]], commands)
        self.assertIn(["set_property", "start", "65.000"], commands)
        self.assertIn(["loadfile", f"{self.server.origin}/api/v1/files/{FILE_ID}/direct", "replace"], commands)
        self.assertIn(["set_property", "aid", 2], commands, "server audio index 1 is mpv's second track")
        self.assertFalse(self.server.sessions)
        self.assertFalse(self.server.calls("POST", pattern=r"/control$"), "no session, no heartbeat")
        beat = self.server.calls("POST", f"/api/v1/items/{ITEM_ID}/progress")[0]["body"]
        self.assertEqual(beat["position_ms"], 70_000)

    def test_text_subtitle_loads_as_a_shifted_sidecar(self):
        self.server.redeem_body["subtitle"] = 2
        self.close_at(4)
        self.run_launcher()
        commands = self.mpv.commands()
        self.assertIn(["sub-add", f"{self.server.origin}/api/v1/files/{FILE_ID}/subs/2", "select", "plurx"], commands)
        self.assertIn(["set_property", "sub-delay", -60.0], commands, "absolute cues on a session timeline")
        self.assertEqual(self.server.calls("POST", f"/api/v1/files/{FILE_ID}/decision")[0]["query"]["subtitle"], ["2"])

    def test_image_subtitle_burns_and_falls_back_when_hdr_refuses(self):
        self.server.redeem_body["subtitle"] = 3
        self.server.create_status = [422]
        self.close_at(4)
        self.run_launcher()
        creates = self.server.calls("POST", f"/api/v1/files/{FILE_ID}/hls/sessions")
        self.assertEqual(creates[0]["body"]["subtitle_burn"], 3)
        self.assertNotIn("subtitle_burn", creates[1]["body"])
        self.assertIn(["show-text", "This image subtitle cannot be burned into HDR; playing without it", 4000],
                      self.mpv.commands())


class PolicyDefaults(LauncherCase):
    """A picker left on Default (null in the handoff) takes the decision's own choice."""

    def direct(self, **extra):
        self.server.decision = transcode_decision(method="direct_play", delivery={
            "mode": "direct", "url": f"/api/v1/files/{FILE_ID}/direct"}, **extra)

    def test_default_audio_is_selected_for_direct_play(self):
        self.server.redeem_body.update(audio=None, subtitle=None)
        self.direct(audio=[{"index": 0, "default": False}, {"index": 1, "default": True}])
        self.close_at(4)
        self.run_launcher()
        self.assertIn(["set_property", "aid", 2], self.mpv.commands())
        self.assertNotIn("audio", self.server.calls("POST", f"/api/v1/files/{FILE_ID}/decision")[0]["query"])

    def test_default_text_subtitle_loads_as_a_sidecar(self):
        self.server.redeem_body.update(audio=None, subtitle=None)
        self.server.decision = transcode_decision(subtitles=[
            {"index": 2, "text": True, "default": True}, {"index": 3, "text": False}])
        self.close_at(4)
        self.run_launcher()
        commands = self.mpv.commands()
        self.assertIn(["sub-add", f"{self.server.origin}/api/v1/files/{FILE_ID}/subs/2", "select", "plurx"], commands)
        create = self.server.calls("POST", f"/api/v1/files/{FILE_ID}/hls/sessions")[0]["body"]
        self.assertNotIn("audio", create, "an untouched audio picker is not sent back")
        self.assertNotIn("subtitle_burn", create)

    def test_selection_echo_names_the_policy_subtitle_when_audio_was_picked(self):
        self.server.redeem_body.update(audio=1, subtitle=None)
        self.server.decision = transcode_decision(
            subtitles=[{"index": 2, "text": True, "default": False}],
            selection={"audio_index": 1, "subtitle_index": 2, "subtitle_requires_burn_in": False,
                       "subtitle_burn_in_blocked_by_hdr": False})
        self.close_at(4)
        self.run_launcher()
        self.assertIn(["sub-add", f"{self.server.origin}/api/v1/files/{FILE_ID}/subs/2", "select", "plurx"],
                      self.mpv.commands())

    def test_a_bitmap_default_and_an_explicit_off_load_nothing(self):
        for subtitle, tracks in ((None, [{"index": 3, "text": False, "default": True}]),
                                 (-1, [{"index": 2, "text": True, "default": True}])):
            with self.subTest(subtitle=subtitle):
                self.server.redeemed, self.server.revoked = False, False
                self.server.redeem_body.update(audio=None, subtitle=subtitle)
                self.server.decision = transcode_decision(subtitles=tracks)
                self.mpv.reset()
                before = len(self.server.calls("POST", f"/api/v1/files/{FILE_ID}/hls/sessions"))
                self.clock.at(self.clock.now + 4, self.mpv.close)
                self.run_launcher()
                self.assertFalse([c for c in self.mpv.commands() if c[0] == "sub-add"])
                create = self.server.calls("POST", f"/api/v1/files/{FILE_ID}/hls/sessions")[before]["body"]
                self.assertNotIn("subtitle_burn", create)


class SignalDuringRedeem(LauncherCase):
    def test_sigterm_while_redeeming_still_revokes_the_issued_token(self):
        # The server issues the token and the signal lands before the answer
        # is read: without the gate, Terminated would be raised inside the
        # redeem call and the live token would never be revoked.
        route = self.server.route

        def signalling(method, path, headers, body):
            answer = route(method, path, headers, body)
            if path.endswith("/redeem"):
                os.kill(os.getpid(), signal.SIGTERM)
            return answer

        self.server.route = signalling
        deps = launcher_module.Deps(confirm=self.refuse, error=self.errors.append, clock=self.clock,
                                    sleep=self.clock.sleep, config_dir=self.config,
                                    runtime_dir=self.runtime, install_signals=True)
        previous = signal.getsignal(signal.SIGTERM)
        code = launcher_module.main([self.server.url(), "--mpv", self.wrapper], deps)
        self.assertIs(signal.getsignal(signal.SIGTERM), previous, "handlers are restored")
        self.assertEqual(code, 128 + signal.SIGTERM)
        self.assertTrue(self.server.redeemed)
        self.assertTrue(self.server.revoked, "the token issued during the signal is revoked")
        self.assertFalse(self.server.calls("POST", f"/api/v1/files/{FILE_ID}/decision"))


class Mapper(LauncherCase):
    def test_one_client_log_line_per_playback_with_profile_mapper_and_target(self):
        self.server.origins = [60_000, 559_000]
        self.mpv.props(track_list=[{"type": "video", "selected": True, "dolby-vision-profile": 8}],
                       video_target_params={"max-luma": 616, "gamma": "pq", "primaries": "bt.2020"})
        self.clock.at(2, lambda: self.mpv.emit({"event": "log-message", "prefix": "vo/gpu-next/libplacebo",
                                                "level": "info", "text": "plurx mapper: closed-form CM v4.0\n"}))
        self.clock.at(15, lambda: self.mpv.seek(500.0))
        self.close_at(31)
        self.run_launcher()
        lines = [r["body"] for r in self.server.calls("POST", "/api/v1/client-log")
                 if r["body"]["event"] == "native_mapper"]
        self.assertEqual(len(lines), 1, "one line per playback, not per session")
        line = lines[0]
        self.assertEqual(line["message"],
                         "plurx Player (Linux): DV profile 8, mapper closed-form CM v4.0, target 616 nits")
        self.assertEqual(json.loads(line["detail"])["target_nits"], 616)
        self.assertEqual(line["level"], "warn", "ClientLog has only error and WARN; info would be logged as WARN")
        self.assertEqual(line["file_id"], FILE_ID)
        self.assertIn(["show-text", "DV profile 8, mapper closed-form CM v4.0, target 616 nits", 4000],
                      self.mpv.commands())

    def test_an_unreported_mapper_is_logged_as_unknown_even_for_a_short_playback(self):
        self.close_at(4)
        self.run_launcher()
        lines = [r["body"] for r in self.server.calls("POST", "/api/v1/client-log")
                 if r["body"]["event"] == "native_mapper"]
        self.assertEqual([l["message"] for l in lines],
                         ["plurx Player (Linux): DV profile none, mapper unknown, target unknown"])


class NoTokenOnDisk(LauncherCase):
    def test_mpv_gets_no_dynamic_loader_overrides(self):
        # LD_LIBRARY_PATH would beat the private mpv's rpath and pick up the
        # system libplacebo; LD_PRELOAD and LD_AUDIT inject code.
        self.close_at(6)
        loader = {"LD_LIBRARY_PATH": "/usr/lib/x86_64-linux-gnu", "LD_PRELOAD": "", "LD_AUDIT": ""}
        with mock.patch.dict(os.environ, loader):
            self.assertEqual(self.run_launcher(), 0)
        self.assertEqual(self.mpv.env(), {})

    def test_the_token_never_reaches_a_file_or_mpv_argv(self):
        self.server.decision = transcode_decision(method="direct_play", delivery={
            "mode": "direct", "url": f"/api/v1/files/{FILE_ID}/direct"})
        self.close_at(4)
        self.assertEqual(self.run_launcher(), 0)
        self.assertNotIn(TOKEN, json.dumps(self.mpv.argv()))
        for directory in (os.path.dirname(self.config), self.runtime):
            for parent, _dirs, files in os.walk(directory):
                for name in files:
                    with open(os.path.join(parent, name), "rb") as handle:
                        self.assertNotIn(TOKEN.encode(), handle.read(), name)
        # The only place it went is the IPC socket, as a header field.
        carried = [c for c in self.mpv.commands() if TOKEN in json.dumps(c)]
        self.assertEqual(carried, [["set_property", "http-header-fields", [f"Authorization: Bearer {TOKEN}"]]])


DISPLAY_VARS = ("WAYLAND_DISPLAY", "DISPLAY", "XAUTHORITY", "XDG_SESSION_TYPE", "XDG_CURRENT_DESKTOP")
SESSION = ("PATH=/home/someone/bin:/usr/bin\nWAYLAND_DISPLAY=wayland-0\nDISPLAY=:0\n"
           "XAUTHORITY=/run/user/1000/.mutter-Xwaylandauth.ABC\nXDG_SESSION_TYPE=wayland\n"
           "XDG_CURRENT_DESKTOP=ubuntu:GNOME\nLD_PRELOAD=/home/someone/evil.so\n")


class SessionDisplay(LauncherCase):
    """Started by xdg-desktop-portal without a display, the launcher takes the session's."""

    def run_without_display(self, session):
        self.close_at(4)
        bare = {key: value for key, value in os.environ.items() if key not in DISPLAY_VARS}
        with mock.patch.dict(os.environ, bare, clear=True):
            code = self.run_launcher(session_environment=session)
        return code

    def test_a_portal_started_launcher_gives_mpv_the_sessions_display(self):
        self.assertEqual(self.run_without_display(SESSION), 0)
        self.assertEqual(self.mpv.display(), {
            "WAYLAND_DISPLAY": "wayland-0", "DISPLAY": ":0",
            "XAUTHORITY": "/run/user/1000/.mutter-Xwaylandauth.ABC",
            "XDG_SESSION_TYPE": "wayland", "XDG_CURRENT_DESKTOP": "ubuntu:GNOME"})
        self.assertEqual(self.mpv.env(), {}, "only display variables are taken from the session")
        self.assertNotIn("/home/someone/bin", os.environ.get("PATH", ""))

    def test_a_launcher_with_a_display_keeps_its_own(self):
        environ = {"WAYLAND_DISPLAY": "wayland-1"}
        self.assertEqual(launcher_module.adopt_session_display(environ, lambda: SESSION), [])
        self.assertEqual(environ, {"WAYLAND_DISPLAY": "wayland-1"})

    def test_a_session_without_a_display_changes_nothing(self):
        environ = {}
        self.assertEqual(launcher_module.adopt_session_display(
            environ, lambda: "XDG_SESSION_TYPE=tty\nXAUTHORITY=/x\n"), [])
        self.assertEqual(environ, {})

    def test_the_session_display_is_taken_as_a_set(self):
        environ = {"XAUTHORITY": "/stale/Xauthority", "XDG_SESSION_TYPE": "tty", "HOME": "/home/someone"}
        launcher_module.adopt_session_display(environ, lambda: SESSION)
        self.assertEqual(environ["XAUTHORITY"], "/run/user/1000/.mutter-Xwaylandauth.ABC")
        self.assertEqual(environ["XDG_SESSION_TYPE"], "wayland")
        self.assertEqual(environ["HOME"], "/home/someone")

    def test_an_escaped_value_is_not_taken(self):
        environ = {}
        launcher_module.adopt_session_display(environ, lambda: "DISPLAY=$'\\033:0'\nWAYLAND_DISPLAY=wayland-0\n")
        self.assertEqual(environ, {"WAYLAND_DISPLAY": "wayland-0"})


class VideoWindow(LauncherCase):
    def test_a_title_with_video_but_no_window_fails_instead_of_playing_audio_only(self):
        # What mpv really does without a display: it deselects the video
        # track and goes on with the audio.
        self.mpv.props(track_list=[{"type": "video", "selected": False, "albumart": False},
                                   {"type": "audio", "selected": True}], vo_configured=False)
        self.clock.at(30, self.mpv.close)
        self.assertEqual(self.run_launcher(), launcher_module.EXIT_PLAYBACK)
        self.assertEqual(self.errors, ["plurx Player: mpv could not open a video window: "
                                       "is a graphical session running?"])
        failures = [r["body"] for r in self.server.calls("POST", "/api/v1/client-log")
                    if r["body"]["event"] == "playback_failed"]
        self.assertEqual(len(failures), 1)
        self.assertTrue(self.server.revoked)

    def test_mpvs_vo_error_line_is_a_failure_too(self):
        self.mpv.props(track_list=[{"type": "video", "selected": False}])
        self.mpv.emit({"event": "log-message", "prefix": "cplayer", "level": "fatal",
                       "text": "Error opening/initializing the selected video_out (--vo) device.\n"})
        self.clock.at(30, self.mpv.close)
        self.assertEqual(self.run_launcher(), launcher_module.EXIT_PLAYBACK)
        self.assertEqual(len(self.errors), 1)

    def test_an_audio_file_with_cover_art_plays_without_a_window(self):
        self.mpv.props(track_list=[{"type": "video", "selected": False, "albumart": True},
                                   {"type": "audio", "selected": True}], vo_configured=False)
        self.close_at(4)
        self.assertEqual(self.run_launcher(), 0)
        self.assertEqual(self.errors, [])

    def test_a_title_with_a_window_plays(self):
        self.mpv.props(track_list=[{"type": "video", "selected": True}], vo_configured=True)
        self.close_at(4)
        self.assertEqual(self.run_launcher(), 0)
        self.assertEqual(self.errors, [])



class FilmTimeline(LauncherCase):
    """A live session shows the whole film on mpv's OSC (mpv patch 0005)."""

    def film_settings(self):
        return [c for c in self.mpv.commands()
                if c[:2] == ["set_property", "user-data/osc/film"] or c == ["del", "user-data/osc/film"]]

    def test_a_live_session_tells_the_osc_the_film_timeline(self):
        self.close_at(4)
        self.assertEqual(self.run_launcher(), 0)
        self.assertEqual(self.film_settings(),
                         [["set_property", "user-data/osc/film", {"origin": 60.0, "duration": 7200.0}]])
        self.assertIn("--force-seekable=yes", self.mpv.argv())

    def test_a_vod_session_has_no_film_timeline(self):
        self.server.vod = True
        self.close_at(4)
        self.assertEqual(self.run_launcher(), 0)
        self.assertEqual(self.film_settings(), [["del", "user-data/osc/film"]])

    def test_a_film_seek_outside_the_stream_recreates_the_session_there(self):
        self.server.origins = [60_000, 3_000_000]
        self.clock.at(6, lambda: self.mpv.emit({"event": "client-message", "args": ["osc-film-seek", "3000.5"]}))
        self.close_at(30)
        self.assertEqual(self.run_launcher(), 0)
        creates = self.server.calls("POST", f"/api/v1/files/{FILE_ID}/hls/sessions")
        self.assertEqual(len(creates), 2)
        self.assertEqual(creates[1]["body"]["start"], 3000.5)
        self.assertEqual(self.film_settings()[-1],
                         ["set_property", "user-data/osc/film", {"origin": 3000.0, "duration": 7200.0}])

    def test_a_malformed_film_seek_is_ignored(self):
        self.clock.at(6, lambda: self.mpv.emit({"event": "client-message", "args": ["osc-film-seek", "nan"]}))
        self.close_at(12)
        self.assertEqual(self.run_launcher(), 0)
        self.assertEqual(len(self.server.calls("POST", f"/api/v1/files/{FILE_ID}/hls/sessions")), 1)


if __name__ == "__main__":
    unittest.main()
