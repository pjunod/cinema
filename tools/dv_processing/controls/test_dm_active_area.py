"""Execute source-bound C active-area controls; never build or touch a GPU.

Run with PLURX_DM_ACTIVE_AREA_BUILD pointing at the capped compiler's JSON
receipt. The ordinary preflight does not execute this explicit SDK control.
"""
from pathlib import Path
import hashlib
import json
import os
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[3]
SOURCES = (
    "tools/dv_processing/controls/test_dm_active_area.c",
    "tools/dv_processing/dv_dm.h",
    "tools/dv_processing/dm_closed_form.h",
)


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


class ActiveAreaCControl(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        receipt = Path(os.environ["PLURX_DM_ACTIVE_AREA_BUILD"])
        if not receipt.is_absolute() or receipt.resolve() != receipt:
            raise RuntimeError("use an absolute canonical owned compiler receipt")
        proof = json.loads(receipt.read_text())
        if proof["schema"] != "plurx.dm-active-area-cpu-build.v1":
            raise RuntimeError("wrong active-area control build schema")
        if proof["source_sha256"] != {name: sha(ROOT / name) for name in SOURCES}:
            raise RuntimeError("C control was compiled from different source")
        binary = Path(proof["binary"]["path"])
        if not binary.is_absolute() or binary.resolve() != binary or not binary.is_file():
            raise RuntimeError("use the canonical compiled C control")
        if sha(binary) != proof["binary"]["sha256"]:
            raise RuntimeError("compiled C control changed")
        cls.binary = binary
        cls.digest = proof["binary"]["sha256"]

    def run_case(self, name):
        self.assertEqual(sha(self.binary), self.digest)
        result = subprocess.run([str(self.binary), name], check=True,
                                capture_output=True, text=True, timeout=30)
        self.assertEqual(result.stdout, name + ": PASS\ntest_dm_active_area: PASS\n")
        self.assertEqual(result.stderr, "")
        self.assertEqual(sha(self.binary), self.digest)

    def test_l5_owned_offsets_and_rectangles(self):
        self.run_case("test_l5_owned_offsets_and_rectangles")

    def test_l5_absent_and_zero_preserve_full_frame(self):
        self.run_case("test_l5_absent_and_zero_preserve_full_frame")

    def test_l5_crop_domain_excludes_adversarial_bars(self):
        self.run_case("test_l5_crop_domain_excludes_adversarial_bars")

    def test_l5_cm29_owned_offsets_and_rectangles(self):
        self.run_case("test_l5_cm29_owned_offsets_and_rectangles")

    def test_l5_cm29_absent_and_zero_preserve_full_frame(self):
        self.run_case("test_l5_cm29_absent_and_zero_preserve_full_frame")

    def test_l5_cm29_crop_domain_excludes_adversarial_bars(self):
        self.run_case("test_l5_cm29_crop_domain_excludes_adversarial_bars")

    def test_cm29_detail_holds_at_the_top_clip(self):
        self.run_case("test_cm29_detail_holds_at_the_top_clip")

    def test_cm29_top_clip_state(self):
        self.run_case("test_cm29_top_clip_state")

    def test_cm29_detail_hold_chroma_weight(self):
        self.run_case("test_cm29_detail_hold_chroma_weight")


if __name__ == "__main__":
    unittest.main()
