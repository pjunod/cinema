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
import copy
import csv
import hashlib
import io
import json
from pathlib import Path
import struct
import tempfile
import unittest
from unittest import mock

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
    def test_external_observer_sanitizes_actual_settings_and_refuses_unbound_deployment(self):
        """Synthetic HTTP adapter only; never a real deployment or twelve-hour run."""
        source, build, now = "a" * 40, "v0.3.0-1-gaaaaaaaaa", 1790000000
        deployment = json.dumps({"schema": "k03-deployment-binding-v1", "observations": [
            dict(node=node, build=build, source_commit=source,
                 origin=url.removesuffix("/metrics")) for node, url in capture.AFTER_URLS.items()
        ]}).encode()
        digest = hashlib.sha256(deployment).hexdigest()
        metrics = (f'plurx_build_info{{version="0.3.0",build="{build}"}} 1\n'
            'plurx_uptime_seconds 1000\nplurx_cluster_local_is_voter 1\n'
            'plurx_raft_commit_index 100\n'
            'plurx_store_operations_total{class="authority_read",method="read"} 2\n'
            'plurx_store_operations_total{class="write",method="write"} 3\n'
            'plurx_raft_snapshot_seconds_count{operation="build"} 0\n'
            'plurx_watched_outbox{status="pending"} 0\n'
            'plurx_transcode_sessions_active 0\n'
            'plurx_live_tv_sessions{state="starting"} 0\n'
            'plurx_live_tv_sessions{state="active"} 0\n'
            'plurx_cache_protected_entries{reason="active_playback"} 0\n'
            'plurx_takeover_settings_authority_reads_started_total 0\n' + ''.join(
                f'plurx_watched_outbox_ticks_total{{outcome="{name}"}} 0\n'
                for name in capture.OUTCOMES)).encode()
        settings = {"cluster_media_pool_enabled": False, "cluster_session_takeover_enabled": True,
                    "curator_api_key": "SECRET-DTO", "other_private": "SECRET-OTHER"}
        calls = []
        def fetch(url, token):
            calls.append((url, token))
            if url.endswith("/metrics"):
                self.assertIsNone(token)
                return metrics
            self.assertTrue(url.endswith("/api/v1/settings"))
            self.assertEqual(token, "SECRET-TOKEN")
            return json.dumps(settings).encode()
        credentials = dict.fromkeys(capture.NODES, "SECRET-TOKEN")
        raw = capture.observe_after_tick(deployment, digest, credentials, fetch, lambda: now)
        self.assertNotIn(b"SECRET", raw)
        for node in capture.NODES:
            self.assertEqual(capture.after_acquisition(raw, now, node, build, source)["node"], node)
        self.assertEqual(len(calls), 6)
        with self.assertRaises(ValueError):
            capture.observe_after_tick(deployment, "0" * 64, credentials, fetch, lambda: now)
        self.assertEqual(len(calls), 6)  # refuses before any HTTP adapter call
        for replacement in (None, "false", True):
            settings["cluster_media_pool_enabled"] = replacement
            with self.assertRaises(ValueError):
                capture.observe_after_tick(deployment, digest, credentials, fetch, lambda: now)
        settings["cluster_media_pool_enabled"] = False
        changed = json.loads(deployment)
        changed["observations"][0]["source_commit"] = "b" * 40
        unbound = json.dumps(changed).encode()
        with self.assertRaises(ValueError):
            capture.observe_after_tick(unbound, hashlib.sha256(unbound).hexdigest(), credentials, fetch, lambda: now)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "deployment").write_bytes(deployment)
            (root / "credentials").write_text(json.dumps(credentials))
            class Response(io.BytesIO):
                status = 200
            def opened(request, timeout):
                self.assertEqual(timeout, 5)
                return Response(metrics if request.full_url.endswith("/metrics") else json.dumps(settings).encode())
            transport = mock.Mock()
            transport.open.side_effect = opened
            owner = root / "observer"
            with mock.patch.object(capture.urllib.request, "build_opener", return_value=transport), \
                 mock.patch.object(capture.signal, "getitimer", return_value=(0.0, 0.0)), \
                 mock.patch.object(capture.signal, "signal"), \
                 mock.patch.object(capture.signal, "setitimer") as timer, \
                 mock.patch.object(capture.time, "time", return_value=now):
                self.assertEqual(capture.observe_after(owner, root / "deployment", digest,
                                                      root / "credentials", 1), 0)
                self.assertEqual(timer.call_args.args, (capture.signal.ITIMER_REAL, 0))
            self.assertEqual((owner / "manifest.json").read_bytes(), raw)
            self.assertTrue(all(b"SECRET" not in path.read_bytes() for path in owner.iterdir()))
            with self.assertRaises(FileExistsError):
                capture.observe_after(owner, root / "deployment", digest, root / "credentials", 1)

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

    def test_strict_after_requires_fresh_bound_evidence_and_exact_elapsed_rates(self):
        """ONE offline control; synthetic acquisition bytes are not a fleet run."""
        source = "a" * 40
        build = "v0.3.0-1-gaaaaaaaaa"
        epoch0 = 1_790_000_000
        with tempfile.TemporaryDirectory() as directory:
            evidence = Path(directory)

            def retain(raw, suffix):
                digest = hashlib.sha256(raw).hexdigest()
                (evidence / (digest + suffix)).write_bytes(raw)
                return digest

            def metrics(minute, elapsed, node_index):
                return (
                    f'plurx_build_info{{version="0.3.0",build="{build}"}} 1\n'
                    f'plurx_uptime_seconds {1000 + elapsed + node_index}\n'
                    'plurx_cluster_local_is_voter 1\n'
                    f'plurx_raft_commit_index {1000 + 10 * minute}\n'
                    f'plurx_store_operations_total{{class="authority_read",method="read"}} {minute * 2}\n'
                    f'plurx_store_operations_total{{class="write",method="write"}} {minute * 3}\n'
                    f'plurx_raft_snapshot_seconds_count{{operation="build"}} {minute // 100}\n'
                    'plurx_watched_outbox{status="pending"} 0\n'
                    'plurx_transcode_sessions_active 0\n'
                    'plurx_live_tv_sessions{state="starting"} 0\n'
                    'plurx_live_tv_sessions{state="active"} 0\n'
                    'plurx_cache_protected_entries{reason="active_playback"} 0\n'
                    f'plurx_takeover_settings_authority_reads_started_total {minute}\n'
                    + ''.join(f'plurx_watched_outbox_ticks_total{{outcome="{outcome}"}} {minute * (index + 1)}\n'
                              for index, outcome in enumerate(capture.OUTCOMES))
                ).encode()

            rows = []
            for minute in range(721):
                elapsed = minute * 60 + (7 if minute == 720 else 0)
                tick = epoch0 + elapsed
                acquisition = {"schema": capture.AFTER_SCHEMA, "observations": [
                    {"node": node, "build": build, "source_commit": source, "epoch": tick,
                     "cluster_media_pool_enabled": False, "cluster_session_takeover_enabled": True}
                    for node in capture.NODES
                ]}
                acquisition_hash = retain(json.dumps(acquisition).encode(), ".json")
                for index, node in enumerate(capture.NODES):
                    parsed, raw = capture.after_metrics(metrics(minute, elapsed, index))
                    observed = tick + index
                    rows.append(dict(parsed, timestamp_utc=capture.iso(observed), epoch=str(tick),
                                     observed_epoch=str(observed), node=node, url=capture.AFTER_URLS[node],
                                     reachable="1", acquisition_sha256=acquisition_hash,
                                     metrics_sha256=retain(raw, ".prom")))

            def evaluate(candidate, expected=build, expected_source=source):
                text = io.StringIO()
                writer = csv.DictWriter(text, fieldnames=capture.AFTER_FIELDS, delimiter="\t")
                writer.writeheader()
                writer.writerows(candidate)
                text.seek(0)
                return capture.evaluate_after(text, evidence, expected, expected_source)

            result = evaluate(rows)
            self.assertTrue(result["measurement_eligible"])
            self.assertTrue(result["takeover_bound_met"])
            self.assertEqual(result["overlap_seconds"], 43205)
            for node in capture.NODES:
                rate = result["per_node"][node]["takeover_started"]
                self.assertEqual(rate["delta"], 720)
                self.assertEqual(rate["elapsed_seconds"], 43207)
                self.assertEqual(rate["per_day_numerator"], 720 * 86400)
                self.assertEqual(rate["per_day_denominator"], 43207)
                self.assertAlmostEqual(rate["per_day"], 720 * 86400 / 43207)
            self.assertIn("not assessed", result["outbox_wal_proposal_bound"])
            self.assertFalse(evaluate(rows[:6])["measurement_eligible"])

            def metric_change(candidate, index, old, new):
                row = candidate[index]
                raw = (evidence / (row["metrics_sha256"] + ".prom")).read_bytes()
                changed = raw.replace(old, new)
                row["metrics_sha256"] = retain(changed, ".prom")
                # When valid, bind TSV to the altered real fixture bytes too;
                # reset/role failures must not merely be hash mismatches.
                try:
                    parsed, _ = capture.after_metrics(changed)
                except ValueError:
                    return
                row.update(parsed)

            def acquisition_change(candidate, index, modify):
                row = candidate[index]
                raw = (evidence / (row["acquisition_sha256"] + ".json")).read_bytes()
                data = json.loads(raw)
                modify(data)
                row["acquisition_sha256"] = retain(json.dumps(data).encode(), ".json")

            def refuse(label, candidate):
                with self.subTest(refusal=label), self.assertRaises((ValueError, OSError)):
                    evaluate(candidate)

            missing = copy.deepcopy(rows); missing[0]["uptime_seconds"] = ""
            refuse("missing", missing)
            absent = copy.deepcopy(rows); absent[0]["metrics_sha256"] = "0" * 64
            refuse("missing retained bytes", absent)
            duplicate = copy.deepcopy(rows); duplicate.insert(0, duplicate[0].copy())
            refuse("duplicate tick", duplicate)
            refuse("missing roster", rows[1:])
            unrefreshed = copy.deepcopy(rows)
            for row in unrefreshed:
                row["acquisition_sha256"] = rows[0]["acquisition_sha256"]
            refuse("unchanged initial receipt", unrefreshed)
            for label, offset in (("stale", -96), ("future", 1)):
                candidate = copy.deepcopy(rows)
                acquisition_change(candidate, 0, lambda data, offset=offset: data["observations"][0].update(epoch=epoch0 + offset))
                refuse(label, candidate)
            for label, mutation in (
                ("on-on", {"cluster_media_pool_enabled": True}),
                ("non-boolean", {"cluster_media_pool_enabled": "false"}),
                ("source", {"source_commit": "b" * 40}),
                ("build", {"build": "unknown"}),
            ):
                candidate = copy.deepcopy(rows)
                acquisition_change(candidate, 0, lambda data, mutation=mutation: data["observations"][0].update(mutation))
                refuse(label, candidate)
            for label, index, old, new in (
                ("missing numerator", 0, b'plurx_takeover_settings_authority_reads_started_total 0\n', b''),
                ("malformed", 0, b'plurx_takeover_settings_authority_reads_started_total 0', b'plurx_takeover_settings_authority_reads_started_total NaN'),
                ("saturated", 0, b'plurx_takeover_settings_authority_reads_started_total 0', f'plurx_takeover_settings_authority_reads_started_total {capture.U64_LIMIT}'.encode()),
                ("counter reset", 3, b'plurx_raft_commit_index 1010', b'plurx_raft_commit_index 999'),
                ("uptime reset", 3, b'plurx_uptime_seconds 1060', b'plurx_uptime_seconds 1'),
                ("wrong role", 0, b'plurx_cluster_local_is_voter 1', b'plurx_cluster_local_is_voter 0'),
            ):
                candidate = copy.deepcopy(rows)
                metric_change(candidate, index, old, new)
                refuse(label, candidate)
            gap = copy.deepcopy(rows)
            gap[3].update(epoch=str(epoch0 + 96), observed_epoch=str(epoch0 + 96), timestamp_utc=capture.iso(epoch0 + 96))
            refuse("gap", gap)
            mismatch = copy.deepcopy(rows); mismatch[0]["takeover_started"] = "1"
            refuse("unbound TSV value", mismatch)
            for wrong_build, wrong_source in (("unknown", source), (build, "b" * 40), (build, "a" * 9)):
                with self.subTest(expected_source=wrong_source), self.assertRaises(ValueError):
                    evaluate(rows, wrong_build, wrong_source)
            # A legitimate measured high rate remains evidence, not "idle is
            # safe": exact inequality separately refuses acceptance.
            high = copy.deepcopy(rows)
            for index, row in enumerate(high):
                minute = index // 3
                metric_change(high, index,
                              f'plurx_takeover_settings_authority_reads_started_total {minute}\n'.encode(),
                              f'plurx_takeover_settings_authority_reads_started_total {minute * 2}\n'.encode())
            high_result = evaluate(high)
            self.assertTrue(high_result["measurement_eligible"])
            self.assertFalse(high_result["takeover_bound_met"])

            # Exercise the actual sampler's manifest-read loop with finite
            # synthetic time/I/O. This is NOT HTTP-origin or 12-hour evidence.
            manifest = evidence / "refresh.json"
            wall = [epoch0]

            def refresh():
                manifest.write_text(json.dumps({"schema": capture.AFTER_SCHEMA,
                    "observations": [{"node": node, "build": build,
                        "source_commit": source, "epoch": wall[0],
                        "cluster_media_pool_enabled": False,
                        "cluster_session_takeover_enabled": True}
                        for node in capture.NODES]}))

            def sleep(seconds):
                wall[0] += int(seconds)
                refresh()

            class Response(io.BytesIO):
                status = 200

            def opened(url, timeout):
                self.assertEqual(timeout, 5)
                index = list(capture.AFTER_URLS.values()).index(url)
                elapsed = wall[0] - epoch0
                return Response(metrics(elapsed // 60, elapsed, index))

            refresh()
            transport = mock.Mock()
            transport.open.side_effect = opened
            owner = evidence / "finite-sampler-owner"
            with mock.patch.object(capture.urllib.request, "build_opener", return_value=transport), \
                 mock.patch.object(capture.time, "time", side_effect=lambda: wall[0]), \
                 mock.patch.object(capture.time, "monotonic", side_effect=lambda: wall[0]), \
                 mock.patch.object(capture.time, "sleep", side_effect=sleep), \
                 mock.patch.object(capture.signal, "getitimer", return_value=(0.0, 0.0)), \
                 mock.patch.object(capture.signal, "signal") as handler, \
                 mock.patch.object(capture.signal, "setitimer") as timer:
                with self.assertRaises(TimeoutError):
                    capture.sample_after(owner, manifest, build, source, 121)
                self.assertEqual(timer.call_args.args, (capture.signal.ITIMER_REAL, 0))
                self.assertEqual(handler.call_count, 2)
            with (owner / "samples.tsv").open() as handle:
                acquired = list(csv.DictReader(handle, delimiter="\t"))
            self.assertEqual(transport.open.call_count, 9)
            self.assertEqual(len(acquired), 9)
            self.assertEqual(len({row["acquisition_sha256"] for row in acquired}), 3)
            for row in acquired:
                capture.after_acquisition(capture.after_evidence(owner / "evidence",
                    row["acquisition_sha256"], ".json"), int(row["observed_epoch"]),
                    row["node"], build, source)
            with self.assertRaises(FileExistsError):
                capture.sample_after(owner, manifest, build, source, 1)


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
