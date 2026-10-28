"""plurx-player <url>: the x-scheme-handler for plurx-player:// URLs.

docs/clients/LINUX-PLAYER-PLAN.md section 4.2. In order: parse and trust
the URL, redeem the one-time code, play, and on every way out (normal exit,
SIGTERM/SIGINT/SIGHUP, an exception) release the session and revoke the
handoff token. The token is held in memory only.
"""

import argparse
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass, field
from typing import Callable, Optional

from . import DEVICE_NAME, VERSION
from .api import ApiError, Http, error_text
from .caps import device_caps
from .handoff import HandoffUrlError, normalize_origin, parse_handoff_url
from .mpv import Mpv, MpvError, resolve_binary
from .playback import Playback, PlaybackError, Timing
from .trust import TrustStore, default_config_dir, ensure_trusted, zenity_confirm, zenity_error

EXIT_OK = 0
EXIT_URL = 2
EXIT_UNTRUSTED = 3
EXIT_REDEEM = 4
EXIT_PLAYBACK = 5
SIGNALS = (signal.SIGTERM, signal.SIGINT, signal.SIGHUP)
# What a window needs from the desktop session. A handler started by
# xdg-desktop-portal (Chrome opens plurx-player:// through the portal) gets
# the portal's environment, which has none of these when the portal started
# before the session imported them.
SESSION_DISPLAY = ("WAYLAND_DISPLAY", "DISPLAY", "XAUTHORITY", "XDG_SESSION_TYPE", "XDG_CURRENT_DESKTOP")


def systemd_user_environment():
    """The user manager's environment, which the desktop session fills in."""
    try:
        result = subprocess.run(["systemctl", "--user", "show-environment"], stdin=subprocess.DEVNULL,
                                capture_output=True, text=True, timeout=5, check=False)
    except (OSError, subprocess.SubprocessError):
        return ""
    return result.stdout if result.returncode == 0 else ""


def adopt_session_display(environ, read_session_environment):
    """Take the session's display variables when this process has no display.

    Only the variables in SESSION_DISPLAY, and only when neither display is
    set: a launcher started inside the session keeps its own. Returns the
    names it set.
    """
    if environ.get("WAYLAND_DISPLAY") or environ.get("DISPLAY"):
        return []
    session = {}
    for line in read_session_environment().splitlines():
        key, sep, value = line.partition("=")
        # systemctl writes a value with special characters as $'...'; no
        # display name or session type needs one.
        if sep and key in SESSION_DISPLAY and value and not value.startswith("$'"):
            session[key] = value
    if not (session.get("WAYLAND_DISPLAY") or session.get("DISPLAY")):
        return []
    # As a set: an XAUTHORITY left over beside no display is not this
    # session's, and must not be paired with its DISPLAY.
    for key in SESSION_DISPLAY:
        environ.pop(key, None)
    adopted = [key for key in SESSION_DISPLAY if key in session]
    for key in adopted:
        environ[key] = session[key]
    return adopted


@dataclass
class Deps:
    """What the tests replace: dialogs, time, and where files go."""
    confirm: Callable[[str], bool] = zenity_confirm
    error: Callable[[str], None] = zenity_error
    clock: Callable[[], float] = time.monotonic
    sleep: Callable[[float], None] = time.sleep
    timing: Timing = field(default_factory=Timing)
    config_dir: Optional[str] = None
    runtime_dir: Optional[str] = None
    install_signals: bool = True
    session_environment: Callable[[], str] = systemd_user_environment


class Terminated(BaseException):
    """Raised by the signal handler so every finally block runs."""

    def __init__(self, signum):
        super().__init__(signum)
        self.signum = signum


class SignalGate:
    """Defers SIGTERM/SIGINT/SIGHUP until the token is inside the guarded region.

    A signal that interrupted the redeem call after the server had issued the
    token, but before the try whose finally revokes it, would leave a live
    token behind. Until arm(), the handler only records the signal; arm()
    runs inside that try, right after the token is stored, and raises then.
    """

    def __init__(self):
        self.armed = False
        self.deferred = None

    def handler(self, signum, _frame):
        if self.armed:
            raise Terminated(signum)
        if self.deferred is None:
            self.deferred = signum

    def arm(self):
        self.armed = True
        if self.deferred is not None:
            raise Terminated(self.deferred)


def log(message):
    print(f"plurx-player: {message}", file=sys.stderr, flush=True)


def parse_args(argv):
    parser = argparse.ArgumentParser(
        prog="plurx-player",
        description="Play a plurx title handed over by the plurx web app (plurx-player://play?...).")
    parser.add_argument("url", help="the plurx-player:// URL the web app opened")
    parser.add_argument("--ca", metavar="FILE",
                        help="extra CA certificate for a server with a private certificate; "
                             "remembered for that server")
    parser.add_argument("--mpv", metavar="PATH",
                        help="mpv binary (default: /usr/lib/plurx-player/bin/mpv, then mpv on PATH)")
    parser.add_argument("--version", action="version", version=f"plurx-player {VERSION}")
    return parser.parse_args(argv)


