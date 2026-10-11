"""Same-review mount consumer proof, no real Docker or host mutations."""
import copy
import os
import unittest
from unittest.mock import patch

if __package__:
    from .test_k06_owned_lab import LAB, manifest
else:
    from test_k06_owned_lab import LAB, manifest


class OwnedClockLabMountTests(unittest.TestCase):
    def test_actual_mounts_refuse_unbudgeted_volumes_and_require_exact_tmpfs(self):
        m = manifest()
        m["artifact"]["image_config"]["Volumes"] = {"/var/lib/plurx": {}}
        n = m["nodes"][0]
        n.update(container_id="1" * 64, network_id="2" * 64)
        image = {"Id": n["docker_image_id"], "Architecture": "amd64", "Os": "linux",
                 "RootFS": {"Type": "layers", "Layers": m["artifact"]["rootfs_diff_ids"]},
                 "Config": m["artifact"]["image_config"]}
        item = {"Id": n["container_id"], "Image": n["docker_image_id"],
                "Config": {"User": f"{os.getuid()}:{os.getgid()}", "Labels": {
                    LAB.LABEL: m["owner"], "tv.plurx.k06-source": m["artifact"]["source"],
                    "tv.plurx.k06-config": "sha256:" + m["artifact"]["config_digest"]}},
                "HostConfig": {"NanoCpus": 2_000_000_000, "Memory": 2 * 1024**3,
                    "MemorySwap": 2 * 1024**3, "PidsLimit": 256, "ReadonlyRootfs": True,
                    "RestartPolicy": {"Name": "no"}, "CapDrop": ["ALL"], "Privileged": False,
                    "Devices": [], "NetworkMode": n["network_id"], "SecurityOpt": ["no-new-privileges"],
                    "PortBindings": {str(p) + "/tcp": [{"HostIp": n["ip"], "HostPort": str(p)}]
                                     for p in (55420, 55421, 55422)},
                    "LogConfig": {"Type": "local", "Config": {"max-size": "10m", "max-file": "3"}},
                    "Tmpfs": dict(LAB.TMPFS)},
                "Mounts": [{"Type": "bind", "Source": n["root"], "Destination": "/data", "RW": True}]}
        LAB.validate_manifest(m)
        argv = LAB.daemon_args(m, n, os.getuid(), os.getgid())
        self.assertIn("--tmpfs=/var/lib/plurx:rw,nosuid,nodev,noexec,size=16m", argv)
        with patch.object(LAB, "inspect", return_value=image):
            LAB.validate_container(item, m, n)
            complete = copy.deepcopy(item)
            complete["Mounts"] += [{"Type": "tmpfs", "Source": "", "Destination": path, "RW": True}
                                    for path in LAB.TMPFS]
            LAB.validate_container(complete, m, n)
            for extra in ({"Type": "volume", "Source": "anonymous", "Destination": "/var/lib/plurx", "RW": True},
                          {"Type": "bind", "Source": n["root"], "Destination": "/other", "RW": True},
                          {"Type": "tmpfs", "Source": "", "Destination": "/other", "RW": True}):
                wrong = copy.deepcopy(item)
                wrong["Mounts"].append(extra)
                with self.assertRaises(ValueError):
                    LAB.validate_container(wrong, m, n)
            for tmpfs in ({}, dict(LAB.TMPFS, **{"/var/lib/plurx": "rw,size=16m"}),
                          dict(LAB.TMPFS, **{"/other": "rw,size=1m"})):
                wrong = copy.deepcopy(item)
                wrong["HostConfig"]["Tmpfs"] = tmpfs
                with self.assertRaises(ValueError):
                    LAB.validate_container(wrong, m, n)
        wrong = copy.deepcopy(m)
        wrong["artifact"]["image_config"]["Volumes"]["/unexpected"] = {}
        with self.assertRaises(ValueError):
            LAB.validate_manifest(wrong)
        with self.assertRaises(ValueError):
            LAB.daemon_args(wrong, wrong["nodes"][0], os.getuid(), os.getgid())
