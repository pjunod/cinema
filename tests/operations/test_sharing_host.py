"""Host-side sharing lifecycle; all commands are fake and never touch Tailscale."""
import copy
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
LOADER = importlib.machinery.SourceFileLoader("sharing_host_worker", str(ROOT / "scripts/plurx-sharing-host"))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
host = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(host)
INSTALL_LOADER = importlib.machinery.SourceFileLoader("sharing_host_installer", str(ROOT / "scripts/install-sharing-host"))
INSTALL_SPEC = importlib.util.spec_from_loader(INSTALL_LOADER.name, INSTALL_LOADER)
installer = importlib.util.module_from_spec(INSTALL_SPEC)
INSTALL_LOADER.exec_module(installer)
NOW = 100_000


def request(enabled=True):
    return {"schema_version": 1, "generation": "11111111-1111-4111-8111-111111111111",
            "node_id": "owned-node", "enabled": enabled, "checked_at_ms": NOW,
            "listener_profile": "docker_bridge", "listener_address": "0.0.0.0:32444",
            "container_egress": "172.30.44.2"}


def container():
    return {"Config": {"User": "1000:1000", "Labels": {"com.docker.compose.project": "cinema", "com.docker.compose.service": "plurxd"}}, "HostConfig": {"Privileged": False, "CapAdd": None},
            "State": {"Running": True}, "Mounts": [{"Source": "/srv/cinema", "Destination": "/var/lib/plurx"}],
            "NetworkSettings": {"Ports": {"32444/tcp": [{"HostIp": "127.0.0.1", "HostPort": "32444"}]},
                                "Networks": {"sharing": {"IPAddress": "172.30.44.2", "IPAMConfig": {"IPv4Address": "172.30.44.2"}, "GwPriority": 1}, "media": {"IPAddress": "172.19.0.2", "GwPriority": 0}}}}


