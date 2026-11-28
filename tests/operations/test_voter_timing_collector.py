"""Read-only timing collector safety and observation-cohort contracts."""
from contextlib import contextmanager
import importlib.util
import io
from pathlib import Path
import unittest
from unittest import mock
import signal
import socketserver
import threading
import time
import urllib.request
SPEC = importlib.util.spec_from_file_location("voter_timing", Path(__file__).resolve().parents[2] / "scripts/collect-voter-timing.py")
collector = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(collector)
def node():
    return {"name":"voter-a", "url":"http://127.0.0.1:32400", "expected_source":"abcdef012" + "0"*31, "expected_build":"tag-1-gabcdef012", "expected_role":"voter"}
BODY = '\n'.join([
 'plurx_build_info{version="0.8.0",build="tag-1-gabcdef012"} 1',
 'plurx_cluster_timing_epoch_info{epoch="000000000000abcd"} 1',
 'plurx_cluster_replicated 1',
 'plurx_cluster_local_is_voter 1',
 'plurx_cluster_membership_sample_valid 1',
 'plurx_cluster_membership_sample_age_seconds 1',
 'plurx_raft_current_term 3',
 'plurx_cluster_append_observation_samples_total{group="db",stage="rpc_attempt",local="1",remote="2",source="5",term="3",endpoint="abc"} 7',
 'unrelated_sensitive_info{title="must not persist"} 1',
])
@contextmanager
def owned_http_server(body=BODY, slow_drip=False):
    stop = threading.Event()
    state = {"requests": 0, "handlers": []}
    class Handler(socketserver.BaseRequestHandler):
        def handle(self):
            state["handlers"].append(threading.current_thread())
            self.request.settimeout(0.5)
            try:
                self.request.recv(4096)
                state["requests"] += 1
                payload = ("x" * 1000 if slow_drip else body).encode()
                header = f"HTTP/1.1 200 OK\r\nContent-Length: {len(payload)}\r\nConnection: close\r\n\r\n".encode()
                self.request.sendall(header)
                if slow_drip:
                    for byte in payload:
                        if stop.wait(0.025):
                            break
                        self.request.sendall(bytes([byte]))
                else:
                    self.request.sendall(payload)
            except (OSError, TimeoutError):
                pass
    class Server(socketserver.ThreadingTCPServer):
        allow_reuse_address = True
        daemon_threads = True
    server = Server(("127.0.0.1", 0), Handler)
    worker = threading.Thread(target=lambda: server.serve_forever(poll_interval=0.01), daemon=True)
    worker.start()
    target = node()
    target["url"] = f"http://127.0.0.1:{server.server_address[1]}"
    try:
        yield target, state
    finally:
        stop.set()
        server.shutdown()
        server.server_close()
        worker.join(timeout=1)
        for handler in state["handlers"]:
            handler.join(timeout=1)

