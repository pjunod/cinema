#!/usr/bin/env python3
"""Runs only as the LightDM PAM session's XDG autostart child."""
import argparse
import ctypes
import json
import os
from pathlib import Path
import shlex
import subprocess
import time
from common import Receipt, hosted_guard, run, wait_for, write_json
from lifecycle import Lifecycle


def session_for(pid):
    library = ctypes.CDLL("libsystemd.so.0")
    library.sd_pid_get_session.argtypes = [ctypes.c_int, ctypes.POINTER(ctypes.c_void_p)]
    library.sd_pid_get_session.restype = ctypes.c_int
    pointer = ctypes.c_void_p()
    code = library.sd_pid_get_session(pid, ctypes.byref(pointer))
    if code < 0:
        raise RuntimeError(f"PID {pid} has no logind session ({code})")
    try:
        session_id = ctypes.string_at(pointer).decode()
    finally:
        libc = ctypes.CDLL(None)
        libc.free.argtypes = [ctypes.c_void_p]
        libc.free(pointer)
    text = run(["loginctl", "show-session", session_id, "--no-pager",
                "-p", "Id", "-p", "User", "-p", "Name", "-p", "Type", "-p", "Class",
                "-p", "Remote", "-p", "Active", "-p", "Seat", "-p", "Service"]).stdout
    return dict(line.split("=", 1) for line in text.splitlines() if "=" in line)


def require_session(value, kind, uid):
    for key, expected in {"Type": kind, "Class": "user", "Remote": "no", "Active": "yes",
                          "Seat": "seat0", "User": str(uid), "Service": "lightdm-autologin"}.items():
        if value.get(key) != expected:
            raise RuntimeError(f"real PAM session {key}={value.get(key)!r}, expected {expected!r}")


def bus_json(*args):
    return json.loads(run(["busctl", "--user", "--json=short", *args]).stdout)["data"]


def bus_pid(name):
    data = bus_json("call", "org.freedesktop.DBus", "/org/freedesktop/DBus",
                    "org.freedesktop.DBus", "GetConnectionUnixProcessID", "s", name)
    return data[0]


def notifier_host(kind, session):
    registered = bus_json("get-property", "org.kde.StatusNotifierWatcher", "/StatusNotifierWatcher",
                          "org.kde.StatusNotifierWatcher", "IsStatusNotifierHostRegistered")
    if registered not in (True, [True]):
        raise RuntimeError("no real StatusNotifierHost is registered")
    names = bus_json("call", "org.freedesktop.DBus", "/org/freedesktop/DBus",
                     "org.freedesktop.DBus", "ListNames")
    if len(names) == 1 and isinstance(names[0], list):
        names = names[0]
    observations = []
    for name in names:
        if not name.startswith("org.kde.StatusNotifierHost"):
            continue
        pid = bus_pid(name)
        executable = Path(f"/proc/{pid}/exe").resolve(strict=True)
        owner = run(["dpkg-query", "-S", executable]).stdout.strip()
        expected = "xfce4-panel" if kind == "x11" else "plasma-workspace"
        if expected not in owner:
            continue
        process_session = session_for(pid)
        if process_session["Id"] != session["Id"]:
            raise RuntimeError("notifier host belongs to another login session")
        observations.append({"bus": name, "pid": pid, "executable": str(executable),
                             "package": owner, "session": process_session})
    if not observations:
        raise RuntimeError("registered notifier host cannot be tied to the distro panel in this PAM session")
    return observations


