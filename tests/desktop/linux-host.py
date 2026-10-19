#!/usr/bin/env python3
"""Provision a bounded real PAM GUI qualification only on disposable hosted Linux VMs."""
import argparse
import json
import os
from pathlib import Path
import pwd
import shutil
import subprocess
import time
from common import Receipt, hosted_guard, run, sha256, write_json

USER = "plurxqual"
ROOT = Path(__file__).resolve().parents[2]
POLKIT = Path("/etc/polkit-1/rules.d/00-plurx-qualification.rules")
SESSION_CODE = Path("/usr/local/lib/plurx-qualification")
SESSION_WORK = Path("/var/lib/plurx-qualification")


def authorization(allow):
    # Fixture authorization only: one action, one dedicated account, local active session.
    result = "YES" if allow else "NO"
    POLKIT.write_text('polkit.addRule(function(action, subject) {\n'
        '  if (action.id === "tv.plurx.install" && subject.user === "plurxqual") {\n'
        '    if (!subject.local || !subject.active) return polkit.Result.NO;\n'
        f'    return polkit.Result.{result};\n'
        '  }\n});\n')
    POLKIT.chmod(0o644)


def check_refusal(output, label, prefix, env, uid, gid):
    directory = output / ("refusal-" + label)
    directory.mkdir()
    os.chown(directory, uid, gid)
    ui_receipt = directory / "ui.json"
    command = [*prefix, "/usr/bin/plurx-tray", "--smoke-receipt", ui_receipt, "--smoke-timeout", "5"]
    result = run(command, timeout=40, check=False, env=env)
    observation = {"scope": "native-refusal", "case": label, "returncode": result.returncode,
                   "stdout": result.stdout[-4000:], "stderr": result.stderr[-4000:]}
    write_json(directory / "process.json", observation)
    if result.returncode != 1 or not ui_receipt.is_file():
        raise RuntimeError(f"{label} did not produce an explicit failed eligibility receipt")
    value = json.loads(ui_receipt.read_bytes())
    if value["session"].get("eligible") is not False or not value["session"].get("reason"):
        raise RuntimeError(f"{label} did not prove explicit session ineligibility")
    if value["tray"].get("item_registered") or (value.get("window") or {}).get("present"):
        raise RuntimeError(f"{label} created an automatic tray/window in an ineligible context")


def write_session_autostart(home, output, candidate, kind):
    autostart = home / ".config/autostart"
    autostart.mkdir(parents=True, exist_ok=True)
    # Prevent the production login entry from racing the explicit smoke child.
    # The same XDG basename masks only the companion, not this test driver.
    (autostart / "tv.plurx.Server-autostart.desktop").write_text(
        "[Desktop Entry]\nType=Application\nName=Plurx Server\n"
        "Exec=/usr/bin/plurx-tray --autostart\nHidden=true\nX-GNOME-Autostart-enabled=false\n")
    # LightDM supplies the bus/display/logind session; no su/env substitute can pass the observer.
    launcher = home / "qualification-session-driver"
    inherited = {name: os.environ[name] for name in ("GITHUB_ACTIONS", "RUNNER_ENVIRONMENT", "GITHUB_SERVER_URL",
                 "GITHUB_SHA", "PLURX_QUALIFICATION_MUTATE")}
    import shlex
    exports = "\n".join(f"export {name}={shlex.quote(value)}" for name, value in inherited.items())
    launcher.write_text("#!/bin/sh\n" + exports + "\nexec /usr/bin/python3 " +
        shlex.join([str(SESSION_CODE / "linux-session.py"), "--candidate", candidate,
                    "--kind", kind, "--output", str(output)]) + "\n")
    launcher.chmod(0o755)
    (autostart / "plurx-qualification.desktop").write_text(
        f"[Desktop Entry]\nType=Application\nName=Plurx qualification driver\nExec={launcher}\n")
    # Deliberately provision the distro panel's real Status Tray plugin, not a fixture watcher.
    if kind == "x11":
        config = home / ".config/xfce4/xfconf/xfce-perchannel-xml/xfce4-panel.xml"
        config.parent.mkdir(parents=True, exist_ok=True)
        config.write_text('''<?xml version="1.0" encoding="UTF-8"?>
<channel name="xfce4-panel" version="1.0"><property name="configver" type="int" value="2"/>
<property name="panels" type="array"><value type="int" value="1"/>
<property name="panel-1" type="empty"><property name="position" type="string" value="p=6;x=0;y=0"/>
<property name="length" type="uint" value="100"/><property name="plugin-ids" type="array"><value type="int" value="1"/></property></property></property>
<property name="plugins" type="empty"><property name="plugin-1" type="string" value="systray"/></property></channel>
''')


