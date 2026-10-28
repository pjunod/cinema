"""The UI fixture owns every listener it asks the real daemon to open."""
from pathlib import Path
import runpy
import socket
import tempfile
import tomllib
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]


class FixtureListenerIsolationTests(unittest.TestCase):
    def test_fixture_writes_four_distinct_private_listener_ports(self):
        harness = runpy.run_path(str(ROOT / "scripts/ui-baseline"), run_name="listener_fixture_test")
        with tempfile.TemporaryDirectory() as output, socket.socket() as occupied:
            occupied.bind(("127.0.0.1", 0))
            preferred = occupied.getsockname()[1]
            args = harness["parse_args"](["--self-host", "--out", output, "--port", str(preferred)])
            fixture = harness["SelfHost"](args)
            # Stop at the first boot: inspect exactly the config the daemon gets,
            # without invoking ffmpeg, compiling, or mutating any database.
            with patch.dict(fixture.start.__globals__, {"generate_media": lambda *_: None}), \
                    patch.object(fixture, "spend_activation_recovery_boot", side_effect=InterruptedError):
                with self.assertRaises(InterruptedError):
                    fixture.start()
            config = tomllib.loads(fixture.config.read_text())
            ports = [int(config["server"]["bind"].rsplit(":", 1)[1]),
                     int(config["cluster"]["raft_bind"].rsplit(":", 1)[1]),
                     int(config["cluster"]["api_bind"].rsplit(":", 1)[1]),
                     config["server"]["jellyfin_port"]]
            self.assertEqual(len(set(ports)), 4)
            self.assertNotIn(preferred, ports)
            self.assertTrue(all(port > 0 for port in ports))
            self.assertEqual(ports[-1], fixture.jellyfin_port)


if __name__ == "__main__":
    unittest.main()