class VoterTimingCollectorTests(unittest.TestCase):
    def test_only_explicit_origins_and_bounded_targets_are_accepted(self):
        self.assertEqual(collector.validate_nodes({"nodes":[node()]}), [node()])
        for url in ["http://user:password@localhost", "http://localhost/?token=secret", "http://localhost/#fragment", "file:///secret"]:
            bad=node();bad["url"]=url
            with self.assertRaises(ValueError):collector.validate_nodes({"nodes":[bad]})
        with self.assertRaises(ValueError):collector.validate_nodes({"nodes":[node()]*17})
    def test_filtered_samples_preserve_source_role_term_and_process_identity(self):
        sample=collector.parse_metrics(BODY,node())
        self.assertTrue(sample["valid"])
        self.assertEqual(sample["process_epoch"],"000000000000abcd")
        self.assertEqual(len(sample["series"]),1)
        self.assertNotIn("must not persist",str(sample))
    def test_absent_or_unproved_instrumentation_is_not_fleet_evidence(self):
        for body in [BODY.replace('plurx_cluster_timing_epoch_info{epoch="000000000000abcd"} 1',''), BODY.replace('age_seconds 1','age_seconds 11'), BODY.replace('local_is_voter 1','local_is_voter 0'), BODY.replace('gabcdef012','g123456789')]:
            self.assertFalse(collector.parse_metrics(body,node())["valid"])
    def test_process_changes_start_a_new_observation_cohort(self):
        prior={};a=collector.parse_metrics(BODY,node());collector.mark_cohort(a,prior)
        self.assertFalse(a["cohort_changed"])
        b=collector.parse_metrics(BODY.replace('epoch="000000000000abcd"','epoch="000000000000dcba"'),node());collector.mark_cohort(b,prior)
        self.assertTrue(b["cohort_changed"])
    def test_output_capacity_is_hard_and_never_partially_writes_a_record(self):
        stream=io.BytesIO();self.assertTrue(collector.write_record(stream,{"a":1},32));before=stream.getvalue()
        self.assertFalse(collector.write_record(stream,{"large":"x"*100},32));self.assertEqual(stream.getvalue(),before)
    def test_redirect_handler_refuses_redirection(self):
        self.assertIsNone(collector.NoRedirect().redirect_request(None,None,302,"found",{},"http://localhost/other"))
    @unittest.skipUnless(hasattr(signal, "setitimer"), "Unix deadline mechanism required")
    def test_actual_socket_slow_drip_has_an_absolute_body_deadline(self):
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), collector.NoRedirect)
        previous_handler = signal.getsignal(signal.SIGALRM)
        with owned_http_server(slow_drip=True) as (target, state):
            begun = time.monotonic()
            with self.assertRaises(collector.DeadlineExceeded):
                collector.fetch(target, opener, begun + 0.15)
            self.assertLess(time.monotonic() - begun, 0.8)
            self.assertEqual(state["requests"], 1)
        self.assertEqual(signal.getsignal(signal.SIGALRM), previous_handler)
        self.assertEqual(signal.getitimer(signal.ITIMER_REAL), (0.0, 0.0))

    @unittest.skipUnless(hasattr(signal, "setitimer"), "Unix deadline mechanism required")
    def test_short_run_deadline_stops_before_visiting_all_slow_targets(self):
        with owned_http_server(slow_drip=True) as (target, state):
            targets = [dict(target, name=f"voter-{index}") for index in range(16)]
            output = io.BytesIO()
            begun = time.monotonic()
            self.assertEqual(collector.collect(targets, output, 1, 1024 * 1024), 0)
            self.assertLess(time.monotonic() - begun, 2.5)
            self.assertLessEqual(state["requests"], 2)
            records = [collector.json.loads(line) for line in output.getvalue().splitlines()]
            self.assertEqual(records[-1]["reason"], "duration")
            self.assertLessEqual(sum(record["kind"] == "sample" for record in records), 2)

    @unittest.skipUnless(hasattr(signal, "setitimer"), "Unix deadline mechanism required")
    def test_resolver_phase_uses_the_same_absolute_budget(self):
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), collector.NoRedirect)
        begun = time.monotonic()
        with mock.patch("socket.getaddrinfo", side_effect=lambda *args, **kwargs: time.sleep(5)):
            with self.assertRaises(collector.DeadlineExceeded):
                collector.fetch(node(), opener, begun + 0.1)
        self.assertLess(time.monotonic() - begun, 0.8)

    @unittest.skipUnless(hasattr(signal, "setitimer"), "Unix deadline mechanism required")
    def test_actual_http_unknown_source_and_missing_telemetry_remain_invalid(self):
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), collector.NoRedirect)
        with owned_http_server() as (target, _):
            target["expected_source"] = "1" * 40
            result = collector.fetch(target, opener, time.monotonic() + 0.5)
            self.assertFalse(result["valid"])
            self.assertIn("source_identity_unproven", result["limitations"])
        missing = BODY.replace('plurx_cluster_timing_epoch_info{epoch="000000000000abcd"} 1', '')
        with owned_http_server(body=missing) as (target, _):
            result = collector.fetch(target, opener, time.monotonic() + 0.5)
            self.assertFalse(result["valid"])
            self.assertIn("timing_instrumentation_unavailable", result["limitations"])

    @unittest.skipUnless(hasattr(signal, "setitimer"), "Unix deadline mechanism required")
    def test_existing_alarm_is_refused_without_stealing_its_timer(self):
        signal.setitimer(signal.ITIMER_REAL, 5)
        try:
            with self.assertRaisesRegex(RuntimeError, "exclusive ITIMER_REAL"):
                collector.require_deadline_support()
            self.assertGreater(signal.getitimer(signal.ITIMER_REAL)[0], 4)
        finally:
            signal.setitimer(signal.ITIMER_REAL, 0)

    def test_unsupported_deadline_context_is_explicitly_refused(self):
        with mock.patch.object(collector.threading, "current_thread", return_value=object()):
            with self.assertRaisesRegex(RuntimeError, "Unix ITIMER_REAL"):
                collector.require_deadline_support()

if __name__ == "__main__":unittest.main()