def configure_session(kind, home, output, candidate):
    desktop_dir = Path("/usr/share/xsessions" if kind == "x11" else "/usr/share/wayland-sessions")
    desktop_dir.mkdir(parents=True, exist_ok=True)
    session_name = f"plurx-qualification-{kind}"
    script = Path("/usr/local/libexec/plurx-qualification-session")
    script.parent.mkdir(parents=True, exist_ok=True)
    script.write_text("#!/bin/sh\n" + (
        "exec /usr/bin/xfce4-session\n" if kind == "x11" else
        "export XDG_SESSION_TYPE=wayland\nexport XDG_CURRENT_DESKTOP=KDE\n"
        "export QT_QPA_PLATFORM=wayland\nexport KWIN_COMPOSE=Q\nexport LIBGL_ALWAYS_SOFTWARE=1\n"
        "exec /usr/bin/kwin_wayland --virtual --width 1280 --height 800 --output-count 1 "
        "--exit-with-session /usr/local/libexec/plurx-qualification-plasma\n"))
    if kind == "wayland":
        plasma = Path("/usr/local/libexec/plurx-qualification-plasma")
        plasma.write_text("#!/bin/sh\nset -eu\n"
            "dbus-update-activation-environment --systemd WAYLAND_DISPLAY XDG_CURRENT_DESKTOP XDG_SESSION_TYPE\n"
            "kded5 &\nplasmashell &\n"
            "dex --autostart --environment KDE\nwait\n")
        plasma.chmod(0o755)
    script.chmod(0o755)
    (desktop_dir / (session_name + ".desktop")).write_text(
        "[Desktop Entry]\nName=Plurx disposable qualification\nType=Application\n"
        f"Exec={script}\nDesktopNames={'XFCE' if kind == 'x11' else 'KDE'}\n")
    write_session_autostart(home, output, candidate, kind)
    run(["chown", "-R", f"{USER}:{USER}", home])
    configuration = Path("/etc/lightdm/lightdm.conf.d/99-plurx-qualification.conf")
    configuration.parent.mkdir(parents=True, exist_ok=True)
    configuration.write_text("[LightDM]\nlogind-check-graphical=false\n[Seat:*]\n"
        f"autologin-user={USER}\nautologin-user-timeout=0\nautologin-session={session_name}\n"
        f"user-session={session_name}\nallow-guest=false\n"
        "xserver-command=Xorg -config /etc/X11/plurx-qualification.conf -nolisten tcp\n")


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--candidate", required=True)
    p.add_argument("--artifacts", type=Path, required=True)
    p.add_argument("--output", type=Path, required=True)
    a = p.parse_args()
    a.output = a.output.resolve()
    with Receipt(a.output / "host.json", a.candidate, "native-linux-hosted-vm") as receipt:
        hosted_guard(a.candidate, mutate=True)
        if os.getuid() != 0:
            raise RuntimeError("VM provisioning must run as root after the hosted guard")
        for path in ("/etc/plurx/installation.json", "/usr/local/bin/plurxd", "/usr/bin/plurxd"):
            if Path(path).exists():
                raise RuntimeError(f"refusing existing operator installation: {path}")
        try:
            pwd.getpwnam(USER)
        except KeyError:
            pass
        else:
            raise RuntimeError("dedicated qualification user already exists")
        for path in (SESSION_CODE, SESSION_WORK, POLKIT):
            if path.exists():
                raise RuntimeError(f"refusing pre-existing qualification namespace: {path}")
        SESSION_CODE.mkdir(mode=0o755)
        SESSION_WORK.mkdir(mode=0o755)
        # Copy public driver source only. Never loosen runner-home permissions
        # or expose its workflow token/toolchain configuration to the test user.
        for name in ("common.py", "lifecycle.py", "linux-session.py"):
            destination = SESSION_CODE / name
            shutil.copyfile(ROOT / "tests/desktop" / name, destination)
            destination.chmod(0o644)
        receipt.check("root-owned-session-driver-source", {p.name: sha256(p) for p in SESSION_CODE.iterdir()})
        run(["apt-get", "-qq", "update"], timeout=300)
        run(["apt-get", "-qq", "install", "-y", "--no-install-recommends", "lightdm", "lightdm-gtk-greeter",
             "xserver-xorg-core", "xserver-xorg-video-dummy", "x11-utils", "dbus-x11", "xfce4-session",
             "xfce4-panel", "xfwm4", "xfdesktop4", "xfce4-settings", "plasma-workspace-wayland",
             "kwin-wayland", "wayland-utils", "policykit-1", "python3", "dex"], timeout=600,
            env={**os.environ, "DEBIAN_FRONTEND": "noninteractive"})
        run(["systemctl", "stop", "lightdm"], timeout=60)
        arch = run(["dpkg", "--print-architecture"]).stdout.strip()
        packages = sorted(a.artifacts.glob(f"*_{arch}.deb"))
        if len(packages) != 2 or {p.name.split("_")[0] for p in packages} != {"plurxd", "plurx-desktop"}:
            raise RuntimeError("expected the exact server and desktop deb cohort for this native architecture")
        receipt.check("artifact-sha256", {p.name: sha256(p) for p in packages})
        run(["apt-get", "-qq", "install", "-y", *packages], timeout=600,
            env={**os.environ, "DEBIAN_FRONTEND": "noninteractive"})
        installed_identity = json.loads(Path("/etc/plurx/installation.json").read_bytes())
        if installed_identity["current_commit"] != a.candidate:
            raise RuntimeError("installed package source differs from dispatched exact candidate")
        receipt.check("native-installation-identity", installed_identity)
        run(["/usr/bin/plurx-control", "stop", "--confirm-interruption"], timeout=120)
        run(["/usr/bin/plurx-control", "startup", "set", "server", "off"])
        roots = json.loads(run(["/usr/bin/plurx-control", "trust-info"]).stdout)
        receipt.check("untouched-candidate-production-roots", roots)
        run(["useradd", "--create-home", "--shell", "/bin/bash", USER])
        run(["groupadd", "--force", "nopasswdlogin"])
        run(["usermod", "-a", "-G", "nopasswdlogin", USER])
        home = Path(pwd.getpwnam(USER).pw_dir)
        Path("/etc/X11/plurx-qualification.conf").write_text('''Section "Device"
 Identifier "PlurxDummy"
 Driver "dummy"
 VideoRam 256000
EndSection
Section "Monitor"
 Identifier "PlurxMonitor"
 HorizSync 28.0-80.0
 VertRefresh 48.0-75.0
 Modeline "1280x800" 83.50 1280 1352 1480 1680 800 803 809 831 -hsync +vsync
EndSection
Section "Screen"
 Identifier "PlurxScreen"
 Device "PlurxDummy"
 Monitor "PlurxMonitor"
 DefaultDepth 24
 SubSection "Display"
  Depth 24
  Modes "1280x800"
  Virtual 1280 800
 EndSubSection
EndSection
''')
        env = {k: v for k, v in os.environ.items() if k not in ("DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS", "XDG_SESSION_ID")}
        account = pwd.getpwnam(USER)
        unprivileged = ["setpriv", "--reuid", str(account.pw_uid), "--regid", str(account.pw_gid), "--init-groups"]
        check_refusal(SESSION_WORK, "root", [], env, 0, 0)
        check_refusal(SESSION_WORK, "headless", unprivileged, env, account.pw_uid, account.pw_gid)
        check_refusal(SESSION_WORK, "ssh-forwarded-env", unprivileged,
                      {**env, "SSH_CONNECTION": "127.0.0.1 1 127.0.0.1 2", "DISPLAY": "localhost:10.0"},
                      account.pw_uid, account.pw_gid)
        # Namespace-private /run and marker: never create a host container marker.
        check_refusal(SESSION_WORK, "container-marker", ["unshare", "--mount", "--fork", "bash", "-c",
            'set -eu; mount --make-rprivate /; mount -t tmpfs tmpfs /run; touch /run/.containerenv; '
            'exec setpriv --reuid "$1" --regid "$2" --init-groups "${@:3}"',
            "sh", str(account.pw_uid), str(account.pw_gid)], env, account.pw_uid, account.pw_gid)
        for kind in ("x11", "wayland"):
            output = SESSION_WORK / kind
            output.mkdir(parents=True)
            run(["chown", f"{USER}:{USER}", output])
            authorization(False)
            configure_session(kind, home, output, a.candidate)
            run(["systemctl", "restart", "lightdm"], timeout=60)
            deadline = time.monotonic() + 420
            while time.monotonic() < deadline:
                if (output / "request-allow").exists() and not (output / "allow-ready").exists():
                    authorization(True)
                    time.sleep(1)
                    (output / "allow-ready").touch()
                if (output / "session.json").exists():
                    value = json.loads((output / "session.json").read_bytes())
                    if value["status"] != "running":
                        if value["status"] != "passed":
                            raise RuntimeError(f"{kind} native session failed: {value.get('failure')}")
                        receipt.check("real-pam-" + kind, value)
                        break
                time.sleep(1)
            else:
                raise RuntimeError(f"no qualified {kind} PAM session receipt; hosted seat/display is unqualified")
            run(["systemctl", "stop", "lightdm"], timeout=60)
            run(["loginctl", "terminate-user", USER], timeout=30, check=False)
        shutil.copytree(SESSION_WORK, a.output / "native-session", dirs_exist_ok=True)
        receipt.value["limitations"] = ["Narrow fixture authorization is not a real user's interactive authentication.",
            "Virtual outputs are not physical monitors; reboot, old legacy adoption and production signed upgrades remain unqualified."]


if __name__ == "__main__":
    main()
