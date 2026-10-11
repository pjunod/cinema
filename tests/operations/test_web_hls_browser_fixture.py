"""Browser evidence waits for the real asynchronous log's fixture receipt."""
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import tempfile
import threading
import unittest
import urllib.request


ROOT = Path(__file__).resolve().parents[2]
LOADER = importlib.machinery.SourceFileLoader(
    "web_hls_fixture", str(ROOT / "scripts/web-hls-startup-browser-check")
)
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
FIXTURE = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(FIXTURE)


class WebHlsBrowserFixture(unittest.TestCase):
    def test_delayed_seek_log_is_observed_before_evidence_snapshot(self):
        with tempfile.TemporaryDirectory() as raw:
            server = FIXTURE.FixtureServer(Path(raw))
            serving = threading.Thread(target=server.serve_forever)
            serving.start()
            release = threading.Event()
            errors = []

            def post(event):
                request = urllib.request.Request(
                    f"http://127.0.0.1:{server.server_port}/api/v1/client-log",
                    data=json.dumps({"event": event}).encode(),
                    headers={"content-type": "application/json"},
                )
                with urllib.request.urlopen(request, timeout=2) as response:
                    self.assertEqual(response.status, 204)

            def delayed_post():
                try:
                    if not release.wait(timeout=2):
                        raise AssertionError("test never released the delayed log")
                    post("seek_local")
                except Exception as error:
                    errors.append(error)

            sender = threading.Thread(target=delayed_post)
            sender.start()
            try:
                post("seek_route")
                self.assertFalse(server.wait_for_log("seek_local", timeout=0.02))
                release.set()
                self.assertTrue(server.wait_for_log("seek_local", timeout=2))
                with server.lock:
                    self.assertEqual(
                        [row["event"] for row in server.client_logs],
                        ["seek_route", "seek_local"],
                    )
                    self.assertEqual(server.session_creates, 0)
                sender.join(timeout=2)
                self.assertFalse(sender.is_alive())
                self.assertEqual(errors, [])
            finally:
                release.set()
                sender.join(timeout=2)
                server.shutdown()
                server.server_close()
                serving.join(timeout=2)


if __name__ == "__main__":
    unittest.main()