class HostLifecycle(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.config = {"kind": "docker", "docker": "/usr/bin/docker", "iptables": "/usr/sbin/iptables", "private_egress_guard": True, "container": "owned", "project": "cinema",
                       "data_dir": "/srv/cinema", "container_data_dir": "/var/lib/plurx", "source_uid": 1000}
        self.firewall = [["-A", "DOCKER-USER", "-j", "RETURN"]]
        self.forward = [["-A", "FORWARD", "-j", "DOCKER-USER"]]
        self.serve = {"TCP": {"443": {"HTTPS": True}}, "Web": {"cinema.tail123.ts.net:443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:9999"}}}}}
        self.calls = []
        self.container = container()
        self.worker = host.Worker(self.config, self.directory, self.execute, lambda: NOW)
        self.patcher = patch.object(host, "trusted_program", return_value="/usr/bin/tailscale")
        self.patcher.start()
        self.addCleanup(self.patcher.stop)
        tunnel = patch.object(host, "host_tunnel_ready", return_value=True)
        tunnel.start()
        self.addCleanup(tunnel.stop)

    def execute(self, argv):
        self.calls.append(argv)
        if argv[0] == "/usr/sbin/iptables":
            if "-S" in argv:
                if argv[-1] == "FORWARD":
                    return ("-P FORWARD ACCEPT\n" + "\n".join(" ".join(row) for row in self.forward) + "\n").encode()
                return ("-N DOCKER-USER\n" + "\n".join(" ".join(row) for row in self.firewall) + "\n").encode()
            if "-I" in argv:
                self.firewall.insert(0, ["-A", "DOCKER-USER", *argv[6:]])
            return b""
        if "inspect" in argv:
            return json.dumps([self.container]).encode()
        if "status" in argv and "serve" not in argv:
            return json.dumps({"BackendState": "Running", "Self": {"DNSName": "cinema.tail123.ts.net.", "TailscaleIPs": ["100.80.1.2"]}}).encode()
        if "status" in argv:
            return json.dumps(self.serve).encode()
        if "off" in argv:
            self.serve["TCP"].pop("32443")
        else:
            self.serve["TCP"]["32443"] = {"TCPForward": host.TARGET}
        return b""

    def test_enable_is_idempotent_disable_removes_only_owned_rule(self):
        original = copy.deepcopy(self.serve)
        self.assertEqual(self.worker.reconcile(request())[0], "ready")
        self.assertEqual(self.worker.reconcile(request())[0], "ready")
        self.assertEqual(sum("--bg" in call for call in self.calls), 1)
        self.assertEqual(self.worker.reconcile(request(False)), ("disabled", None))
        self.assertEqual(self.serve, original)

    def test_disable_preserves_a_preexisting_manual_forward(self):
        self.serve["TCP"]["32443"] = {"TCPForward": host.TARGET}
        self.worker.reconcile(request())
        self.worker.reconcile(request(False))
        self.assertIn("32443", self.serve["TCP"])
        self.assertFalse(any("off" in call for call in self.calls))

    def test_conflict_and_public_forward_are_preserved(self):
        for config in ({"TCP": {"32443": {"TCPForward": "127.0.0.1:8080"}}},
                       {"TCP": {"32443": {"TCPForward": host.TARGET}}, "AllowFunnel": {"cinema.tail123.ts.net:32443": True}}):
            self.serve = config
            original = copy.deepcopy(config)
            with self.assertRaises(host.SetupError) as raised:
                self.worker.reconcile(request())
            self.assertEqual(raised.exception.state, "port_conflict")
            self.assertEqual(self.serve, original)
            self.assertFalse(any("--bg" in call for call in self.calls))

    def test_drift_after_enable_is_not_removed_on_disable(self):
        self.worker.reconcile(request())
        self.serve["TCP"]["32443"] = {"TCPForward": "127.0.0.1:8080"}
        with self.assertRaises(host.SetupError):
            self.worker.reconcile(request(False))
        self.assertEqual(self.serve["TCP"]["32443"]["TCPForward"], "127.0.0.1:8080")

    def test_wildcard_publication_and_wrong_container_never_configure_tailscale(self):
        self.container["NetworkSettings"]["Ports"]["32444/tcp"][0]["HostIp"] = "0.0.0.0"
        with self.assertRaises(host.SetupError):
            self.worker.reconcile(request())
        self.assertEqual(len(self.calls), 1)
        self.calls.clear()
        self.container = container()
        self.container["Config"]["Labels"]["com.docker.compose.project"] = "unrelated"
        with self.assertRaises(host.SetupError):
            self.worker.reconcile(request())
        self.assertEqual(len(self.calls), 1)

    def test_stale_or_path_bearing_requests_never_run_commands(self):
        for change in ({"checked_at_ms": NOW - 15001}, {"enabled": "true"}, {"command": "anything"}):
            value = request()
            value.update(change)
            with self.assertRaises(host.SetupError):
                self.worker.reconcile(value)
            self.assertEqual(self.calls, [])

    def test_report_is_bound_to_the_request_and_data_directory(self):
        data = self.directory / "data"
        data.mkdir()
        shared = data / "sharing-host"
        shared.mkdir(mode=0o700)
        path = shared / "request.json"
        path.write_text(json.dumps(request(False)))
        path.chmod(0o600)
        metadata = data.stat()
        self.worker.registration.update(data_dir=str(data), source_uid=os.getuid(), data_identity=[metadata.st_dev, metadata.st_ino])
        self.worker.tick()
        report = json.loads((shared / "report.json").read_text())
        self.assertEqual(report["generation"], request()["generation"])
        self.assertEqual(report["state"], "disabled")
        self.assertEqual(self.calls, [])
        self.worker.registration["data_identity"][1] += 1
        with self.assertRaises(host.SetupError):
            self.worker.tick()

    def test_symlink_request_is_refused(self):
        target = self.directory / "outside"
        target.write_text(json.dumps(request()))
        (self.directory / "request.json").symlink_to(target)
        directory = os.open(self.directory, os.O_DIRECTORY)
        try:
            with self.assertRaises(OSError):
                host.read_json_at(directory, "request.json", [os.getuid()])
        finally:
            os.close(directory)

    def test_enable_does_not_mutate_after_a_disable_during_probes(self):
        def changed():
            raise host.SetupError("unavailable")
        with self.assertRaises(host.SetupError):
            self.worker.reconcile(request(), changed)
        self.assertNotIn("32443", self.serve["TCP"])
        self.assertFalse((self.directory / "serve-owned.json").exists())

    def test_firewall_guard_matches_only_the_verified_container_and_tailnet_range(self):
        self.worker.reconcile(request())
        inserted = [argv for argv in self.calls if argv[0] == "/usr/sbin/iptables" and "-I" in argv]
        self.assertEqual(inserted, [["/usr/sbin/iptables", "-w", "5", "-I", "DOCKER-USER", "1", "-s", "172.30.44.2/32", "-d", "100.64.0.0/10", "!", "-o", "tailscale0", "-j", "REJECT"]])

    def test_earlier_accept_cannot_bypass_the_guard(self):
        self.worker.reconcile(request())
        bypass = ["-A", "DOCKER-USER", "-j", "ACCEPT"]
        self.firewall.insert(0, bypass)
        self.worker.reconcile(request())
        self.assertIn(bypass, self.firewall)
        self.assertEqual(self.firewall[0][-1], "REJECT")

    def test_shadowed_docker_user_chain_cannot_authorize_egress(self):
        self.forward.insert(0, ["-A", "FORWARD", "-j", "ACCEPT"])
        with self.assertRaises(host.SetupError):
            self.worker.reconcile(request())
        self.assertFalse(self.worker.egress_guard)
        self.assertFalse(any("--bg" in argv for argv in self.calls))

    def test_kernel_default_reject_output_is_recognized(self):
        rule = ["DOCKER-USER", "-s", "172.30.44.2/32", "-d", "100.64.0.0/10", "!", "-o", "tailscale0", "-j", "REJECT"]
        output = ("-A " + " ".join(rule) + " --reject-with icmp-port-unreachable\n").encode()
        self.assertTrue(host.guard_is_first(output, rule))

    def test_root_or_capability_privileged_containers_are_refused(self):
        for section, key, value in (("Config", "User", "0:0"), ("HostConfig", "Privileged", True), ("HostConfig", "CapAdd", ["CHOWN"])):
            candidate = container()
            candidate[section][key] = value
            with self.assertRaises(host.SetupError):
                host.validate_container(candidate, self.config, request())

    def test_absent_tunnel_never_enables_forwarding(self):
        with patch.object(host, "host_tunnel_ready", return_value=False):
            with self.assertRaises(host.SetupError) as raised:
                self.worker.reconcile(request())
        self.assertEqual(raised.exception.state, "deployment_required")
        self.assertFalse(any("--bg" in argv for argv in self.calls))