def smoke(action, output, lifecycle, session):
    lifecycle.call("start")
    before = lifecycle.state("running")
    args = ["/usr/bin/plurx-tray", "--smoke-receipt", output, "--smoke-action", action,
            "--smoke-timeout", "90", "--control", "/usr/bin/plurx-control"]
    with output.with_suffix(".log").open("w") as log:
        process = subprocess.Popen([str(a) for a in args], stdout=log, stderr=subprocess.STDOUT)
        observed = None
        deadline = time.monotonic() + 100
        while process.poll() is None and time.monotonic() < deadline:
            try:
                if Path(f"/proc/{process.pid}/exe").resolve() == Path("/usr/bin/plurx-tray").resolve():
                    observed = session_for(process.pid)
            except (OSError, RuntimeError):
                pass
            time.sleep(0.05)
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=10)
            raise RuntimeError("real tray smoke timed out")
        if process.returncode:
            raise RuntimeError(f"tray smoke {action} exited {process.returncode}; see {log.name}")
    if not observed or observed["Id"] != session["Id"]:
        raise RuntimeError("actual tray process membership was not observed in the PAM session")
    value = json.loads(output.read_bytes())
    if value["ui_pid"] != process.pid or value["uid"] != os.getuid():
        raise RuntimeError("UI receipt process identity differs from independently observed child")
    if not all(value["tray"].get(k) for k in ("watcher_present", "host_registered", "item_registered")):
        raise RuntimeError("real SNI item was not registered with the host")
    if Path(value["controller"]["path"]).resolve() != Path("/usr/bin/plurx-control").resolve():
        raise RuntimeError("UI controller path differs from installed package")
    if not value["controller"]["first_status"]["ok"]:
        raise RuntimeError("native menu never observed actual successful controller status")
    if not value["window"]["present"] or not value["menu"]["titles"]:
        raise RuntimeError("real GTK window/menu was not observed")
    if value["quit"]["controller_calls_during_quit"] != 0 or value["quit"]["via"] != "quit_action":
        raise RuntimeError("Quit did not use the normal no-server-command path")
    after = lifecycle.state("stopped" if action == "stop" else "running")
    if after["installation_id"] != before["installation_id"]:
        raise RuntimeError("UI action changed installation identity")
    if action == "stop" and not value["action"]["ui_alive_after"]:
        raise RuntimeError("Stop closed the tray UI")
    return {"ui": value, "process_session": observed, "independent_before": before, "independent_after": after}


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--candidate", required=True)
    p.add_argument("--kind", choices=["x11", "wayland"], required=True)
    p.add_argument("--output", type=Path, required=True)
    a = p.parse_args()
    with Receipt(a.output / "session.json", a.candidate, f"native-linux-{a.kind}") as receipt:
        hosted_guard(a.candidate, mutate=True)
        if os.getuid() == 0 or os.environ.get("SSH_CONNECTION") or os.environ.get("SSH_TTY"):
            raise RuntimeError("qualification child must be an unprivileged local PAM session")
        session = session_for(os.getpid())
        require_session(session, a.kind, os.getuid())
        receipt.check("independent-logind-process-membership", session)
        display = run(["xdpyinfo"] if a.kind == "x11" else ["wayland-info"], timeout=20).stdout
        receipt.check("actual-display-roundtrip", {"kind": a.kind, "details": display[:6000]})
        hosts = wait_for(lambda: notifier_host(a.kind, session), seconds=45, description="distro SNI host")
        receipt.check("real-distro-notifier-host", hosts)
        lifecycle = Lifecycle("/usr/bin/plurx-control", receipt)
        denied_before = lifecycle.call("status")
        denied = run(["/usr/bin/plurx-control", "start"], timeout=30, check=False)
        denied_value = json.loads(denied.stdout)
        if denied.returncode == 0 or denied_value.get("error", {}).get("code") != "authorization_denied":
            raise RuntimeError("narrow denied fixture rule did not refuse real helper authorization")
        if lifecycle.call("status") != denied_before:
            raise RuntimeError("denied authorization changed status or startup policy")
        receipt.check("fixture-authorization-denied", denied_value)
        (a.output / "request-allow").touch()
        wait_for(lambda: (a.output / "allow-ready").exists(), seconds=60, description="approved fixture rule")
        receipt.check("fixture-authorization-approved", lifecycle.call("start"))
        lifecycle.four_states()
        for action in ("quit", "stop", "restart"):
            receipt.check("native-ui-" + action, smoke(action, a.output / f"ui-{action}.json", lifecycle, session))
        lifecycle.stop()
        lifecycle.call("startup", "set", "server", "off")
        lifecycle.call("startup", "set", "tray", "off")
        receipt.value["limitations"] = ["Authorization was provided by a narrow disposable fixture rule, not a human prompt.",
            "Virtual display output proves GUI protocol/session behavior, not physical monitor output.",
            "Production GitHub update download, signed distribution, old legacy adoption and reboot remain separate acceptance."]


if __name__ == "__main__":
    main()
