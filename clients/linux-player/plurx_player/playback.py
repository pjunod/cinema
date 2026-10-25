"""One handed-off playback: decision, session, mpv, heartbeats, teardown.

docs/clients/LINUX-PLAYER-PLAN.md section 4.2, steps 2 to 7. Positions sent
to the server are always mpv's playback time plus the session's
``media_origin_ms``: mpv's timeline starts at the session origin, and this
is the one place the origin is added.
"""

import json
import re
import subprocess
import urllib.parse
import uuid
from dataclasses import dataclass

from .api import ApiError, error_text
from .mpv import CLOSED, MpvError

PROTOCOL = "plurx-playback-control-v1"
# How close to the end of what a growing session has produced a seek may land
# before mpv would stall waiting for the encoder.
SEEK_MARGIN_MS = 2000
# The mapper is whatever libplacebo reports. M5's build is expected to log
# "plurx mapper: <name>"; a bare "CM vX" from libplacebo is also accepted.
EXPLICIT_MAPPER = re.compile(r"plurx mapper:\s*(.+?)\s*$")
BARE_MAPPER = re.compile(r"\b((?:closed-form )?CM v\d+(?:\.\d+)*)\b")


@dataclass
class Timing:
    control_s: float = 5.0
    progress_s: float = 10.0
    tick_s: float = 0.25
    stop_osd_s: float = 3.0


class ServerStop(Exception):
    """The server ended this playback (control 404/410) or revoked access (401)."""


class PlaybackError(Exception):
    """This playback cannot continue; the message is for the viewer."""


def _track(value):
    return value if isinstance(value, int) and not isinstance(value, bool) else None


def _number(value):
    return value if isinstance(value, (int, float)) and not isinstance(value, bool) else None


