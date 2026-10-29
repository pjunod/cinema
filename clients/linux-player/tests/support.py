"""Fakes for the launcher tests: a plurx server, mpv's files, and a clock.

The fake server checks what the real one would refuse: a bearer on the
redeem call, a missing or revoked bearer on token routes, and any control
field outside ControlRequestV1 (crates/plurxd/src/playback_control.rs is
deny_unknown_fields).
"""

import json
import os
import re
import sys
import tempfile
import threading
import time
import unittest
import urllib.parse
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
PACKAGE_ROOT = os.path.dirname(HERE)
if PACKAGE_ROOT not in sys.path:
    sys.path.insert(0, PACKAGE_ROOT)

from plurx_player.launcher import Deps, main  # noqa: E402
from plurx_player.playback import Timing  # noqa: E402
from plurx_player.trust import TrustStore  # noqa: E402

FAKE_MPV = os.path.join(HERE, "fake_mpv.py")
TOKEN = "tok-SECRET-5f2b9c1e7a"
CODE = "Q2xhdWRlLWhhbmRvZmYtY29kZS0zMi1ieXRlcy1sb25n"
FILE_ID = 7
ITEM_ID = 70
PROTOCOL = "plurx-playback-control-v1"
CONTROL_FIELDS = {
    "protocol", "generation", "control_epoch", "client_instance_id", "sequence", "demand",
    "position_ms", "buffered_from_ms", "buffered_through_ms", "playback_rate", "render_state",
    "seek_target_ms", "observed_download_bps", "selection", "capabilities", "observation",
    "acknowledgement", "supported_actions", "intent"}
CONTROL_REQUIRED = {
    "protocol", "generation", "control_epoch", "client_instance_id", "sequence", "demand",
    "position_ms", "buffered_through_ms", "playback_rate", "render_state", "selection"}
SELECTION_FIELDS = {"quality", "audio_track", "subtitle", "audio_offset_ms", "codec", "dynamic_range"}


def transcode_decision(**extra):
    decision = {"method": "transcode", "reasons": ["video codec"],
                "delivery": {"mode": "transcode", "sessions_url": f"/api/v1/files/{FILE_ID}/hls/sessions"},
                "source": {"duration_ms": 7_200_000}, "audio": [{"index": 0}, {"index": 1}],
                "subtitles": [{"index": 2, "text": True}, {"index": 3, "text": False}]}
    decision.update(extra)
    return decision


