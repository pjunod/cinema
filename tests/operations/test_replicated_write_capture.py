"""K-03's M0 evaluator: the window rules and the 12-hour gate.

`scripts/replicated-write-capture evaluate` decides whether the replicated-
write capture has an idle window long enough to be the M0 baseline, and
produces the per-voter readout from it. Paul set the gate to 12 hours on
2026-09-25. These cases pin the rules the sampler applies, so the evaluator
cannot quietly accept a window the sampler would have reset.
"""

from __future__ import annotations

import importlib.machinery
import importlib.util
import io
from pathlib import Path
import struct
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "replicated-write-capture"
SAMPLER = ROOT / "scripts" / "replicated-write-capture-sampler"

loader = importlib.machinery.SourceFileLoader("replicated_write_capture", str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
capture = importlib.util.module_from_spec(spec)
loader.exec_module(capture)

HEADER = (
    "timestamp_utc\tepoch\tnode\turl\treachable\tbuild\tlocal_is_voter\tcommit_index\t"
    "authority_reads\twrites\tsnapshot_builds\tpending_outbox\ttranscode_active\t"
    "live_tv_starting\tlive_tv_active\tactive_playback_entries\n"
)


def samples(minutes: int, *, build_at=None, transcode_at=None, rollback_at=None) -> io.StringIO:
    """One sample a minute on the three voters, commit index +600 a minute."""
    out = [HEADER]
    for minute in range(minutes):
        epoch = 1_790_000_000 + 60 * minute
        for node in capture.NODES:
            build = "b2" if build_at is not None and minute >= build_at else "b1"
            commit = 1_000 + 600 * minute
            if rollback_at is not None and minute == rollback_at:
                commit -= 10_000
            transcode = 1 if transcode_at is not None and minute == transcode_at else 0
            out.append(
                f"t\t{epoch}\t{node}\thttp://x\t1\t{build}\t1\t{commit}\t{50 * minute}\t"
                f"{20 * minute}\t{minute // 100}\t0\t{transcode}\t0\t0\t0\n"
            )
    return io.StringIO("".join(out))


class GateCase(unittest.TestCase):
    def test_the_gate_is_twelve_hours_in_the_evaluator_and_the_sampler(self):
        self.assertEqual(capture.GATE_HOURS, 12)
        self.assertIn("required_idle_seconds=43200\n", SAMPLER.read_text())

    def test_a_thirteen_hour_idle_window_qualifies_and_reads_out_per_voter(self):
        result = capture.evaluate(samples(13 * 60 + 1))
        self.assertEqual(len(result["qualifying"]), 1)
        readout = result["qualifying"][0]["readout"]
        self.assertEqual(readout["hours"], 13.0)
        for node in capture.NODES:
            commit = readout["per_node"][node]["commit_index"]
            self.assertEqual(commit["delta"], 600 * 13 * 60)
            self.assertEqual(commit["per_day"], 600 * 60 * 24)

    def test_eleven_hours_does_not_qualify(self):
        self.assertEqual(capture.evaluate(samples(11 * 60))["qualifying"], [])

    def test_a_build_change_resets_the_window(self):
        result = capture.evaluate(samples(20 * 60, build_at=8 * 60))
        # 8 h on b1, then the b2 window starts a sample later: 12 h - 2 min.
        self.assertEqual(result["qualifying"], [])
        self.assertEqual(result["longest"][0]["ended_by"], "still open")

    def test_activity_and_counter_rollback_reset_the_window(self):
        self.assertEqual(
            capture.evaluate(samples(14 * 60, transcode_at=7 * 60))["qualifying"], []
        )
        self.assertEqual(
            capture.evaluate(samples(14 * 60, rollback_at=7 * 60))["qualifying"], []
        )


def wal(records: list[tuple[int, bytes]]) -> bytes:
    body = bytearray(b"HQL_WAL" + bytes([1]) + struct.pack(">QQII", 0, 0, 32, 0))
    for log_id, payload in records:
        body += struct.pack(">Q", log_id) + b"\0\0\0\0" + struct.pack(">I", len(payload))
        body += payload + b"\0"
    return bytes(body) + b"\0" * 64


class AttributionCase(unittest.TestCase):
    def test_entries_are_attributed_to_the_table_and_lease_they_write(self):
        entries = [
            (7, b"UPDATE watched_outbox SET claim_until = $1 WHERE id IN (SELECT 1)"),
            (8, b"INSERT INTO job_leases\n    (resource)\0\x17metadata-classification\0"),
            (9, b"UPDATE watched_outbox SET claim_until = $1"),
            (10, b"\x01\x02"),
        ]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "0000000000000001.wal"
            path.write_bytes(wal(entries))
            result = capture.attribute([path])
        self.assertEqual(result["entries"], 4)
        self.assertTrue(result["contiguous"])
        by = {row["writer"]: row["entries"] for row in result["by_writer"]}
        self.assertEqual(by["UPDATE watched_outbox"], 2)
        self.assertEqual(by["job_leases acquire metadata-classification"], 1)
        self.assertEqual(by["(no SQL: blank, membership or other)"], 1)


if __name__ == "__main__":
    unittest.main()
