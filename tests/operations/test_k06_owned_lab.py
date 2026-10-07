"""Source-only safety contracts; never opens SSH, Docker or an HTTP endpoint."""
import copy
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("k06_owned_lab", ROOT / "scripts/k06-owned-lab.py")
LAB = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LAB)
# The real roster lives outside the repository; the shipped example has the
# same shape with the public mirror's names.
FLEET = LAB.read_fleet(ROOT / "scripts/k06-owned-lab.fleet.example.json")


def manifest():
    owner = "a" * 64
    artifact = {"binary_sha256": "b" * 64, "image": "sha256:" + "c" * 64,
                "source": "d" * 40, "tree": "e" * 40, "archive_sha256": "f" * 64,
                "build": "d" * 40, "compiler": "rustc 1.97.1 (8bab26f4f 2026-07-14)",
                "command": "cargo build --offline --locked --release -p plurxd --bin plurxd"}
    artifact.update(config_digest="9" * 64, rootfs_diff_ids=["sha256:" + "8" * 64],
                    image_config={"Labels": {"org.opencontainers.image.revision": artifact["source"],
                                              "tv.plurx.k06-source-tree": artifact["tree"]}})
    nodes = []
    for host, ip in LAB.fleet_hosts(FLEET):
        stem = "plurx-k06-measure." + owner + "-" + host
        nodes.append({"host": host, "ip": ip, "root": "/var/tmp/" + stem,
                      "name": stem, "network_name": stem + "-net",
                      "docker_image_id": artifact["image"]})
    return {"schema": 1, "owner": owner, "artifact": artifact, "nodes": nodes, "phase": "planned",
            "fleet": copy.deepcopy(FLEET)}


class OwnedClockLabTests(unittest.TestCase):
    def test_owned_daemon_plan_refuses_cross_campaign_paths_and_cap_escape(self):
        m = manifest()
        LAB.validate_manifest(m)
        for unsafe in ("/", "/var/tmp", "/var/tmp/../home", "/var/tmp/plurx-k06-measure." + "0" * 64 + "-media1"):
            bad = copy.deepcopy(m)
            bad["nodes"][0]["root"] = unsafe
            with self.assertRaises(ValueError):
                LAB.validate_manifest(bad)
        bad = copy.deepcopy(m)
        bad["nodes"][0]["ip"] = "10.42.4.1"
        with self.assertRaises(ValueError):
            LAB.validate_manifest(bad)
        bad = copy.deepcopy(m)
        del bad["fleet"]
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
                         [f"{n['ip']}:{port}:{port}/tcp" for port in (55420, 55421, 55422)])
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


class FleetTests(unittest.TestCase):
    """The roster comes from the fleet file; nothing about the lab is in source."""

    def test_the_source_names_no_lab_host_or_address(self):
        text = (ROOT / "scripts/k06-owned-lab.py").read_text()
        for host, ip in LAB.fleet_hosts(FLEET):
            self.assertNotIn(ip, text)
            self.assertNotIn('"' + host + '"', text)

    def test_a_fleet_must_be_four_private_hosts_inside_its_trusted_network(self):
        LAB.validate_fleet(copy.deepcopy(FLEET))
        mutations = {
            "three hosts": lambda f: f["hosts"].pop(),
            "duplicate host": lambda f: f["hosts"][1].update(host=f["hosts"][0]["host"]),
            "duplicate address": lambda f: f["hosts"][1].update(ip=f["hosts"][0]["ip"]),
            "outside network": lambda f: f["hosts"][0].update(ip="172.16.0.9"),
            "public address": lambda f: (f.update(trusted_network="8.8.0.0/16"),
                                         f["hosts"][0].update(ip="8.8.8.8")),
            "unsafe host name": lambda f: f["hosts"][0].update(host="../x"),
            "unsafe user": lambda f: f.update(ssh_user="root@evil"),
            "network type": lambda f: f.update(trusted_network=167772160),
            "load on one host": lambda f: f["load"].update(receiver=f["load"]["sender"]),
            "load outside fleet": lambda f: f["load"].update(sender="elsewhere"),
            "extra field": lambda f: f.update(note="x"),
            "wrong schema": lambda f: f.update(schema="k06-fleet-v0"),
        }
        for label, mutate in mutations.items():
            fleet = copy.deepcopy(FLEET)
            mutate(fleet)
            with self.subTest(label), self.assertRaises(ValueError):
                LAB.validate_fleet(fleet)

    def test_config_ssh_and_load_pair_come_from_the_fleet(self):
        sender, sender_ip, receiver, receiver_ip = LAB.load_pair(FLEET)
        node = dict(zip(("host", "ip"), LAB.fleet_hosts(FLEET)[0]))
        self.assertIn(f'trusted_network = "{FLEET["trusted_network"]}"', LAB.config(node, False, FLEET))
        server = LAB.load_args(receiver, FLEET)
        self.assertEqual(server[server.index("-B") + 1], receiver_ip)
        client = LAB.load_args(sender, FLEET)
        self.assertEqual(client[client.index("-c") + 1], receiver_ip)
        self.assertEqual(client[client.index("-B") + 1], sender_ip)
        others = [host for host, _ in LAB.fleet_hosts(FLEET) if host not in (sender, receiver)]
        with self.assertRaises(ValueError):
            LAB.load_args(others[0], FLEET)
        result = {"connected": [{"local_host": sender_ip, "remote_host": receiver_ip}],
                  "end": {"sum_sent": {"seconds": 60, "bytes": 150_000_000, "bits_per_second": 20_000_000}}}
        LAB.check_load(result, sender, FLEET)
        with self.assertRaises(ValueError):
            LAB.check_load(result, receiver, FLEET)
        m = manifest()
        with patch.object(LAB.subprocess, "run",
                          return_value=subprocess.CompletedProcess([], 0)) as run:
            with patch("tempfile.TemporaryFile", side_effect=lambda: io.BytesIO(b"{}")):
                LAB.remote(Path("/k"), m, m["nodes"][0], "preflight")
        argv = run.call_args.args[0]
        self.assertIn(FLEET["ssh_user"] + "@" + m["nodes"][0]["ip"], argv)

    def test_plan_embeds_the_fleet_and_a_legacy_manifest_takes_it_from_the_flag(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            artifact = root / "artifact.json"
            artifact.write_text(json.dumps(manifest()["artifact"]))
            fleet = root / "lab.fleet.json"
            fleet.write_text(json.dumps(FLEET))
            out = root / "plan"
            argv = ["k06-owned-lab.py", "plan", str(out), "--artifact", str(artifact)]
            with patch("sys.argv", argv), patch.dict("os.environ", {LAB.FLEET_ENV: ""}):
                with self.assertRaises(ValueError):
                    LAB.main()
            self.assertFalse(out.exists())  # refused before creating anything
            with patch("sys.argv", argv + ["--fleet", str(fleet)]), \
                    patch("sys.stdout", io.StringIO()):
                LAB.main()
            planned = json.loads((out / ".active-cleanup.json").read_text())
            self.assertEqual(planned["fleet"], FLEET)
            self.assertEqual([n["host"] for n in planned["nodes"]],
                             [h for h, _ in LAB.fleet_hosts(FLEET)])
            legacy = dict(planned)
            del legacy["fleet"]
            LAB.private_write(out / ".active-cleanup.json", legacy)
            with self.assertRaises(ValueError):
                LAB.load_manifest(out / ".active-cleanup.json")
            self.assertEqual(LAB.load_manifest(out / ".active-cleanup.json", FLEET)["fleet"], FLEET)


if __name__ == "__main__":
    unittest.main()
