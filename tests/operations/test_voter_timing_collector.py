"""Read-only timing collector safety and observation-cohort contracts."""
import importlib.util
import io
from pathlib import Path
import unittest
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
if __name__ == "__main__":unittest.main()
