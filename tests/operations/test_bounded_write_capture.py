"""Bounded offline evidence controls; no fleet, deployment or soak run."""

from __future__ import annotations

import contextlib
import copy
import csv
import hashlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

from tests.operations.test_replicated_write_capture import capture, FLEET


class BoundedCaptureCase(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.evidence = self.root / "evidence"
        self.evidence.mkdir()
        self.source = "a" * 40
        self.build = "v0.3.0-1-gaaaaaaaaa"
        self.epoch = 1_790_000_000

    def retain(self, raw, suffix):
        digest = hashlib.sha256(raw).hexdigest()
        (self.evidence / (digest + suffix)).write_bytes(raw)
        return digest

    def metrics(self, second, *, starts=0, uptime=None, build=None, active=0):
        return (
            f'plurx_build_info{{version="0.3.0",build="{build or self.build}"}} 1\n'
            f'plurx_uptime_seconds {uptime if uptime is not None else 1000 + second}\n'
            'plurx_cluster_local_is_voter 1\n'
            f'plurx_raft_commit_index {1000 + second}\n'
            f'plurx_store_operations_total{{class="authority_read",method="read"}} {second}\n'
            f'plurx_store_operations_total{{class="write",method="write"}} {second}\n'
            'plurx_raft_snapshot_seconds_count{operation="build"} 0\n'
            'plurx_watched_outbox{status="pending"} 0\n'
            f'plurx_transcode_sessions_active {active}\n'
            'plurx_live_tv_sessions{state="starting"} 0\n'
            'plurx_live_tv_sessions{state="active"} 0\n'
            'plurx_cache_protected_entries{reason="active_playback"} 0\n'
            f'plurx_takeover_settings_authority_reads_started_total {starts}\n'
            + ''.join(f'plurx_watched_outbox_ticks_total{{outcome="{outcome}"}} 0\n'
                      for outcome in capture.OUTCOMES)
        ).encode()

    def acquisition(self, second, *, build=None, source=None, enabled=False):
        return json.dumps({"schema": capture.AFTER_SCHEMA, "observations": [
            {"node": node, "build": build or self.build, "source_commit": source or self.source,
             "epoch": self.epoch + second, "cluster_media_pool_enabled": enabled,
             "cluster_session_takeover_enabled": False} for node in capture.NODES
        ]}).encode()

    def tick(self, second, *, source=None, enabled=False, observation_offset=None, **metrics_options):
        acquisition = self.retain(self.acquisition(second, source=source, enabled=enabled,
                                                 build=metrics_options.get("build")), ".json")
        rows = []
        for offset, node in enumerate(capture.NODES):
            if observation_offset is not None:
                offset = observation_offset
                metrics_options.setdefault("uptime", 1000 + second + offset)
            parsed, raw = capture.after_metrics(self.metrics(second, **metrics_options))
            observed = self.epoch + second + offset
            rows.append(dict(parsed, timestamp_utc=capture.iso(observed), epoch=str(self.epoch + second),
                             node=node, url=capture.AFTER_URLS[node], reachable="1",
                             observed_epoch=str(observed), acquisition_sha256=acquisition,
                             metrics_sha256=self.retain(raw, ".prom")))
        return rows

    @staticmethod
    def text(rows):
        buffer = io.StringIO()
        writer = csv.DictWriter(buffer, fieldnames=capture.AFTER_FIELDS, delimiter="\t")
        writer.writeheader()
        writer.writerows(rows)
        buffer.seek(0)
        return buffer

    def evaluate(self, rows):
        return capture.evaluate_bounded_after(self.text(rows), self.evidence, self.build, self.source)

    def test_boundaries_preserve_segments_without_stitching_or_new_source_attestation(self):
        rows = self.tick(0) + self.tick(60, starts=1) + self.tick(120, starts=2)
        rows += self.tick(180, starts=0, uptime=10)
        rows += self.tick(240, active=1)
        rows += self.tick(300, build="v0.3.0-1-gbbbbbbbbb", source="b" * 40)
        rows += self.tick(360, starts=0) + self.tick(420, starts=0)
        rows += self.tick(480, enabled=True) + self.tick(540, enabled=True)
        result = self.evaluate(rows)
        self.assertEqual([(s["start_tick"] - self.epoch, s["end_tick"] - self.epoch)
                          for s in result["segments"]], [(0, 120), (180, 180), (360, 420), (480, 540)])
        self.assertEqual([s["measurement_eligible"] for s in result["segments"]], [True, False, True, True])
        self.assertEqual([t["reason"] for t in result["excluded_ticks"]],
                         ["activity_or_nonvoter", "outside_admitted_source_binding"])
        self.assertEqual([t["reason"] for t in result["boundaries"]],
                         ["counter_uptime_or_gap_boundary", "switch_pair_changed"])
        self.assertTrue(result["bounded_cost_observations_met"])
        self.assertFalse(result["full_acceptance"])
        self.assertEqual(result["long_duration_stability"], "not assessed")
        for rates in result["segments"][0]["per_node"].values():
            self.assertEqual(rates["takeover_started"]["per_day_numerator"], 2 * 86400)
            self.assertEqual(rates["takeover_started"]["per_day_denominator"], 120)
        high = self.evaluate(self.tick(0) + self.tick(120, starts=3))
        # The gap itself separates observations and cannot produce a pass.
        self.assertFalse(high["bounded_cost_observations_met"])
        high = self.evaluate(self.tick(0) + self.tick(60, starts=2) + self.tick(120, starts=3))
        self.assertFalse(high["bounded_cost_observations_met"])

    def test_partial_and_invalid_inputs_cannot_be_zero_or_extend_the_campaign(self):
        rows = self.tick(0) + self.tick(60) + self.tick(120)
        partial = self.evaluate(rows + self.tick(180)[:1])
        self.assertEqual(partial["excluded_ticks"][0]["reason"], "incomplete_tick_roster")
        self.assertEqual(partial["segments"][0]["end_tick"], self.epoch + 120)
        self.assertFalse(self.evaluate(self.tick(0))["bounded_cost_observations_met"])
        duplicate = rows + [rows[-1].copy()]
        missing = copy.deepcopy(rows)
        missing[0]["metrics_sha256"] = "0" * 64
        stale = copy.deepcopy(rows)
        stale[-1]["acquisition_sha256"] = rows[0]["acquisition_sha256"]
        for name, candidate in (("duplicate", duplicate), ("missing", missing), ("stale", stale),
                                ("over cap", rows + self.tick(1800))):
            with self.subTest(name=name), self.assertRaises((ValueError, OSError)):
                self.evaluate(candidate)
        path = self.evidence / (rows[0]["metrics_sha256"] + ".prom")
        path.write_bytes(path.read_bytes() + b"# tampered\n")
        with self.assertRaises(ValueError):
            self.evaluate(rows)

    def test_rolling_deployment_excludes_only_the_mixed_tick_and_retains_both_sides(self):
        mixed = self.tick(120)
        acquisition = json.loads((self.evidence / (mixed[0]["acquisition_sha256"] + ".json")).read_bytes())
        new_build, new_source = "v0.3.0-1-gbbbbbbbbb", "b" * 40
        acquisition["observations"][0].update(build=new_build, source_commit=new_source)
        digest = self.retain(json.dumps(acquisition).encode(), ".json")
        for row in mixed:
            row["acquisition_sha256"] = digest
        parsed, raw = capture.after_metrics(self.metrics(120, build=new_build))
        mixed[0].update(parsed, metrics_sha256=self.retain(raw, ".prom"))
        result = self.evaluate(self.tick(0) + self.tick(60) + mixed + self.tick(180) + self.tick(240))
        self.assertEqual([(s["start_tick"] - self.epoch, s["end_tick"] - self.epoch)
                          for s in result["segments"]], [(0, 60), (180, 240)])
        self.assertEqual(result["excluded_ticks"], [{"tick": self.epoch + 120,
                         "reason": "outside_admitted_source_binding", "observed_nodes": sorted(capture.NODES)}])
        self.assertTrue(result["bounded_cost_observations_met"])
        self.assertFalse(result["full_acceptance"])
        # Mixed tuples are not permission to mismatch the actual node's bytes.
        bad = copy.deepcopy(mixed)
        bad[0].update(capture.after_metrics(self.metrics(120))[0],
                      metrics_sha256=self.retain(capture.after_metrics(self.metrics(120))[1], ".prom"))
        with self.assertRaises(ValueError):
            self.evaluate(self.tick(0) + self.tick(60) + bad)

    def test_tick_gap_splits_even_when_request_offsets_hide_the_observed_gap(self):
        rows = self.tick(0, observation_offset=20) + self.tick(60, observation_offset=20)
        rows += self.tick(160, observation_offset=0) + self.tick(220, observation_offset=0)
        result = self.evaluate(rows)
        self.assertEqual([(s["start_tick"] - self.epoch, s["end_tick"] - self.epoch)
                          for s in result["segments"]], [(0, 60), (160, 220)])
        self.assertEqual(result["boundaries"], [{"tick": self.epoch + 160,
                                               "reason": "counter_uptime_or_gap_boundary"}])
        self.assertTrue(result["bounded_cost_observations_met"])
        self.assertEqual([s["overlap_seconds"] for s in result["segments"]], [60, 60])

    def test_actual_cli_reports_bounded_cost_without_a_twelve_hour_gate(self):
        rows = self.tick(0) + self.tick(60) + self.tick(120)
        samples = self.root / "samples.tsv"
        samples.write_text(self.text(rows).getvalue())
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = capture.main(["evaluate-bounded-after", str(samples), "--evidence-dir", str(self.evidence),
                                 "--expected-build", self.build, "--source-commit", self.source,
                                 "--fleet", str(FLEET)])
        self.assertEqual(code, 0)
        result = json.loads(output.getvalue())
        self.assertEqual(result["mode"], "bounded_after")
        self.assertEqual(result["wall_clock_cap_seconds"], 1800)
        self.assertEqual(result["campaign_span_seconds"], 122)
        self.assertFalse(result["full_acceptance"])

    def test_actual_bounded_sampler_keeps_one_deadline_and_closes_owned_io(self):
        clock = [0]
        root = self.root / "sample-owner"
        acquisition_path = self.root / "external-manifest.json"
        original_read = capture.after_read_regular

        def read(path, limit):
            if Path(path) == acquisition_path:
                return self.acquisition(clock[0])
            return original_read(path, limit)

        class Response(io.BytesIO):
            status = 200

        opener = mock.Mock()
        opener.open.side_effect = lambda *a, **kw: Response(self.metrics(clock[0]))
        output = io.StringIO()
        with mock.patch.object(capture.time, "monotonic", side_effect=lambda: clock[0]), \
             mock.patch.object(capture.time, "time", side_effect=lambda: self.epoch + clock[0]), \
             mock.patch.object(capture.time, "sleep", side_effect=lambda seconds: clock.__setitem__(0, clock[0] + seconds)), \
             mock.patch.object(capture.signal, "getitimer", return_value=(0.0, 0.0)), \
             mock.patch.object(capture.signal, "setitimer") as timer, \
             mock.patch.object(capture.signal, "signal"), \
             mock.patch.object(capture.urllib.request, "build_opener", return_value=opener), \
             mock.patch.object(capture, "after_read_regular", side_effect=read), \
             contextlib.redirect_stdout(output):
            code = capture.sample_after(root, acquisition_path, self.build, self.source, 180, bounded=True)
        self.assertEqual(code, 0)
        self.assertEqual(clock[0], 120)
        self.assertEqual(opener.open.call_count, 3 * len(capture.NODES))
        self.assertEqual(timer.call_args_list[-1].args, (capture.signal.ITIMER_REAL, 0))
        self.assertTrue((root / "samples.tsv").is_file())
        self.assertFalse(json.loads(output.getvalue())["full_acceptance"])
        with self.assertRaises(FileExistsError):
            capture.sample_after(root, acquisition_path, self.build, self.source, 180, bounded=True)
        with self.assertRaises(ValueError):
            capture.sample_after(self.root / "too-long", acquisition_path, self.build, self.source, 1801, bounded=True)
        self.assertFalse((self.root / "too-long").exists())


if __name__ == "__main__":
    unittest.main()
