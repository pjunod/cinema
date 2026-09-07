"""`scripts/ship --status` asks the nodes what they are running.

The probe exists because `--deploy` reports what Ansible did, which is a
different fact from what a node is now serving. Believing the first is how a
node sits on an old binary for a week without anybody noticing.

These tests run the real script against local HTTP servers standing in for
nodes, so they exercise the parsing and the exit reporting rather than
asserting the shape of a string somebody could change without changing
behaviour.
"""

from __future__ import annotations

import http.server
import socket
import subprocess
import threading
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SHIP = ROOT / "scripts" / "ship"

METRICS = (
    'plurx_build_info{version="0.3.0",build="6e225342"} 1\n'
    "plurx_vod_gets_serving 7\n"
    "plurx_vod_blocked_gets_waiting 2\n"
    "plurx_vod_blocked_get_cap 64\n"
)


class FakeNode:
    """One node's `/readyz` and `/metrics`, on a real socket."""

    def __init__(self, ready: str = "ready\n", metrics: str = METRICS) -> None:
        self.ready = ready
        self.metrics = metrics
        node = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args: object) -> None:  # noqa: D102
                pass

            def do_GET(self) -> None:  # noqa: N802
                if self.path == "/readyz":
                    body, status = node.ready.encode(), 200 if node.ready.strip() == "ready" else 503
                elif self.path == "/metrics":
                    body, status = node.metrics.encode(), 200
                else:
                    body, status = b"ok\n", 200
                self.send_response(status)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        self._server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
        self.port = self._server.server_address[1]
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)

    def __enter__(self) -> "FakeNode":
        self._thread.start()
        return self

    def __exit__(self, *exc: object) -> None:
        self._server.shutdown()
        self._server.server_close()


def free_port() -> int:
    """A port with nothing on it, for the node that must not answer."""
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def run_status(nodes: str, port: int) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["bash", str(SHIP), "--status"],
        cwd=ROOT,
        env={
            "PATH": "/usr/bin:/bin:/usr/local/bin",
            "HOME": "/tmp",
            "PLURX_NODES": nodes,
            "PLURX_PORT": str(port),
        },
        capture_output=True,
        text=True,
        timeout=90,
    )


class ShipStatusProbeCase(unittest.TestCase):
    def test_a_ready_node_reports_its_build_and_its_load(self) -> None:
        """The three facts a deploy is supposed to establish, read back."""
        with FakeNode() as node:
            result = run_status("127.0.0.1", node.port)
        self.assertIn("ready", result.stdout)
        self.assertIn(
            "0.3.0+6e225342",
            result.stdout,
            "the build is the whole point: a node on yesterday's binary looks "
            "identical to one on today's until somebody asks it",
        )
        self.assertIn("7", result.stdout, "cache-hit GETs in flight")
        self.assertIn("2/64", result.stdout, "blocked GETs against the cap they are admitted to")
        self.assertEqual(result.returncode, 0)

    def test_a_node_that_does_not_answer_is_named_not_skipped(self) -> None:
        """Silence is the failure mode a probe most needs to report.

        A node that is down answers nothing, so a probe that only prints what
        came back would show a shorter, entirely green list — the exact
        impression a dead node should not create.
        """
        result = run_status("127.0.0.1", free_port())
        self.assertIn("127.0.0.1", result.stdout)
        self.assertIn("no answer", result.stdout)
        self.assertIn("not ready, not answering, or not on the same build", result.stdout)
        self.assertNotEqual(result.returncode, 0)

    def test_an_unready_node_still_reports_its_build(self) -> None:
        """Up-and-unready is a different state from down, and stays different.

        Reporting them as one loses the distinction that decides what to do
        next: a node that is serving the wrong build needs a deploy, and one
        that is unready on the right build needs looking at.
        """
        with FakeNode(ready="quorum unavailable\n") as node:
            result = run_status("127.0.0.1", node.port)
        self.assertIn("quorum unavailable", result.stdout)
        self.assertIn("0.3.0+6e225342", result.stdout)
        self.assertNotEqual(result.returncode, 0)

    def test_ready_without_build_metrics_fails_verification(self) -> None:
        """Readiness without a build identity is not a verified deployment."""
        with FakeNode(metrics="") as node:
            result = run_status("127.0.0.1", node.port)
        self.assertIn("unknown", result.stdout)
        self.assertIn("not ready, not answering, or not on the same build", result.stdout)
        self.assertNotEqual(result.returncode, 0)

    def test_a_split_fleet_is_called_out_rather_than_left_to_be_noticed(self) -> None:
        """Two builds serving at once is a partial deploy.

        It is invisible in a per-node list unless somebody compares the rows,
        which is exactly the work a probe should do rather than present. Both
        nodes are ready and both look healthy on their own line: the fleet is
        the thing that is wrong, so the fleet is what has to be reported on.
        """
        older = METRICS.replace("6e225342", "0aaaaaaa")
        with FakeNode() as new, FakeNode(metrics=older) as old:
            result = run_status(f"127.0.0.1:{new.port} 127.0.0.1:{old.port}", 1)
        self.assertIn("0.3.0+6e225342", result.stdout)
        self.assertIn("0.3.0+0aaaaaaa", result.stdout)
        self.assertIn("2 different builds are serving", result.stdout)
        self.assertIn("mid-deploy or a node was missed", result.stdout)
        self.assertNotEqual(result.returncode, 0)

    def test_one_build_across_the_fleet_raises_nothing(self) -> None:
        """The warning has to stay quiet when the fleet agrees.

        A split-fleet check that fires on a healthy fleet is one people learn
        to ignore, which costs more than not having it.
        """
        with FakeNode() as first, FakeNode() as second:
            result = run_status(f"127.0.0.1:{first.port} 127.0.0.1:{second.port}", 1)
        self.assertNotIn("different builds", result.stdout)
        self.assertNotIn("not ready, not answering", result.stdout)
        self.assertEqual(result.returncode, 0)

    def test_the_probe_changes_nothing(self) -> None:
        """Read-only by construction, so it is safe mid-deploy.

        Asserted against the script text because the guarantee is structural:
        the probe issues GETs and holds no argument that could mutate a node.
        """
        body = SHIP.read_text(encoding="utf-8")
        probe = body[body.index('if [ "$DO_STATUS" -eq 1 ]; then') :]
        probe = probe[: probe.index('say "done"')]

        # Named flags rather than loose substrings: `-d` as a bare fragment
        # also matches `tr -d`, and a check that cries wolf on a correct script
        # gets deleted rather than heeded.
        for forbidden in ("curl -X", "curl --request", "curl -d", "curl --data",
                          "ansible-playbook", "ssh ", "systemctl"):
            self.assertNotIn(
                forbidden,
                probe,
                f"the status probe must stay read-only; found {forbidden!r}",
            )
        # Comments discuss curl; only the invocations are the claim.
        for line in probe.splitlines():
            stripped = line.strip()
            if stripped.startswith("#") or "curl " not in stripped:
                continue
            self.assertRegex(
                stripped,
                r"curl -[fs]*sS ",
                "every request the probe makes is a plain silent GET",
            )


if __name__ == "__main__":
    unittest.main()
