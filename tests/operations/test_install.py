"""`make install` is one command per platform, and each one really does what
deploy/README.md says it does.

The installer is a shell script, and the way a shell script goes wrong is
quietly: a `sudo` in the wrong place, a plist rendered with the placeholder
still in it, an upgrade that replaces the binary under a running service, a
docker path that reads `.env` from the wrong directory. None of that shows up
in a dry-run listing, so these tests execute the real `deploy/install` on
every platform path with the host tools stubbed out -- `sudo`, `systemctl`,
`launchctl`, `docker`, `make`, `curl` -- and assert on what was invoked, in
what order, and on the files the script actually wrote.

`DESTDIR` roots every absolute install path (the standard convention), so the
Linux path installs into a temporary tree instead of the machine running the
tests. The macOS path is rooted by `HOME` and `--prefix`. The Windows path is
PowerShell and cannot execute here; its bash dispatcher is exercised with a
stub `pwsh`, and the script itself is held to the recipe the runbook prints.
"""

from __future__ import annotations

import os
import plistlib
from pathlib import Path
import re
import shutil
import stat
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
INSTALL = ROOT / "deploy" / "install"
INSTALL_PS1 = ROOT / "deploy" / "install.ps1"


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def write_executable(path: Path, body: str) -> None:
    path.write_text(body, encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


class Host:
    """A fake host: a stub bin directory whose tools log every invocation."""

    def __init__(self, tmp: Path):
        self.tmp = tmp
        self.bin = tmp / "stubs"
        self.bin.mkdir()
        # The only real tools the script may see are the coreutils it needs;
        # the machine's own ffmpeg, systemctl, or docker must never leak in,
        # or the "missing ffmpeg" path could not be tested on a host that has one.
        self.sysbin = tmp / "sysbin"
        self.sysbin.mkdir()
        for tool in (
            "awk", "bash", "cat", "chmod", "cp", "dirname", "env", "grep", "head", "mkdir", "mktemp", "mv",
            "printf", "rm", "sed", "sh", "sleep", "tail", "tr", "uname",
        ):
            found = shutil.which(tool)
            if found:
                os.symlink(found, self.sysbin / tool)
        self.log = tmp / "calls.log"
        self.log.touch()
        self.home = tmp / "home"
        self.home.mkdir()
        self.destdir = tmp / "root"
        self.destdir.mkdir()
        self.fake_binary = tmp / "plurxd-prebuilt"
        write_executable(self.fake_binary, "#!/bin/sh\necho plurxd 0.0.0-test\n")
        # Every stub appends `name args...` to the log; specific stubs add
        # behaviour on top.
        self.stub("sudo", 'exec "$@"\n')
        # A fixed non-root identity, so "sudo only where needed" is provable
        # even when the suite itself runs as root (CI containers do).
        self.stub("id", 'case "$1" in -u) echo 1000 ;; -g) echo 1000 ;; *) echo "uid=1000(test) gid=1000(test)" ;; esac\n')
        self.stub("ffmpeg", "")
        self.stub("ffprobe", "")
        self.stub(
            "curl",
            'case "$*" in *readyz*) exit 0 ;; *api/v1/server*) echo \'{"version":"0.0.0-test"}\' ;; esac\n',
        )
        # `install` is the one stub that has to do its job, because later
        # assertions read what it wrote. It honours -d and copies otherwise;
        # ownership flags are logged, not applied (no root here).
        self.stub(
            "install",
            "mode=copy; args=()\n"
            'while [ $# -gt 0 ]; do case "$1" in -d) mode=dir ;; -m|-o|-g) shift ;; *) args+=("$1") ;; esac; shift; done\n'
            'if [ "$mode" = dir ]; then mkdir -p "${args[@]}" 2>/dev/null || true; '
            'else mkdir -p "$(dirname "${args[1]}")"; cp "${args[0]}" "${args[1]}"; chmod 755 "${args[1]}"; fi\n',
        )

    def stub(self, name: str, body: str) -> None:
        write_executable(
            self.bin / name,
            "#!/bin/bash\n"
            f'printf \'%s\\n\' "{name} $*" >> "{self.log}"\n'
            + body,
        )

    def calls(self) -> list[str]:
        return [line for line in self.log.read_text(encoding="utf-8").splitlines() if line]

    def run(self, *args: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
        merged = {
            "PATH": f"{self.bin}:{self.sysbin}",
            "HOME": str(self.home),
            "DESTDIR": str(self.destdir),
            "TMPDIR": str(self.tmp),
            "LC_ALL": "C",
        }
        merged.update(env or {})
        return subprocess.run(
            [str(INSTALL), *args],
            cwd=self.tmp,
            env=merged,
            capture_output=True,
            text=True,
            timeout=60,
        )


class InstallerCase(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="plurx-install-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.host = Host(self.tmp)

    def assertOrdered(self, calls: list[str], *needles: str):
        position = -1
        for needle in needles:
            matches = [i for i, call in enumerate(calls) if needle in call and i > position]
            self.assertTrue(matches, f"{needle!r} not found after position {position} in:\n" + "\n".join(calls))
            position = matches[0]

    # ---- linux ----------------------------------------------------------------

    def test_linux_install_is_the_systemd_recipe_the_runbook_prints(self):
        self.host.stub("systemctl", 'case "$1" in is-active) exit 3 ;; esac\n')
        self.host.stub("useradd", "")
        self.host.stub("getent", "exit 2\n")
        result = self.host.run("linux", "--binary", str(self.host.fake_binary))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        calls = self.host.calls()
        dest = self.host.destdir
        self.assertOrdered(
            calls,
            "getent passwd plurx",
            "sudo useradd --system --home /var/lib/plurx --shell /usr/sbin/nologin plurx",
            f"sudo install -d -m 0750 -o plurx -g plurx {dest}/var/lib/plurx",
            # DESTDIR is writable here, so no sudo; on a real host /usr/local/bin
            # is root-owned and the same call goes through sudo.
            f"install -m 0755 {self.host.fake_binary} {dest}/usr/local/bin/plurxd",
            f"sudo install -m 0644 {ROOT}/deploy/plurxd.service {dest}/etc/systemd/system/plurxd.service",
            "sudo systemctl daemon-reload",
            "sudo systemctl enable --now plurxd",
            "curl",
        )
        self.assertNotIn("systemctl stop", "\n".join(calls), "a fresh install has nothing to stop")
        installed = dest / "usr/local/bin/plurxd"
        self.assertTrue(installed.exists() and os.access(installed, os.X_OK))
        self.assertEqual(
            (dest / "etc/systemd/system/plurxd.service").read_text(encoding="utf-8"),
            read("deploy/plurxd.service"),
            "the default prefix installs the tracked unit byte for byte",
        )
        self.assertIn("ready:", result.stdout)
        self.assertIn("0.0.0-test", result.stdout, "the install ends by printing what the server reports")
        self.assertIn("open http://127.0.0.1:32400", result.stdout)

    def test_linux_upgrade_stops_the_running_service_before_replacing_its_binary(self):
        self.host.stub("systemctl", 'case "$1" in is-active) exit 0 ;; esac\n')
        self.host.stub("getent", "")  # the plurx user already exists
        result = self.host.run("linux", "--binary", str(self.host.fake_binary))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        calls = self.host.calls()
        self.assertOrdered(calls, "systemctl stop plurxd", "install -m 0755", "systemctl daemon-reload", "systemctl start plurxd")
        self.assertNotIn("useradd", "\n".join(calls))
        self.assertNotIn("enable --now", "\n".join(calls), "an upgrade restarts; it does not re-enable")

    def test_linux_upgrade_keeps_the_operators_unit_and_data_directory(self):
        self.host.stub("systemctl", 'case "$1" in is-active) exit 0 ;; esac\n')
        self.host.stub("getent", "")
        unit = self.host.destdir / "etc/systemd/system/plurxd.service"
        unit.parent.mkdir(parents=True)
        unit.write_text("[Service]\nSupplementaryGroups=render\nProtectHome=read-only\n")
        data = self.host.destdir / "var/lib/plurx"
        data.mkdir(parents=True)
        result = self.host.run("linux", "--binary", str(self.host.fake_binary))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(unit.read_text(), "[Service]\nSupplementaryGroups=render\nProtectHome=read-only\n", "the operator's unit was overwritten")
        self.assertIn("keeping the existing", result.stdout)
        calls = "\n".join(self.host.calls())
        self.assertNotIn("install -m 0644", calls)
        self.assertNotIn("install -d -m 0750", calls, "an existing data directory keeps its mode")

    def test_linux_prefix_moves_the_binary_and_the_unit_agrees(self):
        self.host.stub("systemctl", 'case "$1" in is-active) exit 3 ;; esac\n')
        self.host.stub("useradd", "")
        self.host.stub("getent", "exit 2\n")
        prefix = self.tmp / "opt" / "plurx"
        result = self.host.run("linux", "--binary", str(self.host.fake_binary), "--prefix", str(prefix))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        unit = (self.host.destdir / "etc/systemd/system/plurxd.service").read_text(encoding="utf-8")
        self.assertEqual(unit, read("deploy/plurxd.service"), "the base unit stays the tracked one; the prefix is a drop-in")
        dropin = self.host.destdir / "etc/systemd/system/plurxd.service.d/10-prefix.conf"
        self.assertEqual(dropin.read_text(encoding="utf-8"), f"[Service]\nExecStart=\nExecStart={prefix}/bin/plurxd run\n")
        self.assertTrue((prefix / "bin" / "plurxd").exists())
        # And the uninstall finds the binary where the drop-in says it is.
        self.host.stub("systemctl", "")
        result = self.host.run("linux", "--uninstall")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse((prefix / "bin" / "plurxd").exists(), "uninstall removed the wrong path")
        self.assertFalse(dropin.exists())

    def test_a_prefix_under_home_is_refused_because_the_unit_hides_home(self):
        self.host.stub("systemctl", "")
        result = self.host.run("linux", "--binary", str(self.host.fake_binary), "--prefix", "/home/bob/.local")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ProtectHome", result.stderr)
        self.assertEqual(self.host.calls(), [], "refused before anything was installed")

    def test_special_characters_in_a_prefix_survive_rendering(self):
        self.host.stub("systemctl", 'case "$1" in is-active) exit 3 ;; esac\n')
        self.host.stub("useradd", "")
        self.host.stub("getent", "exit 2\n")
        prefix = self.tmp / "a&b|c"
        result = self.host.run("linux", "--binary", str(self.host.fake_binary), "--prefix", str(prefix))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        dropin = self.host.destdir / "etc/systemd/system/plurxd.service.d/10-prefix.conf"
        self.assertIn(f"ExecStart={prefix}/bin/plurxd run", dropin.read_text(encoding="utf-8"))
        self.host.stub("launchctl", 'case "$1" in bootout) exit 3 ;; esac\n')
        result = self.host.run("macos", "--binary", str(self.host.fake_binary), "--prefix", str(prefix))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        with (self.host.home / "Library/LaunchAgents/com.plurx.plurxd.plist").open("rb") as handle:
            self.assertEqual(plistlib.load(handle)["ProgramArguments"][0], f"{prefix}/bin/plurxd")

    def test_an_install_that_never_becomes_ready_fails_and_names_the_log(self):
        self.host.stub("curl", "exit 7\n")
        self.host.stub("systemctl", 'case "$1" in is-active) exit 3 ;; esac\n')
        self.host.stub("useradd", "")
        self.host.stub("getent", "exit 2\n")
        self.host.stub("launchctl", "")
        expected = {
            "linux": "journalctl -u plurxd",
            "macos": "Library/Logs/plurxd.log",
        }
        for mode, log in expected.items():
            self.host.log.write_text("")
            result = self.host.run(mode, "--binary", str(self.host.fake_binary), "--prefix", str(self.tmp / mode), env={"PLURX_INSTALL_READY_TIMEOUT": "2"})
            self.assertNotEqual(result.returncode, 0, f"{mode} claimed success with no server answering")
            self.assertIn("did not become ready within 2s", result.stderr, mode)
            self.assertIn(log, result.stderr, mode)
            self.assertNotIn("ready:", result.stdout, mode)

    def test_linux_uninstall_removes_the_service_and_keeps_the_data(self):
        self.host.stub("systemctl", "")
        data = self.host.destdir / "var/lib/plurx"
        data.mkdir(parents=True)
        (data / "plurx.db").write_text("precious")
        unit = self.host.destdir / "etc/systemd/system/plurxd.service"
        unit.parent.mkdir(parents=True)
        unit.write_text("[Unit]\n")
        binary = self.host.destdir / "usr/local/bin/plurxd"
        binary.parent.mkdir(parents=True)
        binary.write_text("bin")
        result = self.host.run("linux", "--uninstall")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertOrdered(self.host.calls(), "sudo systemctl disable --now plurxd", f"sudo rm -f {unit}", "sudo systemctl daemon-reload")
        self.assertFalse(unit.exists())
        self.assertFalse(binary.exists())
        self.assertEqual((data / "plurx.db").read_text(), "precious")
        self.assertIn("kept /var/lib/plurx", result.stdout)

    def test_a_missing_ffmpeg_is_installed_not_reported(self):
        os.remove(self.host.bin / "ffmpeg")
        self.host.stub("apt-get", "")
        self.host.stub("systemctl", 'case "$1" in is-active) exit 3 ;; esac\n')
        self.host.stub("useradd", "")
        self.host.stub("getent", "exit 2\n")
        # apt-get "installs" it: the stub creates the ffmpeg stub on the way.
        self.host.stub("apt-get", f'printf \'#!/bin/sh\\n\' > "{self.host.bin}/ffmpeg"; chmod +x "{self.host.bin}/ffmpeg"\n')
        result = self.host.run("linux", "--binary", str(self.host.fake_binary))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertOrdered(self.host.calls(), "sudo apt-get install -y --no-install-recommends ffmpeg", "install -m 0755")

    # ---- macos ----------------------------------------------------------------

    def test_macos_install_renders_the_agent_for_this_user_and_bootstraps_it(self):
        self.host.stub("launchctl", 'case "$1" in bootout) exit 3 ;; print) echo "state = running" ;; esac\n')
        prefix = self.tmp / "homebrew"
        result = self.host.run("macos", "--binary", str(self.host.fake_binary), "--prefix", str(prefix))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        plist_path = self.host.home / "Library/LaunchAgents/com.plurx.plurxd.plist"
        self.assertTrue(plist_path.exists(), "the agent plist was not written")
        rendered = plist_path.read_text(encoding="utf-8")
        self.assertNotIn("YOUR_USERNAME", rendered)
        with plist_path.open("rb") as handle:
            plist = plistlib.load(handle)
        self.assertEqual(plist["Label"], "com.plurx.plurxd")
        self.assertEqual(plist["ProgramArguments"], [f"{prefix}/bin/plurxd", "run"])
        env = plist["EnvironmentVariables"]
        self.assertEqual(env["PLURX_FFMPEG"], str(self.host.bin / "ffmpeg"), "the plist names the ffmpeg that was on PATH")
        self.assertEqual(env["PLURX_FFPROBE"], str(self.host.bin / "ffprobe"))
        self.assertEqual(env["PLURX_DATA_DIR"], f"{self.host.home}/Library/Application Support/plurx")
        self.assertEqual(plist["StandardOutPath"], f"{self.host.home}/Library/Logs/plurxd.log")
        self.assertTrue((self.host.home / "Library/Application Support/plurx").is_dir())
        self.assertTrue((prefix / "bin" / "plurxd").exists())
        uid = 1000
        self.assertOrdered(
            self.host.calls(),
            f"launchctl bootout gui/{uid}/com.plurx.plurxd",
            "install -m 0755",
            f"launchctl bootstrap gui/{uid} {plist_path}",
            f"launchctl enable gui/{uid}/com.plurx.plurxd",
            f"launchctl kickstart -k gui/{uid}/com.plurx.plurxd",
            "curl",
        )
        self.assertNotIn("sudo", "\n".join(self.host.calls()), "a user-owned prefix needs no sudo")

    def test_macos_upgrade_keeps_the_operators_plist(self):
        self.host.stub("launchctl", "")
        agents = self.host.home / "Library/LaunchAgents"
        agents.mkdir(parents=True)
        edited = "<plist version=\"1.0\"><dict><key>ProgramArguments</key><array><string>/opt/x/bin/plurxd</string></array></dict></plist>\n"
        (agents / "com.plurx.plurxd.plist").write_text(edited)
        result = self.host.run("macos", "--binary", str(self.host.fake_binary), "--prefix", str(self.tmp / "hb"))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual((agents / "com.plurx.plurxd.plist").read_text(), edited)
        self.assertIn("keeping the existing", result.stdout)

    def test_macos_uninstall_boots_the_agent_out_and_keeps_application_support(self):
        self.host.stub("launchctl", "")
        prefix = self.tmp / "homebrew"
        (prefix / "bin").mkdir(parents=True)
        (prefix / "bin" / "plurxd").write_text("bin")
        agents = self.host.home / "Library/LaunchAgents"
        agents.mkdir(parents=True)
        (agents / "com.plurx.plurxd.plist").write_text(
            f"<plist><dict><key>ProgramArguments</key><array><string>{prefix}/bin/plurxd</string><string>run</string></array></dict></plist>\n"
        )
        support = self.host.home / "Library/Application Support/plurx"
        support.mkdir(parents=True)
        (support / "plurx.db").write_text("precious")
        # No --prefix: the uninstall reads the binary's path from the plist.
        result = self.host.run("macos", "--uninstall")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(f"launchctl bootout gui/{1000}/com.plurx.plurxd", self.host.calls())
        self.assertFalse((agents / "com.plurx.plurxd.plist").exists())
        self.assertFalse((prefix / "bin" / "plurxd").exists())
        self.assertEqual((support / "plurx.db").read_text(), "precious")

    # ---- docker ----------------------------------------------------------------

    def test_docker_install_creates_the_untracked_inputs_then_runs_docker_up(self):
        # A copy of deploy/, so the real checkout's untracked files are never
        # touched, under a root whose `make` is a stub.
        fake_root = self.tmp / "checkout"
        shutil.copytree(ROOT / "deploy", fake_root / "deploy")
        for stale in ("deploy/.env", "deploy/docker-compose.override.yml"):
            path = fake_root / stale
            if path.exists():
                path.unlink()
        (fake_root / "Makefile").write_text("docker-up:\n\t@echo stub\n")
        # The example's data directory is /srv/plurx; point it into tmp so the
        # test never depends on (or touches) the host's own deployment.
        data = self.tmp / "srv-plurx"
        example = fake_root / "deploy" / ".env.example"
        example.write_text(example.read_text(encoding="utf-8").replace("PLURX_DATA=/srv/plurx", f"PLURX_DATA={data}"), encoding="utf-8")
        self.host.stub("docker", "")
        self.host.stub("make", "")
        installer = fake_root / "deploy" / "install"
        result = subprocess.run(
            [str(installer), "docker"],
            cwd=self.tmp,
            env={"PATH": f"{self.host.bin}:{self.host.sysbin}", "HOME": str(self.host.home), "TMPDIR": str(self.tmp)},
            capture_output=True,
            text=True,
            timeout=60,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        env_file = (fake_root / "deploy" / ".env").read_text(encoding="utf-8")
        self.assertIn(f"PUID={1000}\n", env_file)
        self.assertIn(f"PGID={1000}\n", env_file)
        self.assertIn(f"PLURX_DATA={data}", env_file)
        self.assertTrue((fake_root / "deploy" / "docker-compose.override.yml").exists())
        self.assertOrdered(
            self.host.calls(),
            "docker compose version",
            f"install -d -o {1000} -g {1000} {data}",
            "make docker-up",
            "curl",
        )
        self.assertTrue(data.is_dir())
        self.assertNotIn("-f ", " ".join(c for c in self.host.calls() if c.startswith("make")))

    def test_docker_install_respects_an_existing_env_file(self):
        fake_root = self.tmp / "checkout"
        shutil.copytree(ROOT / "deploy", fake_root / "deploy")
        data = self.tmp / "data"
        data.mkdir()
        # Quoted, commented, CRLF: the shapes Compose accepts and a hand edit produces.
        (fake_root / "deploy" / ".env").write_text(f"PLURX_HTTP_PORT=32410\r\nPLURX_DATA=\"{data}\" # host path\nPUID=4242\nPGID=4242\n")
        (fake_root / "deploy" / "docker-compose.override.yml").write_text("services: {}\n")
        (fake_root / "Makefile").write_text("docker-up:\n\t@echo stub\n")
        self.host.stub("docker", "")
        self.host.stub("make", "")
        result = subprocess.run(
            [str(fake_root / "deploy" / "install"), "docker"],
            cwd=self.tmp,
            env={"PATH": f"{self.host.bin}:{self.host.sysbin}", "HOME": str(self.host.home), "TMPDIR": str(self.tmp)},
            capture_output=True,
            text=True,
            timeout=60,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("PUID=4242", (fake_root / "deploy" / ".env").read_text(encoding="utf-8"))
        calls = "\n".join(self.host.calls())
        self.assertNotIn("install -d", calls, "an existing data directory is left alone")
        self.assertIn("127.0.0.1:32410/readyz", calls, "readiness is probed on the configured port")

    # ---- binary, windows, dry-run ---------------------------------------------

    def test_binary_install_puts_plurxd_on_path_and_installs_no_service(self):
        prefix = self.tmp / "local"
        result = self.host.run("binary", "--binary", str(self.host.fake_binary), "--prefix", str(prefix))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((prefix / "bin" / "plurxd").exists())
        calls = "\n".join(self.host.calls())
        for tool in ("systemctl", "launchctl", "docker", "sudo"):
            self.assertNotIn(tool, calls)
        self.assertIn("plurxd run", result.stdout)

    def test_windows_mode_hands_every_flag_to_the_powershell_installer(self):
        self.host.stub("pwsh", "")
        result = self.host.run("windows", "--uninstall", "--dry-run", "--binary", str(self.host.fake_binary), "--prefix", "C:/plurx")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(
            f"+ pwsh -NoProfile -ExecutionPolicy Bypass -File {ROOT}/deploy/install.ps1 -Uninstall -DryRun -Binary {self.host.fake_binary} -InstallDir C:/plurx",
            result.stdout,
        )

    def test_windows_installer_carries_the_runbook_recipe(self):
        script = INSTALL_PS1.read_text(encoding="utf-8")
        for needle in (
            "cargo' @('build', '--locked', '--release', '-p', 'plurxd')",
            "'service', 'install', '--config', $config",
            "'service', 'uninstall'",
            "protocol=$($rule[1])",
            "@($httpRule, 'TCP', '32400')",
            "@($gdmRule, 'UDP', '32414')",
            "Gyan.FFmpeg",
            "'--scope', 'machine'",
            "GetEnvironmentVariable('PLURX_FFMPEG', 'Machine')",
            "$PSNativeCommandUseErrorActionPreference = $false",
            "-Verb RunAs",
            "Win32_Service",
            "[switch]$Uninstall",
            "[switch]$DryRun",
            "/readyz",
        ):
            self.assertIn(needle, script)
        self.assertIn("Test-Admin", script, "the service and firewall steps need elevation, and the script elevates")
        pwsh = shutil.which("pwsh")
        if pwsh:
            parse = subprocess.run(
                [pwsh, "-NoProfile", "-Command",
                 "$t=$null;$e=$null;[System.Management.Automation.Language.Parser]::ParseFile($args[0],[ref]$t,[ref]$e)|Out-Null;"
                 "if($e.Count){$e|%{\"$($_.Extent.StartLineNumber): $($_.Message)\"};exit 1}",
                 str(INSTALL_PS1)],
                capture_output=True, text=True, timeout=60,
            )
            self.assertEqual(parse.returncode, 0, parse.stdout + parse.stderr)
        # The runbook and the installer agree on where things live.
        readme = read("deploy/README.md")
        self.assertIn("deploy\\install.ps1", readme)
        self.assertIn("C:\\ProgramData\\plurx\\plurx.toml", readme)

    def test_dry_run_prints_the_plan_and_touches_nothing(self):
        # No stubs at all beyond ffmpeg: a dry run must not need sudo,
        # systemctl, or launchctl to exist, and must not create anything.
        for stale in ("sudo", "curl", "install"):
            os.remove(self.host.bin / stale)
        before = sorted(str(p) for p in self.host.destdir.rglob("*"))
        fake_root = self.tmp / "checkout"
        shutil.copytree(ROOT / "deploy", fake_root / "deploy")
        for mode in ("linux", "macos", "docker", "binary"):
            installer = str(fake_root / "deploy" / "install") if mode == "docker" else str(INSTALL)
            result = subprocess.run(
                [installer, mode, "--dry-run", "--binary", str(self.host.fake_binary), "--prefix", str(self.tmp / "p")],
                cwd=self.tmp, env={"PATH": f"{self.host.bin}:{self.host.sysbin}", "HOME": str(self.host.home), "DESTDIR": str(self.host.destdir), "TMPDIR": str(self.tmp)},
                capture_output=True, text=True, timeout=60,
            )
            self.assertEqual(result.returncode, 0, f"{mode}: " + result.stdout + result.stderr)
            self.assertTrue(any(line.startswith("+ ") for line in result.stdout.splitlines()), mode)
            if mode == "binary":
                self.assertIn("plurxd run", result.stdout, mode)
            else:
                self.assertIn("would wait for http://127.0.0.1:32400/readyz", result.stdout, mode)
        after = sorted(str(p) for p in self.host.destdir.rglob("*"))
        self.assertEqual(before, after)
        self.assertEqual([c for c in self.host.calls() if not c.startswith("id ")], [], "a dry run invokes no host tool")

    # ---- the make targets and the docs --------------------------------------

    def test_make_targets_route_to_the_installer(self):
        makefile = read("Makefile")
        expected = {
            "install": "auto",
            "install-linux": "linux",
            "install-macos": "macos",
            "install-windows": "windows",
            "install-docker": "docker",
            "install-binary": "binary",
        }
        for target, mode in expected.items():
            self.assertRegex(
                makefile,
                re.compile(rf"^{re.escape(target)}: ## .*\n\t@deploy/install {mode} \$\(INSTALL_FLAGS\)\n", re.M),
                f"`make {target}` must run `deploy/install {mode}`",
            )
        self.assertRegex(makefile, re.compile(r"^uninstall: ## .*\n\t@deploy/install auto --uninstall \$\(INSTALL_FLAGS\)\n", re.M))
        self.assertRegex(makefile, re.compile(r"^uninstall-docker: ## .*\n\t@deploy/install docker --uninstall \$\(INSTALL_FLAGS\)\n", re.M))
        self.assertTrue(os.access(INSTALL, os.X_OK), "deploy/install must be executable")
        subprocess.run(["bash", "-n", str(INSTALL)], check=True)

    def test_installer_stays_bash_3_2_clean_for_macos(self):
        script = "\n".join(
            line for line in INSTALL.read_text(encoding="utf-8").splitlines() if not line.lstrip().startswith("#")
        )
        for bashism in ("declare -A", "mapfile", "readarray", ",,}", "^^}", "&>>"):
            self.assertNotIn(bashism, script, f"{bashism!r} is not available in macOS's /bin/bash 3.2")

    def test_the_runbooks_lead_with_the_one_command(self):
        deploy_readme = read("deploy/README.md")
        for section, command in (
            ("## Run as a service — Windows", "deploy\\install.ps1"),
            ("## Run as a service — systemd (Linux)", "make install"),
            ("## Run as a service — launchd (macOS)", "make install"),
            ("## Docker / Compose", "make install-docker"),
        ):
            start = deploy_readme.index(section)
            body = deploy_readme[start:start + 1500]
            self.assertIn(command, body, f"{section} does not lead with `{command}`")
        readme = read("README.md")
        for target in ("make install", "make install-docker", "make install-binary"):
            self.assertIn(target, readme)
        operations = read("docs/OPERATIONS.md")
        self.assertIn("make install", operations)


if __name__ == "__main__":
    unittest.main()
