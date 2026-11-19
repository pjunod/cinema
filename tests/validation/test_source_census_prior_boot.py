"""The host source census accepts a witness from an earlier boot of this
machine only when this machine's own journal lists that boot."""
import importlib.machinery
import importlib.util
import json
import subprocess
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[2] / "scripts" / "source-custody-source-census"
OLD = "b21b0fcb-408b-4865-9b20-e09c203acb89"
CURRENT = "c5835d0e-7632-4d87-89ae-c1ea9674c48a"


def census():
    loader = importlib.machinery.SourceFileLoader("source_census_helper", str(SCRIPT))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


def journal(*boots):
    listed = [{"index": index - len(boots) + 1, "boot_id": boot.replace("-", ""),
               "first_entry": 1, "last_entry": 2} for index, boot in enumerate(boots)]
    return subprocess.CompletedProcess([], 0, stdout=json.dumps(listed), stderr="")


class EndedBootTest(unittest.TestCase):
    def setUp(self):
        self.helper = census()

    def ended(self, witness, listed):
        with mock.patch.object(self.helper.subprocess, "run", return_value=listed):
            return self.helper.ended_boot(witness, CURRENT)

    def refuses(self, witness, listed, reason):
        with self.assertRaises(self.helper.Refused) as refusal:
            self.ended(witness, listed)
        self.assertIn(reason, str(refusal.exception))

    def test_an_earlier_listed_boot_has_ended(self):
        self.assertEqual(self.ended(f"linux:{OLD}", journal(OLD, CURRENT)),
                         {"kernel_instance": f"linux:{OLD}"})

    def test_a_boot_this_machine_never_logged_refuses(self):
        # A cloned data directory on another machine, or volatile journald.
        self.refuses(f"linux:{OLD}", journal(CURRENT), "does not list")

    def test_the_current_boot_is_not_an_ended_one(self):
        self.refuses(f"linux:{CURRENT}", journal(OLD, CURRENT), "does not list")

    def test_malformed_identities_and_journals_refuse_without_a_traceback(self):
        self.refuses("macos:" + OLD, journal(OLD, CURRENT), "not a Linux boot")
        self.refuses("linux:not-a-boot", journal(OLD, CURRENT), "not a boot UUID")
        broken = subprocess.CompletedProcess([], 0, stdout="[{}]", stderr="")
        self.refuses(f"linux:{OLD}", broken, "cannot list its boots")
        with mock.patch.object(self.helper.subprocess, "run",
                               side_effect=subprocess.CalledProcessError(1, "journalctl")):
            with self.assertRaises(self.helper.Refused):
                self.helper.ended_boot(f"linux:{OLD}", CURRENT)


if __name__ == "__main__":
    unittest.main()