def main(argv=None, deps=None):
    deps = deps or Deps()
    args = parse_args(argv)
    adopted = adopt_session_display(os.environ, deps.session_environment)
    if adopted:
        log(f"no display in this process's environment; using the desktop session's {', '.join(adopted)}")
    try:
        handoff = parse_handoff_url(args.url)
    except HandoffUrlError as error:
        log(f"refused: {error}")
        deps.error(f"plurx Player could not open this link: {error}.")
        return EXIT_URL
    if args.ca and not os.path.isfile(args.ca):
        deps.error(f"plurx Player: no CA file at {args.ca}.")
        return EXIT_URL
    store = TrustStore(deps.config_dir)
    if not ensure_trusted(store, handoff.server, deps.confirm, args.ca):
        log(f"refused: {handoff.server} is not a server you allowed")
        return EXIT_UNTRUSTED
    ca = args.ca or (store.entry(handoff.server) or {}).get("ca")
    http = Http(handoff.server, ca_file=ca, user_agent=f"plurx-player/{VERSION}")
    gate = SignalGate()
    previous = {}
    if deps.install_signals and threading.current_thread() is threading.main_thread():
        previous = {sig: signal.signal(sig, gate.handler) for sig in SIGNALS}
    try:
        code = _redeem_and_play(args, deps, handoff, http, gate)
        if gate.deferred is not None and not gate.armed:
            # Stopped during a redeem that gave no token: nothing to revoke.
            log(f"stopped by signal {gate.deferred}")
            return 128 + gate.deferred
        return code
    except Terminated as terminated:
        log(f"stopped by signal {terminated.signum}")
        return 128 + terminated.signum
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)


def _redeem_and_play(args, deps, handoff, http, gate):
    try:
        status, redeemed = http.request("POST", "/api/v1/playback-handoffs/redeem",
                                        {"code": handoff.code, "device": DEVICE_NAME}, auth=False)
    except ApiError as error:
        deps.error(f"plurx Player could not reach {handoff.server}: {error}")
        return EXIT_REDEEM
    if status == 410:
        deps.error("This plurx Player link has expired or was already used. Press Play in plurx again.")
        return EXIT_REDEEM
    if status != 200 or not isinstance(redeemed, dict) or not isinstance(redeemed.get("token"), str) \
            or not redeemed["token"]:
        deps.error(f"plurx refused the handoff: {error_text(redeemed, f'HTTP {status}')}")
        return EXIT_REDEEM
    runtime = mpv = playback = None
    failure = None
    try:
        # Stored inside the region whose finally revokes it; a signal that
        # arrived while redeeming is raised by arm(), after this line.
        http.token = redeemed.pop("token")
        gate.arm()
        _check_redeemed(redeemed, handoff.server)
        binary = resolve_binary(args.mpv)
        if not binary:
            raise PlaybackError("mpv is not installed")
        runtime = tempfile.mkdtemp(prefix="plurx-player-", dir=_runtime_base(deps))
        mpv = Mpv(binary, os.path.join(runtime, "mpv.sock"),
                  config_dir=os.path.join(deps.config_dir or default_config_dir(), "mpv"))
        mpv.start()
        from .cinema import Preferences
        preferences = Preferences(deps.config_dir or default_config_dir())
        while True:
            playback = Playback(http, mpv, redeemed, device_caps(VERSION), deps.clock, deps.sleep,
                                deps.timing, log, preferences)
            code = playback.run()
            if playback.next_handoff is None or mpv.closed:
                break
            redeemed = playback.next_handoff
            playback.finish()
    except (PlaybackError, MpvError) as error:
        if playback is not None and playback.failure is None:
            playback.failure = str(error)
        log(str(error))
        failure, code = f"plurx Player: {error}", EXIT_PLAYBACK
    except Exception as error:
        if playback is not None and playback.failure is None:
            playback.failure = f"plurx Player crashed: {type(error).__name__}"
        raise
    finally:
        _teardown(http, playback, mpv, runtime, deps)
    # After the release: a dialog waits for the viewer, and the session and
    # token must not wait with it.
    if failure:
        deps.error(failure)
    return code


def _check_redeemed(redeemed, server):
    canonical = redeemed.get("origin")
    if canonical is not None:
        try:
            same = normalize_origin(canonical) == server
        except (HandoffUrlError, TypeError):
            same = False
        if not same:
            raise PlaybackError(f"the server answered as {canonical!r}, not {server}")
    file_id = redeemed.get("file_id")
    if not isinstance(file_id, int) or isinstance(file_id, bool) or file_id <= 0:
        raise PlaybackError("the server's handoff named no file")


def _runtime_base(deps):
    base = deps.runtime_dir or os.environ.get("XDG_RUNTIME_DIR")
    return base if base and os.path.isdir(base) else None


def _teardown(http, playback, mpv, runtime, deps):
    """Release everything; a second signal must not interrupt the release."""
    masked = {}
    if deps.install_signals and threading.current_thread() is threading.main_thread():
        masked = {sig: signal.signal(sig, signal.SIG_IGN) for sig in SIGNALS}
    try:
        if playback is not None:
            playback.finish()
        try:
            status, body = http.request("DELETE", "/api/v1/playback-handoffs/self", timeout=5)
            if status not in (200, 204, 401, 404, 410):
                log(f"handoff revoke answered {status}: {error_text(body, '')}")
        except ApiError as error:
            log(f"handoff revoke failed: {error}")
        http.token = None
        if mpv is not None:
            mpv.quit()
        if runtime:
            shutil.rmtree(runtime, ignore_errors=True)
    finally:
        for sig, handler in masked.items():
            signal.signal(sig, handler)
