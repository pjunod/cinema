"""Host-side checks for the bounded native guest-stage protocol."""
import unittest
from types import SimpleNamespace

from validation.knowledge.study_driver import guest_stage_store as store


def transport_spec(**overrides):
    binding = {
        "filesystem_uuid": "fs-uuid",
        "boot_id": "boot-id",
        "identities": {},
        "ancestry": [],
        "mounts": [],
        "alias": "/owned/stages",
        "physical_work": "/work",
        "control": "/control",
    }
    values = {
        "executable": "/usr/bin/ssh",
        "executable_sha256": "a" * 64,
        "arguments": ("-F", "config", "{session}"),
        "environment": (("PATH", "/usr/bin"),),
        "expected_binding": binding,
        "source_hashes": {"helper": "b" * 64},
    }
    values.update(overrides)
    return store.GuestTransportSpec(**values)


class GuestTransportContractTests(unittest.TestCase):
    def test_transport_spec_accepts_only_closed_pinned_launch_contract(self):
        spec = transport_spec()
        self.assertEqual(spec.arguments.count("{session}"), 1)
        self.assertEqual(dict(spec.environment), {"PATH": "/usr/bin"})

        invalid = (
            {"arguments": ("{session}", "{session}")},
            {"arguments": ("{session}\n--unsafe",)},
            {"environment": (("PATH", "/usr/bin"), ("PYTHONPATH", "/tmp"))},
            {"expected_binding": {"alias": "/owned/stages"}},
            {"source_hashes": {"helper": "not-a-hash"}},
        )
        for change in invalid:
            with self.subTest(change=change), self.assertRaises(store.NativeGuestError):
                transport_spec(**change)

    def test_root_response_must_bind_every_role_to_original_guest_device(self):
        backend = object.__new__(store.NativeGuestStageStore)
        backend.session = "c" * 32
        roots = {
            role: {"capability": chr(97 + index) * 32, "device": 17, "inode": 100 + index}
            for index, role in enumerate(("candidate", "grader", "runtime"))
        }
        response = {"stage": "d" * 32, "session": backend.session,
                    "identity": [17, 99], "roots": roots}

        accepted = backend._response_roots(response, stage="d" * 32)
        self.assertEqual(set(accepted), {"candidate", "grader", "runtime"})
        self.assertEqual(accepted["runtime"].device, 17)

        wrong_device = {**response, "roots": {**roots, "runtime": {**roots["runtime"], "device": 18}}}
        incomplete = {**response, "roots": {key: value for key, value in roots.items() if key != "grader"}}
        wrong_session = {**response, "session": "e" * 32}
        for malformed in (wrong_device, incomplete, wrong_session):
            with self.subTest(response=malformed), self.assertRaises(store.NativeGuestError):
                backend._response_roots(malformed, stage="d" * 32)


class GuestOutsideChallengeBindingTests(unittest.TestCase):
    def binding(self, replies):
        class FakeStore:
            def __init__(self):
                self.stage = SimpleNamespace(
                    host_root=__import__("pathlib").Path("/owned/stages/") / ("f" * 32),
                    stage="f" * 32,
                    identity=(17, 99),
                )
                self.replies = iter(replies)
                self.calls = []

            def _ref(self, stage):
                if stage is not self.stage:
                    raise store.NativeGuestError("OriginalNativeStageRequired")

            def _exchange(self, op, body):
                self.calls.append((op, body))
                return next(self.replies)

        fake = FakeStore()
        return store.NativeGuestBinding(fake, fake.stage), fake

    def valid_receipt(self, name, content):
        return {"scope": "outside-all-role-mounts", "stage": "f" * 32, "name": name,
                "device": 17, "inode": 501,
                "sha256": store._sha(content), "bytes": 32, "mode": 0o600, "present": True}

    def test_begin_checks_guest_receipt_and_end_requires_exact_removal(self):
        name = "outside-" + "1" * 32
        challenge = b"x" * 32
        issued = self.valid_receipt(name, challenge)
        binding, fake = self.binding([issued, dict(issued, present=False, removed=True)])

        receipt = binding.outside_begin(binding.stage.host_root / ".isolation" / name, challenge)
        removed = binding.outside_end(receipt)

        self.assertEqual(removed["removed"], True)
        self.assertEqual([call[0] for call in fake.calls], ["outside_sentinel", "outside_sentinel"])
        self.assertEqual(fake.calls[0][1]["content_base64"], "eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHg=")
        self.assertEqual(fake.calls[1][1]["expected"], issued)

    def test_begin_and_end_reject_forged_or_incomplete_receipts(self):
        name = "outside-" + "2" * 32
        challenge = b"y" * 32
        issued = self.valid_receipt(name, challenge)
        forged = dict(issued, inode=0)
        binding, _ = self.binding([forged])
        with self.assertRaises(store.NativeGuestError):
            binding.outside_begin(binding.stage.host_root / ".isolation" / name, challenge)

        binding, _ = self.binding([issued, dict(issued, present=False)])
        receipt = binding.outside_begin(binding.stage.host_root / ".isolation" / name, challenge)
        with self.assertRaises(store.NativeGuestError):
            binding.outside_end(receipt)

    def test_mounted_role_ids_are_guest_observations_for_fixed_targets(self):
        class FakeStore:
            def __init__(self):
                self.paths = []

            def _ref(self, _stage):
                return None

            def observe(self, path, *, metadata_only):
                self.paths.append((path, metadata_only))
                return {"entries": [{"stat": [23, len(self.paths), 0o700, 0, 1, 1]}]}

        fake = FakeStore()
        stage = SimpleNamespace(stage="f" * 32)
        binding = store.NativeGuestBinding(fake, stage)
        config = SimpleNamespace(candidate_root="/owned/candidate", runtime_root="/owned/runtime",
                                 grader_root="/owned/grader")

        observed = binding.mounted_identities(config)

        self.assertEqual(observed, {"/work": [23, 1], "/work/.runtime": [23, 3], "/grader": [23, 5]})
        self.assertEqual(fake.paths, [
            ("/owned/candidate", True), ("/owned/candidate", True),
            ("/owned/runtime", True), ("/owned/runtime", True),
            ("/owned/grader", True), ("/owned/grader", True),
        ])


if __name__ == "__main__":
    unittest.main()
