"""Focused false-acceptance controls for the bounded reuse evidence checker."""
import copy
import json
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
import check_reuse as checker

SOURCE = Path(sys.argv.pop(1))


class EvidenceControls(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="reuse-check-", dir=SOURCE)
        self.root = Path(self.temporary.name)
        for mode in ("normal", "missing-rpu", "seek-reset"):
            shutil.copytree(SOURCE / mode, self.root / mode)
        for path in SOURCE.iterdir():
            if path.is_file() and (path.name in ("decode_reuse", "cache_probe") or path.suffix in (".mkv", ".nal", ".json", ".jsonl", ".status", ".stderr", ".gz", ".yuv420p10le")):
                shutil.copyfile(path, self.root / path.name)
        relative = "dovi_tool-83e1fdad6dcd5995556235946e7c5c0f9010d5a1/target/release/examples/m0_inspect_reuse"
        (self.root / relative).parent.mkdir(parents=True)
        shutil.copyfile(SOURCE / relative, self.root / relative)

    def tearDown(self):
        self.temporary.cleanup()

    def events(self, mode, change):
        path = self.root / mode / "events.jsonl"
        rows = checker.lines(path)
        change(rows)
        path.write_text("".join(json.dumps(row, allow_nan=False) + "\n" for row in rows))
        self.rebind(path, self.root / mode / "execution.json")

    def rebind(self, path, receipt):
        record = checker.load(receipt)
        record["outputs"][str(path.relative_to(self.root))] = checker.digest(path)
        receipt.write_text(json.dumps(record))

    def refused(self, reason):
        with self.assertRaisesRegex(ValueError, reason):
            checker.inspect(self.root)

    def test_baseline_passes(self):
        result = checker.inspect(self.root)
        self.assertEqual([row["accepted"] for row in result["decoded"]], [True, True, False])

    def test_late_seek_boundary(self):
        def change(rows):
            index = next(i for i, row in enumerate(rows) if row["kind"] == "epoch_boundary")
            rows.append(rows.pop(index))
        self.events("seek-reset", change)
        self.refused("ordered decode lifecycle")

    def test_missing_drain(self):
        self.events("normal", lambda rows: rows.pop())
        self.refused("ordered decode lifecycle")

    def test_duplicate_reset(self):
        self.events("seek-reset", lambda rows: rows.insert(11, copy.deepcopy(rows[10])))
        self.refused("ordered decode lifecycle")

    def test_unapplied_reset(self):
        self.events("seek-reset", lambda rows: rows[10].update(reset_applied=False))
        self.refused("ordered decode lifecycle")

    def test_wrong_pts(self):
        self.events("normal", lambda rows: next(row for row in rows if row["kind"] == "decoded_frame").update(pts="999/1"))
        self.refused("ordered decode lifecycle")

    def test_wrong_table(self):
        self.events("normal", lambda rows: next(row for row in rows if row["kind"] == "resolved_metadata")["curves"][0].update(coefficients=[1, 2, 3]))
        self.refused("ordered decode lifecycle")

    def test_bool_for_int(self):
        self.events("normal", lambda rows: next(row for row in rows if row["kind"] == "resolved_metadata").update(mapping_id=False))
        self.refused("ordered decode lifecycle")

    def test_overflow(self):
        path = self.root / "normal/events.jsonl"
        path.write_text(path.read_text().replace('"source_min_pq":0', '"source_min_pq":1e400', 1))
        with self.assertRaisesRegex(ValueError, "noninteger/nonfinite"):
            checker.lines(path)
        self.rebind(path, self.root / "normal/execution.json")
        self.refused("noninteger/nonfinite")

    def test_nan(self):
        path = self.root / "normal/events.jsonl"
        path.write_text(path.read_text().replace('"source_min_pq":0', '"source_min_pq":NaN', 1))
        with self.assertRaisesRegex(ValueError, "noninteger/nonfinite"):
            checker.lines(path)
        self.rebind(path, self.root / "normal/execution.json")
        self.refused("noninteger/nonfinite")

    def test_crash_after_complete_writes(self):
        (self.root / "normal/decode-status.txt").write_text("139\n")
        self.refused("decode process did not exit cleanly")

    def test_wrong_cache_status(self):
        (self.root / "cache-flush.status").write_text("139\n")
        self.refused("cache process did not exit cleanly")

    def test_wrong_failure_reason(self):
        path = self.root / "cache-flush.stderr"
        path.write_text("unrelated fixture corruption\n")
        self.rebind(path, self.root / "cache-flush.execution.json")
        self.refused("cache refusal diagnostic")

    def test_wrong_failure_stage(self):
        path = self.root / "cache-flush.jsonl"
        rows = checker.lines(path)
        rows[2], rows[3] = rows[3], rows[2]
        path.write_text("".join(json.dumps(row) + "\n" for row in rows))
        self.rebind(path, self.root / "cache-flush.execution.json")
        self.refused("ordered typed cache")

    def test_stale_parsed_binding(self):
        path = self.root / "normal/epoch-0-bl-frame-001.rpu.nal"
        path.write_bytes(path.read_bytes()[:-1] + b"x")
        self.refused("parsed output/raw input provenance")

    def test_missing_provenance_binding(self):
        path = self.root / "parsed-provenance.json"
        rows = checker.load(path)
        del rows["normal/epoch-0-bl-frame-001.rpu.nal"]
        path.write_text(json.dumps(rows))
        self.refused("provenance coverage")

    def test_missing_reuse_id(self):
        path = self.root / "normal/epoch-0-bl-frame-001.rpu.json"
        rows = checker.load(path)
        rows["header"]["prev_vdr_rpu_id"] = 1
        path.write_text(json.dumps(rows))
        self.refused("parsed output/raw input provenance")

    def test_swapped_native_picture(self):
        path = self.root / "normal/epoch-0-bl-frame-001.yuv420p10le"
        path.write_bytes((self.root / "normal/epoch-0-bl-frame-002.yuv420p10le").read_bytes())
        self.rebind(path, self.root / "normal/execution.json")
        self.refused("native decoded picture")

    def test_changed_source_lock(self):
        path = self.root / "input-lock.json"
        rows = checker.load(path)
        rows["bl.yuv420p10le"] = "0" * 64
        path.write_text(json.dumps(rows))
        self.refused("source-bound identities")

    def test_container_substitution(self):
        (self.root / "normal.mkv").write_bytes(b"not a Matroska container")
        self.refused("execution input/tool/output identity changed")

    def test_cache_container_substitution(self):
        (self.root / "normal.mkv").write_bytes(b"not a Matroska container")
        with self.assertRaisesRegex(ValueError, "execution input/tool/output identity changed"):
            checker.inspect_cache(self.root, "same")

    def test_cache_rbsp_substitution(self):
        (self.root / "normal/epoch-0-bl-frame-001.rpu.rbsp").write_bytes(b"stale reuse")
        with self.assertRaisesRegex(ValueError, "execution input/tool/output identity changed"):
            checker.inspect_cache(self.root, "same")

    def test_helper_substitution(self):
        (self.root / "decode_reuse").write_bytes(b"wrong helper")
        self.refused("execution input/tool/output identity changed")

    def test_invocation_substitution(self):
        path = self.root / "normal/execution.json"
        record = checker.load(path)
        record["argv"][-1] = "seek-reset"
        path.write_text(json.dumps(record))
        self.refused("execution invocation/status mismatch")

    def test_output_coverage_removed(self):
        path = self.root / "normal/execution.json"
        record = checker.load(path)
        del record["outputs"]["normal/events.jsonl"]
        path.write_text(json.dumps(record))
        self.refused("execution output coverage")

    def test_p7_wrong_status(self):
        (self.root / "p7-refusal.status").write_text("139\n")
        self.refused("P7 compression must refuse")

    def test_p7_output_written(self):
        (self.root / "p7-refusal.mkv").write_bytes(b"unexpected")
        self.refused("P7 compression must refuse")


if __name__ == "__main__":
    unittest.main()