class Playback:
    def __init__(self, http, mpv, handoff, caps, clock, sleep, timing=None, log=None):
        self.http = http
        self.mpv = mpv
        self.caps = caps
        self.clock = clock
        self.sleep = sleep
        self.timing = timing or Timing()
        self.log = log or (lambda message: None)
        self.file_id = handoff["file_id"]
        self.item_id = _track(handoff.get("item_id"))
        self.start_ms = max(0, _track(handoff.get("start_ms")) or 0)
        self.audio = _track(handoff.get("audio"))
        self.subtitle = _track(handoff.get("subtitle"))
        # One playback_id for the whole run: a re-created session supersedes
        # its own predecessor and nobody else's (API.md section 9.1).
        self.playback_id = f"plurx-player:{uuid.uuid4()}"
        self.client_instance_id = str(uuid.uuid4())
        self.sequence = 0
        self.decision = None
        self.delivery = None
        self.mode = None
        self.method = None
        self.session = None
        self.origin_ms = 0
        self.sidecar = None
        self.burn = None
        self.notice = None
        self.started = False
        self.loading = False
        self.ended = False
        self.paused = False
        self.seeking = False
        self.last_position_ms = None
        self.mapper = None
        self.mapper_logged = False
        self.facts = None
        self.failure = None
        self.stopped_by_server = None
        self.recreations = 0
        self._auth_header = False

    # -- the server ---------------------------------------------------------

    def _call(self, method, path, body=None, auth=True, timeout=None):
        status, payload = self.http.request(method, path, body, auth=auth, timeout=timeout)
        if status == 401 and auth:
            raise ServerStop("Signed out of plurx: playback stopped")
        return status, payload

    def decide(self):
        query = ["force=auto"]
        if self.audio is not None:
            query.append(f"audio={self.audio}")
        if self.subtitle is not None:
            query.append(f"subtitle={self.subtitle}")
        try:
            status, body = self._call("POST", f"/api/v1/files/{self.file_id}/decision?{'&'.join(query)}",
                                      {"caps": self.caps})
        except ApiError as error:
            raise PlaybackError(f"Could not reach plurx: {error}") from None
        if status != 200 or not isinstance(body, dict) or not isinstance(body.get("delivery"), dict):
            raise PlaybackError(f"plurx could not plan this playback: {error_text(body, f'HTTP {status}')}")
        mode = body["delivery"].get("mode")
        if mode not in ("direct", "remux", "transcode"):
            raise PlaybackError(f"plurx planned a delivery this player does not know: {mode!r}")
        self.decision, self.delivery, self.mode = body, body["delivery"], mode
        self.method = body.get("method") if isinstance(body.get("method"), str) else None

    def plan_subtitle(self):
        if self.subtitle is None or self.subtitle < 0:
            return
        tracks = self.decision.get("subtitles") or []
        track = next((t for t in tracks if isinstance(t, dict) and t.get("index") == self.subtitle), None)
        if track is None:
            self.notice = "The chosen subtitle is not available"
        elif track.get("text"):
            # Cues are on the absolute source timeline; mpv's starts at the
            # session origin, so the track is shifted by -origin (on_file_loaded).
            self.sidecar = f"/api/v1/files/{self.file_id}/subs/{self.subtitle}"
        elif self.mode != "direct":
            self.burn = self.subtitle
        else:
            self.notice = "Image subtitles are not available in plurx Player yet"

    def create_session(self, start_ms):
        body = {"playback_id": self.playback_id, "request_id": str(uuid.uuid4()), "caps": self.caps,
                "start": round(max(0, start_ms) / 1000, 3), "presentation": "vod"}
        if self.audio is not None:
            body["audio"] = self.audio
        if self.mode == "remux":
            body.update(copy=True, aac=bool(self.delivery.get("aac")),
                        preserve_dolby_vision=bool(self.delivery.get("preserve_dolby_vision")))
        else:
            # Transcode: no height. Auto is the server's choice (API.md 7.2).
            body["quality_auto"] = True
        if self.burn is not None:
            body["subtitle_burn"] = self.burn
        try:
            status, created = self._call("POST", self.delivery.get("sessions_url"), body)
            if status == 422 and self.burn is not None:
                self.burn = None
                body.pop("subtitle_burn")
                body["request_id"] = str(uuid.uuid4())
                self.notice = "This image subtitle cannot be burned into HDR; playing without it"
                status, created = self._call("POST", self.delivery.get("sessions_url"), body)
        except ApiError as error:
            raise PlaybackError(f"Could not start the stream: {error}") from None
        if status not in (200, 201) or not isinstance(created, dict) \
                or not created.get("session_id") or not created.get("playlist_url"):
            raise PlaybackError(f"plurx could not start the stream: {error_text(created, f'HTTP {status}')}")
        return created

    def delete_session(self, session):
        if not session:
            return
        sid = urllib.parse.quote(str(session.get("session_id")), safe="")
        try:
            self.http.request("DELETE", f"/api/v1/hls/{sid}", auth=False, timeout=5)
        except ApiError as error:
            self.log(f"session release failed: {error}")

    # -- mpv ----------------------------------------------------------------

    def load(self, url, start_local_s):
        if (self.mode == "direct" or self.sidecar) and not self._auth_header:
            # Over the private IPC socket, never argv: `ps` shows argv to everyone.
            self.mpv.set("http-header-fields", [f"Authorization: Bearer {self.http.token}"])
            self._auth_header = True
        self.loading = True
        self.mpv.set("start", f"{max(0.0, start_local_s):.3f}")
        self.mpv.command("loadfile", url, "replace")

    def start(self):
        self.decide()
        self.plan_subtitle()
        self.mpv.command("observe_property", 1, "pause")
        self.mpv.command("request_log_messages", "info")
        if self.mode == "direct":
            self.origin_ms = 0
            url, start_local = self.http.url(self.delivery.get("url")), self.start_ms / 1000
        else:
            self.session = self.create_session(self.start_ms)
            self.origin_ms = max(0, _track(self.session.get("media_origin_ms")) or 0)
            url = self.http.url(self.session["playlist_url"])
            start_local = (self.start_ms - self.origin_ms) / 1000
        self.load(url, start_local)
        if self.notice:
            self.mpv.osd(self.notice, 4000)

    def on_file_loaded(self):
        try:
            if self.sidecar:
                self.mpv.command("sub-add", self.http.url(self.sidecar), "select", "plurx")
                self.mpv.set("sub-delay", -self.origin_ms / 1000)
            if self.mode == "direct" and self.audio is not None:
                indices = [t.get("index") for t in self.decision.get("audio") or [] if isinstance(t, dict)]
                if self.audio in indices:
                    self.mpv.set("aid", indices.index(self.audio) + 1)
        except MpvError as error:
            self.log(f"track selection failed: {error}")

    def position_ms(self):
        """mpv time plus the session origin; the last known value when mpv has none."""
        if not self.mpv.closed:
            seconds = _number(self.mpv.get("time-pos"))
            if seconds is not None and seconds >= 0:
                self.last_position_ms = self.origin_ms + round(seconds * 1000)
        return self.last_position_ms

    def duration_ms(self):
        for source in ((self.decision or {}).get("source") or {}, self.session or {}):
            value = _track(source.get("duration_ms")) if isinstance(source, dict) else None
            if value:
                return value
        return None

    # -- seeking --------------------------------------------------------------

    def available_end_local_ms(self):
        ends = []
        sid = urllib.parse.quote(str(self.session.get("session_id")), safe="")
        try:
            status, body = self.http.request("GET", f"/api/v1/hls/{sid}/status", auth=False, timeout=3)
            if status == 200 and isinstance(body, dict) and _track(body.get("published_end_ms")) is not None:
                ends.append(body["published_end_ms"])
        except ApiError:
            pass
        duration = _number(self.mpv.get("duration"))
        if duration is not None:
            ends.append(round(duration * 1000))
        return max(ends) if ends else None

    def on_seek(self):
        if self.session is None or self.session.get("vod") or not self.started or self.loading:
            return
        seconds = _number(self.mpv.get("time-pos"))
        if seconds is None:
            return
        target_local = round(seconds * 1000)
        # Everything the session has produced, from its segment 0, is
        # seekable; past that edge mpv would wait on the encoder.
        end = self.available_end_local_ms()
        if end is None or 0 <= target_local <= max(0, end - SEEK_MARGIN_MS):
            return  # inside the session: mpv seeks within the playlist
        self.recreate(self.origin_ms + target_local)

    def recreate(self, target_ms):
        """Outside the available range: a fresh session that starts at the target."""
        self.log(f"seek to {target_ms} ms is outside the session; re-creating it")
        try:
            self.mpv.set("pause", True)
        except MpvError:
            pass
        old, self.session = self.session, None
        self.delete_session(old)
        self.session = self.create_session(target_ms)
        self.origin_ms = max(0, _track(self.session.get("media_origin_ms")) or 0)
        self.last_position_ms = target_ms
        self.load(self.http.url(self.session["playlist_url"]), (target_ms - self.origin_ms) / 1000)
        self.mpv.set("pause", False)
        self.recreations += 1

    # -- reporting ------------------------------------------------------------

    def control_body(self):
        position = self.position_ms()
        position = max(0, position if position is not None else self.origin_ms)
        cached = _number(self.mpv.get("demuxer-cache-duration")) or 0
        seeking = self.seeking or bool(self.mpv.get("seeking", False))
        demand = "hold" if self.paused else "active"
        speed = _number(self.mpv.get("speed")) or 1.0
        rate = min(4.0, max(0.25, speed)) if demand == "active" else min(4.0, max(0.0, speed))
        if not self.started:
            render = "starting"
        elif seeking:
            render = "seeking"
        elif self.mpv.get("paused-for-cache", False):
            render = "waiting"
        else:
            render = "rendering"
        control = self.session["control"]
        self.sequence += 1
        subtitle = {"mode": "burn", "track": self.burn} if self.burn is not None else {"mode": "off"}
        # Passive: no supported_actions, so the server only ever answers `none`.
        return {
            "protocol": PROTOCOL, "generation": control.get("generation"),
            "control_epoch": control.get("control_epoch"),
            "client_instance_id": self.client_instance_id, "sequence": self.sequence,
            "demand": demand, "position_ms": position, "buffered_from_ms": None,
            "buffered_through_ms": position + max(0, round(cached * 1000)),
            "playback_rate": rate, "render_state": render,
            "seek_target_ms": position if render == "seeking" else None,
            "observed_download_bps": None,
            "selection": {"quality": {"mode": "original"} if self.mode == "remux" else {"mode": "auto"},
                          "audio_track": self.audio, "subtitle": subtitle, "audio_offset_ms": 0,
                          "codec": "auto", "dynamic_range": "auto"},
            "capabilities": None, "observation": None, "acknowledgement": None,
        }

    def heartbeat(self):
        control = (self.session or {}).get("control")
        if not isinstance(control, dict) or not control.get("url"):
            return
        try:
            status, body = self.http.request("POST", control["url"], self.control_body(),
                                             auth=False, timeout=4.5)
        except ApiError as error:
            self.log(f"control exchange failed: {error}")
            return
        if status in (404, 410):
            raise ServerStop(error_text(body, "Playback was stopped on the server"))
        if status != 200:
            self.log(f"control exchange answered {status}: {error_text(body, '')}")

    def progress(self, reason):
        if self.item_id is None:
            return
        if reason == "exit":
            position = (self.duration_ms() or self.last_position_ms) if self.ended else self.last_position_ms
        else:
            position = self.position_ms()
        if position is None:
            return
        body = {"position_ms": position, "method": self.method}
        duration = self.duration_ms()
        if duration:
            body["duration_ms"] = duration
        try:
            if reason == "exit":
                self.http.request("POST", f"/api/v1/items/{self.item_id}/progress", body, timeout=5)
            else:
                self._call("POST", f"/api/v1/items/{self.item_id}/progress", body)
        except ApiError as error:
            self.log(f"progress failed: {error}")

    def on_log(self, event):
        text = event.get("text") or ""
        prefix = str(event.get("prefix") or "")
        match = EXPLICIT_MAPPER.search(text)
        if match is None and ("placebo" in prefix or prefix.startswith("vo")):
            match = BARE_MAPPER.search(text)
        if match:
            self.mapper = match.group(1)[:80]

    def capture_facts(self):
        if self.mpv.closed:
            return
        target = self.mpv.get("video-target-params") or {}
        tracks = self.mpv.get("track-list") or []
        video = next((t for t in tracks if isinstance(t, dict) and t.get("type") == "video"
                      and t.get("selected")), {})
        profile = _track(video.get("dolby-vision-profile"))
        nits = _number(target.get("max-luma")) if isinstance(target, dict) else None
        self.facts = {"dv_profile": profile, "mapper": self.mapper or "unknown",
                      "target_nits": round(nits) if nits else None,
                      "target_gamma": target.get("gamma") if isinstance(target, dict) else None,
                      "target_primaries": target.get("primaries") if isinstance(target, dict) else None}

    def log_mapper(self):
        """One /client-log line per playback: the DV profile, the mapper and the target."""
        if self.mapper_logged:
            return
        self.mapper_logged = True
        self.capture_facts()
        facts = self.facts or {"dv_profile": None, "mapper": self.mapper or "unknown", "target_nits": None}
        profile = facts["dv_profile"]
        nits = facts["target_nits"]
        message = (f"plurx Player (Linux): DV profile {profile if profile is not None else 'none'}, "
                   f"mapper {facts['mapper']}, target {f'{nits} nits' if nits else 'unknown'}")
        if not self.mpv.closed:
            self.mpv.osd(message.split(": ", 1)[1], 4000)
        body = {"level": "info", "event": "native_mapper", "message": message, "method": self.method,
                "file_id": self.file_id, "session_id": (self.session or {}).get("session_id"),
                "ua": self.caps.get("client", {}).get("ua"), "detail": json.dumps(facts, sort_keys=True)}
        try:
            self.http.request("POST", "/api/v1/client-log", body, timeout=5)
        except ApiError as error:
            self.log(f"client-log failed: {error}")

    # -- the loop -------------------------------------------------------------

    def on_event(self, event):
        name = event.get("event")
        if name == CLOSED:
            return False
        if name == "file-loaded":
            self.on_file_loaded()
        elif name == "playback-restart":
            first = not self.started
            self.started, self.loading, self.seeking = True, False, False
            if first:
                self.capture_facts()
            else:
                self.progress("seek")
        elif name == "seek":
            self.seeking = True
            self.on_seek()
        elif name == "property-change" and event.get("name") == "pause":
            paused = bool(event.get("data"))
            changed, self.paused = paused != self.paused, paused
            if paused and changed and self.started:
                self.progress("pause")
        elif name == "end-file":
            reason = event.get("reason")
            if reason == "eof":
                self.ended = True
            elif reason == "error":
                self.failure = f"mpv could not play the stream ({event.get('file_error') or 'error'})"
        elif name == "log-message":
            self.on_log(event)
        return True

    def loop(self):
        timing = self.timing
        now = self.clock()
        next_control, next_progress = now, now + timing.progress_s
        while True:
            for event in self.mpv.events():
                if not self.on_event(event):
                    return
            if self.mpv.closed:
                return
            now = self.clock()
            if self.session is not None and now >= next_control:
                self.heartbeat()
                next_control = _advance(next_control, timing.control_s, now)
            if now >= next_progress:
                if self.started and not self.paused:
                    self.progress("tick")
                    self.log_mapper()
                next_progress = _advance(next_progress, timing.progress_s, now)
            self.position_ms()
            self.sleep(timing.tick_s)

    def run(self):
        """Play until mpv closes. 0 on a normal end or a server stop, 5 on failure."""
        try:
            self.start()
            self.loop()
        except ServerStop as stop:
            self.stopped_by_server = str(stop)
            self.log(f"stopped by the server: {stop}")
            self.mpv.osd(str(stop), int(self.timing.stop_osd_s * 1000))
            self.sleep(self.timing.stop_osd_s)
            self.mpv.quit()
            return 0
        except PlaybackError as error:
            self.failure = str(error)
            self.log(str(error))
            if not self.mpv.closed:
                self.mpv.osd(str(error), int(self.timing.stop_osd_s * 1000))
                self.sleep(self.timing.stop_osd_s)
                self.mpv.quit()
            return 5
        try:
            status = self.mpv.proc.wait(timeout=5) if self.mpv.proc is not None else 0
        except subprocess.TimeoutExpired:
            status = None
        if status not in (0, None) and not self.ended and self.failure is None:
            self.failure = f"mpv exited with status {status}"
        return 5 if self.failure else 0

    def finish(self):
        """Teardown, each step independent: final progress, the mapper line, the release."""
        for step in (self._final_progress, self._final_mapper, self._report_failure):
            try:
                step()
            except (ApiError, MpvError, ServerStop, OSError) as error:
                self.log(f"teardown: {error}")
        session, self.session = self.session, None
        self.delete_session(session)

    def _final_progress(self):
        if self.started:
            self.progress("exit")

    def _final_mapper(self):
        if self.started and not self.mapper_logged:
            self.log_mapper()

    def _report_failure(self):
        if self.failure:
            body = {"level": "error", "event": "playback_failed", "message": self.failure[:200],
                    "method": self.method, "file_id": self.file_id,
                    "ua": self.caps.get("client", {}).get("ua")}
            self.http.request("POST", "/api/v1/client-log", body, timeout=5)


def _advance(scheduled, interval, now):
    following = scheduled + interval
    return following if following > now else now + interval
