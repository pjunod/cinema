#!/usr/bin/env python3
"""Real-AppKit driver regressions; never accesses an installed server.

Run with a locally built arm64/x86_64 native app containing plurx-control:
  python3 clients/server-tray/macos/Tests/smoke-driver.py --app ABS_APP
The deadline case replaces the CLI in an owned app copy with a tiny native
fixture. It proves driver behavior only, never installed-service acceptance.
"""
import argparse
import json
from pathlib import Path
import shutil
import stat
import subprocess
import time


def run(arguments, timeout=15):
    return subprocess.run(arguments, capture_output=True, text=True, timeout=timeout, check=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", type=Path, required=True)
    options = parser.parse_args()
    app = options.app.resolve(strict=True)
    executable = app / "Contents/MacOS/Plurx Server"
    assert executable.is_file() and (app / "Contents/MacOS/plurx-control").is_file()
    scratch_root = Path(__file__).resolve().parents[4] / "target/mac-smoke-driver"
    scratch_root.mkdir(parents=True, exist_ok=True)
    # Keep all evidence in a durable, unique task-local directory.
    scratch = scratch_root / f"run-{time.time_ns()}"
    scratch.mkdir(mode=0o700)
    receipt = scratch / "quit.json"
    result = run([str(executable), "--smoke-receipt", str(receipt), "--smoke-action", "quit",
                  "--smoke-timeout", "10", "--smoke-installation", str(scratch / "absent.json")])
    assert result.returncode == 1, (result.returncode, result.stderr)
    observed = json.loads(receipt.read_bytes())
    assert observed["session"]["gui_session"], "this test requires an owned console GUI session"
    assert observed["tray"]["status_item_present"] and observed["tray"]["button_image_template"]
    assert observed["menu"]["open_count"] == observed["menu"]["close_count"] == 1
    assert observed["controller"]["first_status"]["ok"] is False
    assert observed["action"]["service_state_after"] == "unknown"
    assert observed["quit"] == {"via": "quit_action", "controller_calls_during_quit": 0}
    assert all(observed["invariants"][key] for key in
               ["unknown_not_stopped", "start_only_when_stopped", "quit_sent_no_server_command"])
    assert stat.S_IMODE(receipt.stat().st_mode) == 0o600
    assert receipt.stat().st_size <= 65536
    invalid = scratch / "invalid-must-not-exist.json"
    for arguments in [
        ["--smoke-action", "stop"],
        ["--smoke-receipt", str(invalid), "--smoke-timeout", "601"],
        ["--smoke-receipt", str(invalid), "--smoke-action", "start"],
        ["--smoke-receipt", "relative.json"],
        ["--smoke-receipt", str(invalid), "--smoke-receipt", str(invalid)],
    ]:
        result = run([str(executable), *arguments], timeout=5)
        assert result.returncode == 2, (arguments, result.returncode, result.stderr)
        assert not invalid.exists()

    fixture = scratch / "Plurx Server.app"
    shutil.copytree(app, fixture, symlinks=True)
    mutations = scratch / "unexpected-mutations"
    status = {"schema_version": 1, "ok": True, "error": None, "result": {
        "kind": "status", "installation_id": "00000000-0000-4000-8000-000000000001",
        "service_state": "running", "version": "0.7.0", "browser_origin": "http://127.0.0.1:32400",
        "server_startup": {"state": "disabled", "detail": None},
        "tray_startup": {"state": "unknown", "detail": None}, "transaction_id": None,
    }}
    source = scratch / "fixture.c"
    source.write_text("""#include <stdio.h>
#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
  for (int i=1; i<argc; i++) {
    if (!strcmp(argv[i], "stop") || !strcmp(argv[i], "restart") || !strcmp(argv[i], "start")) {
      FILE *log = fopen(MUTATIONS, "a"); if (log) { fputs(argv[i], log); fclose(log); }
      return 70;
    }
  }
  usleep(500000); fputs(STATUS, stdout); return 0;
}
""".replace("MUTATIONS", json.dumps(str(mutations)))
                      .replace("STATUS", json.dumps(json.dumps(status))))
    controller = fixture / "Contents/MacOS/plurx-control"
    # Remove the copied executable before linking; the original stays intact.
    controller.unlink()
    subprocess.run(["xcrun", "clang", str(source), "-o", str(controller)], check=True)
    for item in [controller, fixture]:
        subprocess.run(["codesign", "--force", "--options", "runtime", "--timestamp=none", "--sign", "-", str(item)],
                       capture_output=True, check=True)
    deadline = scratch / "deadline.json"
    result = run([str(fixture / "Contents/MacOS/Plurx Server"), "--smoke-receipt", str(deadline),
                  "--smoke-action", "stop", "--smoke-timeout", "1"], timeout=5)
    assert result.returncode == 1, (result.returncode, result.stderr)
    observed = json.loads(deadline.read_bytes())
    assert observed["controller"]["first_status"]["ok"] is True, observed
    assert observed["menu"]["open_count"] == observed["menu"]["close_count"] == 1, observed
    assert observed["action"]["error"] == "smoke deadline exceeded", observed
    assert observed["elapsed_ms"] < 2000, observed
    assert observed["quit"] == {"via": "quit_action", "controller_calls_during_quit": 0}
    assert not mutations.exists(), "an expired smoke must not start its requested Stop"
    # Positive driver paths use only private fixture state. Log every command
    # to prove a completed status read was issued after the action, even when
    # the menu's earlier read is still in flight.
    calls = scratch / "calls"
    state = scratch / "stopped"
    stopped_status = json.loads(json.dumps(status))
    stopped_status["result"]["service_state"] = "stopped"
    operation = lambda name, after: json.dumps({"schema_version": 1, "ok": True, "error": None,
        "result": {"kind": "operation", "operation": name, "service_state": after}})
    source.write_text("""#include <stdio.h>
#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
  const char *command = argv[argc-1];
  for (int i=1; i<argc; i++) if (!strcmp(argv[i], "stop") || !strcmp(argv[i], "restart")) command = argv[i];
  FILE *log = fopen(CALLS, "a"); if (!log) return 70;
  fprintf(log, "%s\\n", command); fclose(log);
  if (!strcmp(command, "stop")) {
    FILE *state = fopen(STATE, "w"); if (!state) return 70; fclose(state);
    fputs(STOP_RESULT, stdout); return 0;
  }
  if (!strcmp(command, "restart")) { unlink(STATE); fputs(RESTART_RESULT, stdout); return 0; }
  int stopped = access(STATE, F_OK) == 0;
  usleep(500000); fputs(stopped ? STOPPED : RUNNING, stdout); return 0;
}
""".replace("CALLS", json.dumps(str(calls))).replace("STATE", json.dumps(str(state)))
       .replace("STOPPED", json.dumps(json.dumps(stopped_status)))
       .replace("RUNNING", json.dumps(json.dumps(status)))
       .replace("RESTART_RESULT", json.dumps(operation("restart", "running")))
       .replace("STOP_RESULT", json.dumps(operation("stop", "stopped"))))
    controller.unlink()
    subprocess.run(["xcrun", "clang", str(source), "-o", str(controller)], check=True)
    for item in [controller, fixture]:
        subprocess.run(["codesign", "--force", "--options", "runtime", "--timestamp=none", "--sign", "-", str(item)],
                       capture_output=True, check=True)
    for action in ["stop", "restart"]:
        calls.unlink(missing_ok=True)
        state.unlink(missing_ok=True)
        output = scratch / f"{action}.json"
        result = run([str(fixture / "Contents/MacOS/Plurx Server"), "--smoke-receipt", str(output),
                      "--smoke-action", action, "--smoke-timeout", "10"])
        assert result.returncode == 0, (action, result.returncode, result.stderr, output.read_text())
        observed = json.loads(output.read_bytes())
        trace = calls.read_text().splitlines()
        assert trace.count(action) == 1 and "status" in trace[trace.index(action) + 1:], trace
        assert observed["action"]["ok"] and observed["action"]["ui_alive_after"]
        assert observed["action"]["service_state_after"] == ("stopped" if action == "stop" else "running")
        assert observed["quit"]["controller_calls_during_quit"] == 0
    print(f"PASS: Quit/Unknown, 5 usage refusals, menu deadline, owned-fixture Stop/Restart; evidence {scratch}")
    print("Driver checks only; no installed-service or physical Intel acceptance.")


if __name__ == "__main__":
    main()