class FakePlurx:
    def __init__(self):
        self.lock = threading.Lock()
        self.requests = []
        self.clock = lambda: None
        self.code = CODE
        self.redeemed = False
        self.revoked = False
        self.redeem_status = 200
        self.redeem_body = {"file_id": FILE_ID, "item_id": ITEM_ID, "start_ms": 65_000, "audio": 1,
                            "subtitle": None, "user": "viewer"}
        self.context_status = 200
        self.current_file_id, self.current_item_id = FILE_ID, ITEM_ID
        self.context_data = {"file_id": FILE_ID, "item_id": ITEM_ID, "start_ms": 0,
                             "metadata": {"title": "Fixture", "kind": "movie", "year": 2026,
                                          "overview": "A library synopsis.", "runtime_ms": 7_200_000},
                             "next": None}
        self.next_context = None
        self.next_status = 200
        self.decision = transcode_decision()
        self.sessions = []
        self.origins = [60_000]
        self.vod = False
        self.published_end_ms = 30_000
        self.control_status = 200
        self.control_error = None
        self.create_status = []
        self.httpd = ThreadingHTTPServer(("127.0.0.1", 0), _Handler)
        self.httpd.daemon_threads = True
        self.httpd.fake = self
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()
        self.redeem_body["origin"] = self.origin

    @property
    def origin(self):
        return f"http://127.0.0.1:{self.httpd.server_address[1]}"

    def url(self, code=None):
        return (f"plurx-player://play?server={urllib.parse.quote(self.origin, safe='')}"
                f"&code={code or self.code}")

    def stop(self):
        self.httpd.shutdown()
        self.httpd.server_close()

    def calls(self, method=None, prefix=None, pattern=None):
        with self.lock:
            found = list(self.requests)
        return [r for r in found if (method is None or r["method"] == method)
                and (prefix is None or r["path"].startswith(prefix))
                and (pattern is None or re.search(pattern, r["path"]))]

    def index(self, method, path_re, start=0):
        with self.lock:
            found = list(self.requests)
        for at in range(start, len(found)):
            if found[at]["method"] == method and re.search(path_re, found[at]["path"]):
                return at
        return -1

    # -- routes -------------------------------------------------------------

    def route(self, method, path, headers, body):
        auth = headers.get("Authorization")
        authed = auth == f"Bearer {TOKEN}" and not self.revoked
        if path == "/api/v1/playback-handoffs/redeem" and method == "POST":
            if auth:
                return 400, {"error": "redeem takes no bearer"}
            if self.redeem_status != 200:
                return self.redeem_status, {"code": "handoff_gone", "message": "expired or already used"}
            if not isinstance(body, dict) or body.get("code") != self.code or self.redeemed:
                return 410, {"code": "handoff_gone", "message": "expired or already used"}
            if body.get("device") != "plurx Player (Linux)":
                return 400, {"error": "device"}
            self.redeemed = True
            return 200, dict(self.redeem_body, token=TOKEN)
        session = re.match(r"^/api/v1/hls/([^/]+)(?:/(control|status))?$", path)
        if session:
            if auth:
                return 400, {"error": "capability routes take no bearer in these tests"}
            sid, leaf = session.groups()
            known = next((s for s in self.sessions if s["session_id"] == sid), None)
            if method == "DELETE" and leaf is None:
                return 204, None
            if known is None:
                return 404, {"code": "session_gone", "message": "no such session"}
            if leaf == "status" and method == "GET":
                return 200, {"producer_state": "running", "published_end_ms": self.published_end_ms}
            if leaf == "control" and method == "POST":
                return self.control(known, body)
            return 405, {"error": "method"}
        if not authed:
            return 401, {"error": "authentication required"}
        if path == "/api/v1/playback-handoffs/self/context" and method == "GET":
            return self.context_status, self.context_data
        if path == "/api/v1/playback-handoffs/self/next" and method == "POST":
            if body != {"expected_file_id": self.current_file_id}:
                return 409, {"message": "stale predecessor"}
            if self.next_status != 200:
                return self.next_status, {"message": "next refused"}
            if not self.next_context:
                return 404, {"message": "no next episode"}
            self.context_data = self.next_context
            self.next_context = None
            self.current_file_id = self.context_data["file_id"]
            self.current_item_id = self.context_data["item_id"]
            return 200, self.context_data
        if path == "/api/v1/playback-handoffs/self" and method == "DELETE":
            self.revoked = True
            return 204, None
        if path == f"/api/v1/files/{self.current_file_id}/decision" and method == "POST":
            if not isinstance(body, dict) or set(body) != {"caps"} or body["caps"].get("v") != 2:
                return 400, {"error": "caps document v2 expected"}
            payload = json.dumps(self.decision).replace(f"/files/{FILE_ID}/", f"/files/{self.current_file_id}/")
            return 200, json.loads(payload)
        if path == f"/api/v1/files/{self.current_file_id}/hls/sessions" and method == "POST":
            if self.create_status:
                status = self.create_status.pop(0)
                if status != 200:
                    return status, {"code": "hdr_subtitle_burn_refused", "message": "refused"}
            n = len(self.sessions) + 1
            sid = f"00000000-0000-4000-8000-{n:012d}"
            created = {"session_id": sid, "playlist_url": f"/api/v1/hls/{sid}/index.m3u8",
                       "media_origin_ms": self.origins[min(n, len(self.origins)) - 1],
                       "start_seconds": body.get("start"), "duration_ms": 7_200_000, "vod": self.vod,
                       "control": {"protocol": PROTOCOL, "url": f"/api/v1/hls/{sid}/control",
                                   "generation": str(uuid.uuid4()), "control_epoch": 1,
                                   "next_exchange_ms": 5000, "lease_timeout_ms": 60000}}
            self.sessions.append(created)
            return 200, created
        if path == f"/api/v1/items/{self.current_item_id}/progress" and method == "POST":
            return 200, {"position_ms": body.get("position_ms")}
        if path == "/api/v1/client-log" and method == "POST":
            return 204, None
        return 404, {"error": f"{method} {path} not found"}

    def control(self, session, body):
        if not isinstance(body, dict) or not CONTROL_REQUIRED <= set(body) or not set(body) <= CONTROL_FIELDS:
            return 400, {"code": "invalid_control", "invalid_field": sorted(set(body or {}) ^ CONTROL_REQUIRED)}
        if body["protocol"] != PROTOCOL or body["generation"] != session["control"]["generation"]:
            return 409, {"code": "stale_control"}
        if not set(body["selection"]) <= SELECTION_FIELDS:
            return 400, {"code": "invalid_control", "invalid_field": "selection"}
        if body["render_state"] != "seeking" and body["buffered_through_ms"] < body["position_ms"]:
            return 400, {"code": "invalid_control", "invalid_field": "buffered_through_ms"}
        if self.control_status != 200:
            return self.control_status, self.control_error or {"code": "session_gone", "message": "gone"}
        return 200, {"protocol": PROTOCOL, "generation": body["generation"],
                     "control_epoch": body["control_epoch"], "accepted_sequence": body["sequence"],
                     "action": {"type": "none"}}


class _Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def _serve(self):
        fake = self.server.fake
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b""
        body = json.loads(raw) if raw else None
        parts = urllib.parse.urlsplit(self.path)
        record = {"method": self.command, "path": parts.path, "query": urllib.parse.parse_qs(parts.query),
                  "headers": dict(self.headers), "body": body, "at": fake.clock()}
        with fake.lock:
            fake.requests.append(record)
            status, payload = fake.route(self.command, parts.path, self.headers, body)
        data = b"" if payload is None else json.dumps(payload).encode()
        self.send_response(status)
        if data:
            self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    do_GET = do_POST = do_DELETE = _serve


