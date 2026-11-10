#!/usr/bin/env python3
"""Exercise Cinema's documented mpv boundary against the installed renderer.

No server, credentials, display or media library is needed. The null outputs
bound this to IPC/Lua interface parity; HDR/DV needs separate qualification.
"""
import os
from pathlib import Path
import sys
import tempfile
import time
import wave

# Prefer the installed frontend when verifying its binary package.
sys.path.insert(0, os.environ.get("PLURX_PLAYER_MODULES", "/usr/lib/plurx-player"))
from plurx_player.mpv import Mpv, resolve_binary


class InterfaceMpv(Mpv):
    def args(self):
        return super().args() + ["--vo=null", "--ao=null", "--force-window=no",
                                 "--pause=yes", f"--log-file={self.config_dir}/mpv.log"]


def main():
    with tempfile.TemporaryDirectory(prefix="cinema-interface-") as directory:
        root = Path(directory)
        media = root / "control.wav"
        with wave.open(str(media), "wb") as output:
            output.setnchannels(1)
            output.setsampwidth(2)
            output.setframerate(8000)
            output.writeframes(b"\0\0" * 80000)
        player = InterfaceMpv(resolve_binary(), str(root / "ipc.sock"), config_dir=directory)
        try:
            player.start()
            player.command("observe_property", 1, "pause")
            metadata = {"metadata": {"title": "Interface control"}, "state": "playing"}
            player.set("user-data/cinema", metadata)
            assert player.get("user-data/cinema") == metadata
            player.set("user-data/osc/film", {"origin": 60, "duration": 7200})
            assert player.get("user-data/osc/film")["origin"] == 60
            player.command("loadfile", str(media), "replace")
            deadline = time.monotonic() + 10
            while player.get("path") != str(media) or player.get("duration", 0) <= 0:
                assert time.monotonic() < deadline, "loadfile did not load the control"
                time.sleep(0.02)
            # This binding is registered by Cinema, not by the stock OSC.
            # Invoke it through mpv's public script-binding interface to prove
            # the actual installed Lua loaded and handled its callback.
            player.command("script-binding", "cinema/cinema-pause")
            deadline = time.monotonic() + 5
            while player.get("pause") is not False:
                assert time.monotonic() < deadline, "Cinema pause binding did not run"
                time.sleep(0.02)
            assert any(event.get("event") == "property-change" for event in player.events())
            player.command("del", "user-data/osc/film")
            assert player.get("user-data/osc/film") is None
        finally:
            player.quit()
        log = (root / "mpv.log").read_text()
        assert "Lua error" not in log and "Error loading script" not in log, log
    print("PASS: system mpv loadfile/properties/user-data/Cinema Lua interface")


if __name__ == "__main__":
    main()
