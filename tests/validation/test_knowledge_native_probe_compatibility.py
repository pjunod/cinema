"""Synthetic native-probe comparison contracts; no VM or study inputs."""
import hashlib
import unittest

from validation.knowledge import evaluation_isolation as isolation


class NativeProbeCompatibilityTests(unittest.TestCase):
    stage = "a" * 32

    @classmethod
    def observations(cls, *, name, nonce, inode, stage=None, mounts=None):
        digest = hashlib.sha256(nonce).hexdigest()
        return {
            "security": {"read_only": True},
            "observed_probes": {
                "native_mounts": mounts if mounts is not None else {
                    "/work": [41, 101],
                    "/work/.runtime": [41, 102],
                    "/grader": [41, 103],
                },
                "container_user": "1000:1000",
            },
            "sentinel_digest": digest,
            "native_outside_sentinel": {
                "scope": "outside-all-role-mounts",
                "stage": stage or cls.stage,
                "name": name,
                "device": 41,
                "inode": inode,
                "sha256": digest,
                "bytes": 32,
                "mode": 0o600,
                "present": True,
            },
        }

    def test_fresh_authentic_sentinels_project_to_same_stable_probe(self):
        first = self.observations(name="outside-" + "b" * 32, nonce=b"x" * 32, inode=201)
        second = self.observations(name="outside-" + "c" * 32, nonce=b"y" * 32, inode=202)

        self.assertNotEqual(first, second)
        first_stable = isolation._stable_probe_observations(first, native_stage=self.stage)
        second_stable = isolation._stable_probe_observations(second, native_stage=self.stage)
        self.assertEqual(first_stable, second_stable)
        self.assertEqual(first_stable["native_outside_sentinel"], {
            "scope": "outside-all-role-mounts",
            "stage": self.stage,
            "device": 41,
            "bytes": 32,
            "mode": 0o600,
            "present": True,
        })
        self.assertEqual(first_stable["observed_probes"]["native_mounts"],
                         first["observed_probes"]["native_mounts"])

    def test_missing_or_forged_sentinel_provenance_is_rejected(self):
        valid = self.observations(name="outside-" + "d" * 32, nonce=b"z" * 32, inode=203)
        missing = {key: value for key, value in valid.items() if key != "native_outside_sentinel"}
        with self.assertRaises(isolation.IsolationError):
            isolation._stable_probe_observations(missing, native_stage=self.stage)

        for field, value in (
            ("stage", "e" * 32),
            ("name", "replacement-path"),
            ("sha256", "0" * 64),
            ("mode", 0o644),
            ("present", False),
        ):
            forged = {**valid, "native_outside_sentinel": {
                **valid["native_outside_sentinel"], field: value,
            }}
            with self.subTest(field=field), self.assertRaises(isolation.IsolationError):
                isolation._stable_probe_observations(forged, native_stage=self.stage)

        missing_mounts = {**valid, "observed_probes": {"native_mounts": {}}}
        with self.assertRaises(isolation.IsolationError):
            isolation._stable_probe_observations(missing_mounts, native_stage=self.stage)

    def test_changed_native_root_or_mount_ids_do_not_compare_as_compatible(self):
        baseline = self.observations(name="outside-" + "f" * 32, nonce=b"a" * 32, inode=204)
        changed_mounts = {
            "/work": [41, 111],
            "/work/.runtime": [41, 112],
            "/grader": [41, 113],
        }
        changed = self.observations(name="outside-" + "1" * 32, nonce=b"b" * 32,
                                    inode=205, mounts=changed_mounts)

        self.assertNotEqual(
            isolation._stable_probe_observations(baseline, native_stage=self.stage),
            isolation._stable_probe_observations(changed, native_stage=self.stage),
        )

        wrong_root = self.observations(name="outside-" + "2" * 32, nonce=b"c" * 32,
                                       inode=206, mounts={
            "/work": [42, 101],
            "/work/.runtime": [41, 102],
            "/grader": [41, 103],
        })
        with self.assertRaises(isolation.IsolationError):
            isolation._stable_probe_observations(wrong_root, native_stage=self.stage)


if __name__ == "__main__":
    unittest.main()
