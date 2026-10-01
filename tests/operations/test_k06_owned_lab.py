"""Source-only safety contracts; never opens SSH, Docker or an HTTP endpoint."""
import copy
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("k06_owned_lab", ROOT / "scripts/k06-owned-lab.py")
LAB = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LAB)


def manifest():
    owner = "a" * 64
    artifact = {"binary_sha256": "b" * 64, "image": "sha256:" + "c" * 64,
                "source": "d" * 40, "tree": "e" * 40, "archive_sha256": "f" * 64,
                "build": "d" * 40, "compiler": "rustc 1.97.1 (8bab26f4f 2026-07-14)",
                "command": "cargo build --offline --locked --release -p plurxd --bin plurxd"}
    nodes = []
    for host, ip in LAB.HOSTS:
        stem = "plurx-k06-measure." + owner + "-" + host
        nodes.append({"host": host, "ip": ip, "root": "/var/tmp/" + stem,
                      "name": stem, "network_name": stem + "-net"})
    return {"schema": 1, "owner": owner, "artifact": artifact, "nodes": nodes, "phase": "planned"}


class OwnedClockLabTests(unittest.TestCase):
    def test_owned_daemon_plan_refuses_cross_campaign_paths_and_cap_escape(self):
        m = manifest()
        LAB.validate_manifest(m)
        for unsafe in ("/", "/var/tmp", "/var/tmp/../home", "/var/tmp/plurx-k06-measure." + "0" * 64 + "-nynuc"):
            bad = copy.deepcopy(m)
            bad["nodes"][0]["root"] = unsafe
            with self.assertRaises(ValueError):
                LAB.validate_manifest(bad)
        bad = copy.deepcopy(m)
        bad["nodes"][0]["ip"] = "192.168.4.1"
        with self.assertRaises(ValueError):
            LAB.validate_manifest(bad)
        n = m["nodes"][0]
        n["network_id"] = "1" * 64
        argv = LAB.daemon_args(m, n, 1000, 1000)
        for cap in ("--cpus=2", "--memory=2g", "--memory-swap=2g", "--pids-limit=256",
                    "--read-only", "--cap-drop=ALL", "--security-opt=no-new-privileges", "--restart=no",
                    "--log-opt=max-size=10m", "--log-opt=max-file=3", "PLURX_MDNS_ADVERTISE=false"):
            self.assertIn(cap, argv)
        self.assertEqual(argv[argv.index("--network") + 1], n["network_id"])
        self.assertEqual([argv[i + 1] for i, arg in enumerate(argv) if arg == "--publish"],
                         [f"192.168.5.236:{port}:{port}/tcp" for port in (55420, 55421, 55422)])
        self.assertNotIn("--privileged", argv)
        self.assertNotIn("--network=host", argv)
        self.assertNotIn("--device", argv)
        self.assertNotIn("docker.sock", " ".join(argv))
        self.assertEqual(argv[-8:], [m["artifact"]["image"], "-k", "10s", "5400s", "/usr/local/bin/plurxd",
                                    "--config", "/data/plurx.toml", "run"])

    def test_cleanup_authority_refuses_symlink_owner_and_unapproved_launch(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            root.chmod(0o700)
            m = manifest()
            marker = root / ".owner"
            marker.write_text(m["owner"] + "\n")
            marker.chmod(0o600)
            path = root / ".active-cleanup.json"
            LAB.private_write(path, m)
            self.assertEqual(LAB.load_manifest(path)["owner"], m["owner"])
            with patch("sys.argv", ["k06-owned-lab.py", "launch", str(path)]), patch.object(LAB, "remote") as remote:
                with self.assertRaises(ValueError):
                    LAB.main()
                remote.assert_not_called()
            backing = root / "separate-owner"
            marker.rename(backing)
            marker.symlink_to(backing)
            with self.assertRaises((ValueError, OSError)):
                LAB.load_manifest(path)


if __name__ == "__main__":
    unittest.main()