class HostInstallation(unittest.TestCase):
    def test_node_config_preserves_storage_cluster_and_secrets(self):
        source = '# keep this comment\n[storage]\ndata_dir="/var/lib/plurx"\n[cluster]\nnode_id="owned"\n[sharing]\nbind="127.0.0.1:32444"\n[sharing.egress]\nmode="interface"\nname="tailscale0"\n[server]\nname="My Cinema"\n'
        result = installer.node_config(source, "172.30.44.2")
        parsed = installer.tomllib.loads(result)
        self.assertEqual(parsed["storage"], {"data_dir": "/var/lib/plurx"})
        self.assertEqual(parsed["cluster"], {"node_id": "owned"})
        self.assertEqual(parsed["server"], {"name": "My Cinema"})
        self.assertEqual(parsed["sharing"]["egress"]["address"], "172.30.44.2")
        self.assertTrue(parsed["sharing"]["require_host_egress_guard"])
        self.assertIn("# keep this comment", result)

    def test_explicit_config_argument_is_refused_before_any_command(self):
        value = {"Config": {"Entrypoint": ["plurxd"], "Cmd": ["--config", "/custom.toml", "run"]}}
        with patch.object(installer.worker, "run") as execute:
            with self.assertRaises(ValueError):
                installer.original_config(value, ["docker"], {})
            execute.assert_not_called()

    def test_full_compose_identity_is_required_before_recreation(self):
        installer.validate_service_hash("a" * 64, "plurxd " + "a" * 64 + "\n")
        for value in ("plurxd " + "b" * 64, "unrelated " + "a" * 64, ""):
            with self.assertRaises(ValueError):
                installer.validate_service_hash("a" * 64, value)

    def test_environment_update_preserves_existing_values_and_paths(self):
        source = "# keep\nMEDIA=/srv/media\nPASSWORD='example$literal'\nCOMPOSE_FILE=old.yml\n"
        result = installer.environment_with_files(source, [Path("/srv/cinema/base.yml"), Path("/srv/cinema/sharing.json")])
        self.assertIn("PASSWORD='example$literal'", result)
        self.assertIn("MEDIA=/srv/media", result)
        self.assertEqual(result.count("COMPOSE_FILE="), 1)
        with self.assertRaises(ValueError):
            installer.environment_with_files(source, [Path("/srv/unsafe:path.yml")])

    def test_bridge_selection_does_not_overlap_existing_networks(self):
        subnet, address = installer.select_subnet([{"IPAM": {"Config": [{"Subnet": "172.30.44.0/24"}]}}])
        self.assertEqual((subnet, address), ("172.30.45.0/24", "172.30.45.2"))


if __name__ == "__main__":
    unittest.main()
