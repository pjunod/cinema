"""A stand-in for mpv that speaks its JSON IPC protocol on --input-ipc-server.

Driven through files in $FAKE_MPV_DIR, so a test can steer it from outside:

* argv.json      - written at start: the command line it was given
* env.json       - written at start: the dynamic-loader variables it was given
* display.json   - written at start: the display variables it was given
* commands.jsonl - appended: every IPC command received, in order
* state.json     - read on every request: property overrides ("props"),
                   a viewer seek ({"seek": {"id": n, "to": seconds}}), plus
                   {"close": true} (the viewer closed the window: clean exit)
                   or {"crash": true} (die with status 9)
* events.jsonl   - tailed: each new line is sent to the client as an event
"""

import json
import os
import socket
import sys
import threading
import time

DIR = os.environ["FAKE_MPV_DIR"]


def path(name):
    return os.path.join(DIR, name)


def state():
    try:
        with open(path("state.json"), encoding="utf-8") as handle:
            return json.load(handle)
    except (OSError, ValueError):
        return {}


def main():
    sock_path = next(a.split("=", 1)[1] for a in sys.argv[1:] if a.startswith("--input-ipc-server="))
    with open(path("argv.json"), "w", encoding="utf-8") as handle:
        json.dump(sys.argv[1:], handle)
    with open(path("env.json"), "w", encoding="utf-8") as handle:
        json.dump({key: value for key, value in os.environ.items() if key.startswith("LD_")}, handle)
    with open(path("display.json"), "w", encoding="utf-8") as handle:
        json.dump({key: os.environ[key] for key in ("WAYLAND_DISPLAY", "DISPLAY", "XAUTHORITY",
                                                    "XDG_SESSION_TYPE", "XDG_CURRENT_DESKTOP")
                   if key in os.environ}, handle)
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(sock_path)
    server.listen(1)
    conn, _ = server.accept()
    lock = threading.Lock()
    props = {"pause": False, "speed": 1.0, "seeking": False, "paused-for-cache": False,
             "demuxer-cache-duration": 8.0}
    loaded = {"value": False, "count": 0}

    def send(message):
        with lock:
            try:
                conn.sendall((json.dumps(message) + "\n").encode())
            except OSError:
                pass

    def tail():
        offset = 0
        seen_seek = None
        while True:
            current = state()
            # {"seek": {"id": n, "to": seconds}}: the viewer seeks. mpv reports
            # the target as time-pos from the moment the seek is requested.
            seek = current.get("seek")
            if isinstance(seek, dict) and seek.get("id") != seen_seek:
                seen_seek = seek.get("id")
                props["time-pos"] = float(seek["to"])
                send({"event": "seek"})
                time.sleep(0.02)
                send({"event": "playback-restart"})
            if current.get("crash"):
                os._exit(9)
            if current.get("close"):
                conn.shutdown(socket.SHUT_RDWR)
                os._exit(0)
            try:
                with open(path("events.jsonl"), encoding="utf-8") as handle:
                    handle.seek(offset)
                    for line in handle:
                        if line.endswith("\n"):
                            offset += len(line.encode())
                            send(json.loads(line))
            except OSError:
                pass
            time.sleep(0.005)

    threading.Thread(target=tail, daemon=True).start()
    buffer = b""
    while True:
        chunk = conn.recv(65536)
        if not chunk:
            return
        buffer += chunk
        while b"\n" in buffer:
            line, buffer = buffer.split(b"\n", 1)
            request = json.loads(line)
            command = request.get("command") or []
            with open(path("commands.jsonl"), "a", encoding="utf-8") as handle:
                handle.write(json.dumps(command) + "\n")
            reply = {"request_id": request.get("request_id"), "error": "success", "data": None}
            name = command[0] if command else None
            if name == "get_property":
                values = dict(props)
                if not loaded["value"]:
                    values.pop("time-pos", None)
                values.update(state().get("props", {}))
                if command[1] in values and values[command[1]] is not None:
                    reply["data"] = values[command[1]]
                else:
                    reply["error"] = "property unavailable"
            elif name == "set_property":
                props[command[1]] = command[2]
            elif name == "loadfile":
                loaded["count"] += 1
                if state().get("props", {}).get("load-error-on") == loaded["count"]:
                    send(reply)
                    send({"event": "end-file", "reason": "error", "file_error": "fixture decode failure"})
                    continue
                loaded["value"] = True
                props["path"] = command[1]
                props["time-pos"] = float(props.get("start", 0) or 0)
                send(reply)
                for event in ("start-file", "file-loaded", "playback-restart"):
                    send({"event": event})
                continue
            elif name == "quit":
                send(reply)
                conn.shutdown(socket.SHUT_RDWR)
                return
            send(reply)


if __name__ == "__main__":
    main()
