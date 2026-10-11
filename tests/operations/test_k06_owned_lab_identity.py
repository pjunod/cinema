"""Synthetic mixed-store consumers; no real Docker, SSH or host mutations."""
import copy
import unittest
from unittest.mock import patch

if __package__:
    from .test_k06_owned_lab import LAB, manifest
else:
    from test_k06_owned_lab import LAB, manifest


class OwnedClockLabIdentityTests(unittest.TestCase):
    def test_mixed_store_preflight_binds_exact_config_layers_and_node_identity(self):
        m = manifest()
        artifact = m["artifact"]
        identities = LAB.image_ids(artifact)
        for actual in identities:
            node = m["nodes"][0]
            node.pop("docker_image_id", None)
            image = {"Id": actual, "Architecture": "amd64", "Os": "linux",
                     "RootFS": {"Type": "layers", "Layers": artifact["rootfs_diff_ids"]},
                     "Config": artifact["image_config"]}
            def lookup(kind, identity):
                self.assertEqual(kind, "image")
                if identity != actual:
                    raise ValueError("immutable lookup missing")
                return image
            with patch.object(LAB, "inspect", side_effect=lookup), \
                 patch.object(LAB, "command", side_effect=lambda argv: node["host"] if argv == ["hostname"] else ""), \
                 patch.object(LAB, "capacity", return_value={}), \
                 patch.object(LAB, "production", return_value={}), \
                 patch.object(LAB, "discipline", return_value={}):
                result = LAB.worker({"manifest": m, "node": node, "action": "preflight", "payload": None})
                self.assertEqual(result["docker_image_id"], actual)
                node.update(result)  # controller persists this before claim/create
                node["network_id"] = "1" * 64
                argv = LAB.daemon_args(m, node, 1000, 1000)
                self.assertEqual(argv[-8], actual)
                self.assertIn("tv.plurx.k06-config=sha256:" + artifact["config_digest"], argv)
                self.assertEqual(LAB.resolve_image(artifact, actual), actual)
                for key, bad in (("Config", {}), ("RootFS", {"Type": "layers", "Layers": []}),
                                 ("Id", "sha256:" + "0" * 64)):
                    corrupt = copy.deepcopy(image)
                    corrupt[key] = bad
                    with patch.object(LAB, "inspect", return_value=corrupt), self.assertRaises(ValueError):
                        LAB.resolve_image(artifact, actual)
                with patch.object(LAB, "inspect", side_effect=ValueError("absent")), self.assertRaises(ValueError):
                    LAB.resolve_image(artifact)
                def ambiguous(kind, identity):
                    return dict(image, Id=identity)
                with patch.object(LAB, "inspect", side_effect=ambiguous), self.assertRaises(ValueError):
                    LAB.resolve_image(artifact)
                node["docker_image_id"] = "sha256:" + "0" * 64
                with self.assertRaises(ValueError):
                    LAB.daemon_args(m, node, 1000, 1000)
        missing = manifest()
        del missing["artifact"]["image_config"]
        with self.assertRaises(ValueError):
            LAB.validate_manifest(missing)