class FakeClock:
    """Virtual time: sleep() advances it at once and runs hooks that fall due."""

    def __init__(self, limit=600.0):
        self.now = 0.0
        self.limit = limit
        self.hooks = []

    def __call__(self):
        return self.now

    def at(self, when, action):
        self.hooks.append((when, action))
        self.hooks.sort(key=lambda hook: hook[0])

    def sleep(self, seconds):
        self.now += seconds
        time.sleep(0.003)
        while self.hooks and self.hooks[0][0] <= self.now:
            _, action = self.hooks.pop(0)
            action()
        if self.now > self.limit:
            raise AssertionError("virtual clock ran past its limit: the launcher never stopped")


class MpvFiles:
    """The files that steer tests/fake_mpv.py."""

    def __init__(self, directory):
        self.dir = directory
        self.state = {}
        self.seeks = 0

    def _write_state(self):
        temp = os.path.join(self.dir, ".state.json")
        with open(temp, "w", encoding="utf-8") as handle:
            json.dump(self.state, handle)
        os.replace(temp, os.path.join(self.dir, "state.json"))

    def props(self, **values):
        self.state.setdefault("props", {}).update({k.replace("_", "-"): v for k, v in values.items()})
        self._write_state()

    def seek(self, seconds):
        self.seeks += 1
        self.state["seek"] = {"id": self.seeks, "to": seconds}
        self._write_state()

    def reset(self):
        """A fresh mpv for a second run in the same test."""
        self.state = {}
        self._write_state()

    def close(self):
        self.state["close"] = True
        self._write_state()

    def crash(self):
        self.state["crash"] = True
        self._write_state()

    def emit(self, event):
        with open(os.path.join(self.dir, "events.jsonl"), "a", encoding="utf-8") as handle:
            handle.write(json.dumps(event) + "\n")

    def commands(self):
        try:
            with open(os.path.join(self.dir, "commands.jsonl"), encoding="utf-8") as handle:
                return [json.loads(line) for line in handle if line.strip()]
        except FileNotFoundError:
            return []

    def argv(self):
        with open(os.path.join(self.dir, "argv.json"), encoding="utf-8") as handle:
            return json.load(handle)

    def env(self):
        with open(os.path.join(self.dir, "env.json"), encoding="utf-8") as handle:
            return json.load(handle)

    def display(self):
        with open(os.path.join(self.dir, "display.json"), encoding="utf-8") as handle:
            return json.load(handle)


class LauncherCase(unittest.TestCase):
    """One fake server, one fake mpv, one virtual clock, a private config and runtime dir."""

    trusted = True

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="plurx-player-test-")
        self.config = os.path.join(self.tmp, "config", "plurx-player")
        self.runtime = os.path.join(self.tmp, "run")
        os.makedirs(self.runtime, mode=0o700)
        mpv_dir = os.path.join(self.tmp, "mpv")
        os.makedirs(mpv_dir)
        self.bin = os.path.join(self.tmp, "bin")
        os.makedirs(self.bin)
        self.wrapper = os.path.join(self.bin, "mpv")
        with open(self.wrapper, "w", encoding="utf-8") as handle:
            handle.write(f'#!/bin/sh\nexec "{sys.executable}" "{FAKE_MPV}" "$@"\n')
        os.chmod(self.wrapper, 0o755)
        self._env = os.environ.get("FAKE_MPV_DIR")
        os.environ["FAKE_MPV_DIR"] = mpv_dir
        self.mpv = MpvFiles(mpv_dir)
        self.server = FakePlurx()
        self.clock = FakeClock()
        self.server.clock = self.clock
        self.errors = []
        self.confirms = []
        if self.trusted:
            TrustStore(self.config).add(self.server.origin)

    def tearDown(self):
        self.server.stop()
        if self._env is None:
            os.environ.pop("FAKE_MPV_DIR", None)
        else:
            os.environ["FAKE_MPV_DIR"] = self._env
        import shutil
        shutil.rmtree(self.tmp, ignore_errors=True)

    def refuse(self, origin):
        self.confirms.append(origin)
        return False

    def run_launcher(self, url=None, confirm=None, extra=(), timing=None, error=None,
                     session_environment=""):
        deps = Deps(confirm=confirm or self.refuse, error=error or self.errors.append, clock=self.clock,
                    sleep=self.clock.sleep, timing=timing or Timing(), config_dir=self.config,
                    runtime_dir=self.runtime, install_signals=False,
                    session_environment=lambda: session_environment)
        return main([url or self.server.url(), "--mpv", self.wrapper, *extra], deps)

    def close_at(self, when):
        self.clock.at(when, self.mpv.close)
