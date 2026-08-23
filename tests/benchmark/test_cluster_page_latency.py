"""Page-phase artifact, percentile, redaction, and schema contract tests."""

from __future__ import annotations

import copy
import importlib.machinery
import importlib.util
import json
import os
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace


ROOT = Path(__file__).resolve().parents[2]
RUNNER_PATH = ROOT / "scripts" / "cluster-page-latency"
SCHEMA_PATH = ROOT / "benchmarks" / "cluster-page-latency.schema.json"
LOADER = importlib.machinery.SourceFileLoader("cluster_page_latency", str(RUNNER_PATH))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
assert SPEC is not None
LATENCY = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(LATENCY)


def sample(
    shell: int | None = 100,
    content: int | None = 400,
    settled: int | None = 700,
    failure: str | None = None,
) -> dict:
    return {
        "shell_us": shell,
        "content_us": content,
        "settled_us": settled,
        "endpoints": [
            {
                "route": "/api/v1/hubs",
                "start_us": 120,
                "end_us": 350,
                "status": 200,
            }
        ],
        "failure": failure,
    }


def artifact(samples: list[dict] | None = None) -> dict:
    raw = samples or [sample(), sample(200, 500, 900)]
    return LATENCY.build_artifact(
        build_sha="a" * 40,
        server_build="a" * 40,
        scenario="healthy",
        started_at_unix_ms=1_000,
        finished_at_unix_ms=2_000,
        runner={
            "hardware": "m6-pro",
            "storage_device": "nvme",
            "network_path": "lan-ethernet",
        },
        target_role="follower",
        voter_count=3,
        sample_count=len(raw),
        samples_by_route={"home": raw},
    )


def complete_budget_artifact(scenario: str = "healthy") -> dict:
    value = artifact([sample(100, 400_000, 900_000) for _ in range(30)])
    template = value["routes"][0]
    value["routes"] = []
    for route_name in LATENCY.ROUTES:
        route = copy.deepcopy(template)
        route["route"] = route_name
        value["routes"].append(route)
    value["scenario"] = scenario
    LATENCY.validate_artifact(value)
    return value


class ClusterPageLatencyTests(unittest.TestCase):
    def test_type7_percentiles_match_the_cluster_topology_rule(self) -> None:
        self.assertEqual(LATENCY.percentile_type7([4], 0.95), 4.0)
        self.assertEqual(LATENCY.percentile_type7([1, 2, 3, 4], 0.50), 2.5)
        self.assertAlmostEqual(LATENCY.percentile_type7([1, 2, 3, 4], 0.95), 3.85)
        with self.assertRaisesRegex(LATENCY.ArtifactError, "at least one"):
            LATENCY.percentile_type7([], 0.95)

    def test_api_routes_drop_hosts_queries_and_dynamic_ids(self) -> None:
        self.assertEqual(
            LATENCY.normalize_api_route(
                "https://192.0.2.10/api/v1/libraries/"
                "35af4e5e-b952-4cc0-b945-f75f27001751/items?limit=24&sort=added"
            ),
            "/api/v1/libraries/{library_id}/items",
        )
        self.assertEqual(
            LATENCY.normalize_api_route("https://server.invalid/api/v1/private/123"),
            "/api/v1/other",
        )
        self.assertIsNone(LATENCY.normalize_api_route("https://server.invalid/artwork/a.jpg"))

    def test_deployed_build_must_be_stamped_and_match_the_full_sha(self) -> None:
        LATENCY.verify_server_build(
            "v0.2.7-1000-gaaaaaaaa", "a" * 40, resolver=lambda _: "a" * 40
        )
        LATENCY.verify_server_build("aaaaaaaa", "a" * 40, resolver=lambda _: "a" * 40)
        LATENCY.verify_server_build(
            "v0.2.7", "a" * 40, resolver=lambda _: "a" * 40
        )
        with self.assertRaisesRegex(LATENCY.ArtifactError, "unstamped"):
            LATENCY.verify_server_build("unknown", "a" * 40)
        with self.assertRaisesRegex(LATENCY.ArtifactError, "does not match"):
            LATENCY.verify_server_build(
                "v0.2.7-1000-gbbbbbbbb", "a" * 40, resolver=lambda _: "b" * 40
            )
        with self.assertRaisesRegex(LATENCY.ArtifactError, "does not match"):
            LATENCY.verify_server_build(
                "v0.2.7", "a" * 40, resolver=lambda _: "b" * 40
            )
        for fabricated in (
            "not-a-stamp-gaaaaaaaa",
            "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-gaaaaaaaa",
            "v0.2.7-1000-gaaaaaaaa-dirty",
        ):
            with self.subTest(fabricated=fabricated):
                with self.assertRaisesRegex(LATENCY.ArtifactError, "unstamped"):
                    LATENCY.verify_server_build(fabricated, "a" * 40)

    def test_playwright_runtime_is_the_repository_pin(self) -> None:
        LATENCY.verify_playwright_version("1.62.0")
        with self.assertRaisesRegex(LATENCY.ArtifactError, "1.62.0"):
            LATENCY.verify_playwright_version("1.61.0")

    def test_neutral_warmup_finishes_before_sampling(self) -> None:
        runner_source = RUNNER_PATH.read_text(encoding="utf-8")
        warmup = runner_source.index("/#/search/__latency_warmup__")
        completed = runner_source.index('wait_for_selector("#main .pagehead"')
        first_identity = runner_source.index("initial_status =")
        self.assertLess(warmup, completed)
        self.assertLess(completed, first_identity)

    def test_complete_artifact_recomputes_raw_summaries(self) -> None:
        value = artifact()
        LATENCY.validate_artifact(value)
        summary = value["routes"][0]["summary"]
        self.assertEqual(summary["success_count"], 2)
        self.assertEqual(summary["failure_count"], 0)
        self.assertEqual(summary["content"]["p50_us"], 450.0)
        self.assertEqual(summary["settled"]["p95_us"], 890.0)

    def test_failed_sample_may_end_before_content_without_inventing_a_time(self) -> None:
        value = artifact([sample(120, None, None, "phase_timeout")])
        LATENCY.validate_artifact(value)
        summary = value["routes"][0]["summary"]
        self.assertEqual(summary["success_count"], 0)
        self.assertEqual(summary["failure_count"], 1)
        self.assertIsNone(summary["content"]["p95_us"])

    def test_phase_order_and_summary_drift_are_rejected(self) -> None:
        out_of_order = artifact()
        out_of_order["routes"][0]["samples"][0]["content_us"] = 800
        with self.assertRaisesRegex(LATENCY.ArtifactError, "out of order"):
            LATENCY.validate_artifact(out_of_order)

        drifted = artifact()
        drifted["routes"][0]["summary"]["settled"]["p95_us"] += 1
        with self.assertRaisesRegex(LATENCY.ArtifactError, "raw samples"):
            LATENCY.validate_artifact(drifted)

        count_drift = artifact()
        count_drift["routes"][0]["summary"]["shell"]["sample_count"] = 999
        with self.assertRaisesRegex(LATENCY.ArtifactError, "sample_count"):
            LATENCY.validate_artifact(count_drift)

        skipped_shell = artifact([sample(100, None, None, "phase_timeout")])
        skipped_shell["routes"][0]["samples"][0]["shell_us"] = None
        skipped_shell["routes"][0]["samples"][0]["content_us"] = 400
        with self.assertRaisesRegex(LATENCY.ArtifactError, "not a prefix"):
            LATENCY.validate_artifact(skipped_shell)

    def test_unknown_fields_routes_and_failure_text_are_rejected(self) -> None:
        unknown = artifact()
        unknown["surprise"] = True
        with self.assertRaisesRegex(LATENCY.ArtifactError, "keys differ"):
            LATENCY.validate_artifact(unknown)

        duplicate = artifact()
        duplicate["routes"].append(copy.deepcopy(duplicate["routes"][0]))
        with self.assertRaisesRegex(LATENCY.ArtifactError, "duplicated"):
            LATENCY.validate_artifact(duplicate)

        dynamic_failure = artifact()
        dynamic_failure["routes"][0]["samples"][0]["failure"] = "timeout from 10.0.0.4"
        with self.assertRaisesRegex(LATENCY.ArtifactError, "bounded code"):
            LATENCY.validate_artifact(dynamic_failure)

    def test_secrets_network_identity_and_raw_paths_are_rejected(self) -> None:
        sensitive_values = [
            "Bearer abcdefghijklmnopqrstuvwxyz",
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "aaaaaaaa.bbbbbbbb.cccccccc",
            "node 35af4e5e-b952-4cc0-b945-f75f27001751",
            "node 018f0f6d-3b5c-7a91-8def-123456789abc",
            "http://192.0.2.10:32400",
            "ftp://user:password@example.invalid/file",
            "node 2001:db8::1",
            "/srv/media/private/movie.mkv",
        ]
        for sensitive in sensitive_values:
            with self.subTest(sensitive=sensitive):
                with self.assertRaises(LATENCY.ArtifactError):
                    LATENCY._assert_redacted(sensitive)
                value = artifact()
                value["server_build"] = sensitive
                with self.assertRaises(LATENCY.ArtifactError):
                    LATENCY.validate_artifact(value)

        forbidden_key = artifact()
        forbidden_key["routes"][0]["samples"][0]["authorization"] = "redacted"
        with self.assertRaisesRegex(LATENCY.ArtifactError, "keys differ"):
            LATENCY.validate_artifact(forbidden_key)

    def test_storage_state_must_be_owner_only(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "state.json"
            path.write_text("{}", encoding="utf-8")
            os.chmod(path, 0o644)
            with self.assertRaisesRegex(LATENCY.ArtifactError, "owner-only"):
                LATENCY._storage_state(path)
            os.chmod(path, 0o600)
            self.assertEqual(LATENCY._storage_state(path), {})
            link = Path(directory) / "state-link.json"
            link.symlink_to(path)
            with self.assertRaisesRegex(LATENCY.ArtifactError, "symbolic link"):
                LATENCY._storage_state(link)
            self.assertTrue(LATENCY._paths_alias(path, link))

    def test_base_url_is_a_credential_free_origin(self) -> None:
        LATENCY._validate_base_url("https://plurx.example.test")
        for invalid in (
            "https://user:password@plurx.example.test",
            "https://plurx.example.test/path",
            "https://plurx.example.test?token=secret",
            "https://plurx.example.test/#fragment",
            "https://plurx.example.test?",
            "https://plurx.example.test#",
            "https://plurx.example.test:invalid",
            "http://[",
        ):
            with self.subTest(invalid=invalid):
                with self.assertRaises(LATENCY.ArtifactError):
                    LATENCY._validate_base_url(invalid)

    def test_resource_timings_are_body_complete_and_route_scoped(self) -> None:
        entries = [
            {
                "name": "https://plurx.example.test/api/v1/activity",
                "startTime": 101.0,
                "responseEnd": 102.0,
                "responseStatus": 500,
            },
            {
                "name": "https://plurx.example.test/api/v1/cluster/nodes",
                "startTime": 101.5,
                "responseEnd": 103.0,
                "responseStatus": 500,
            },
            {
                "name": "https://plurx.example.test/api/v1/hubs?warm=1",
                "startTime": 102.0,
                "responseEnd": 650.0,
                "responseStatus": 200,
            },
        ]
        endpoints, overflow = LATENCY._extract_endpoints(
            entries, {"/api/v1/activity"}, 100.0
        )
        self.assertFalse(overflow)
        self.assertEqual(endpoints, [
            {
                "route": "/api/v1/cluster/nodes",
                "start_us": 1_500,
                "end_us": 3_000,
                "status": 500,
            },
            {
                "route": "/api/v1/hubs",
                "start_us": 2_000,
                "end_us": 550_000,
                "status": 200,
            },
        ])
        runner_source = RUNNER_PATH.read_text(encoding="utf-8")
        self.assertLess(
            runner_source.index("performance.mark"),
            runner_source.index("element.click()"),
        )

    def test_phase_collection_uses_one_exact_generation(self) -> None:
        entries = [
            {"name": "plurx-page-phase:7:home:shell", "startTime": 100.1},
            {"name": "plurx-page-phase:8:home:shell", "startTime": 101.0},
            {"name": "plurx-page-phase:8:home:content", "startTime": 102.0},
            {"name": "plurx-page-phase:8:home:settled", "startTime": 103.0},
        ]
        self.assertEqual(
            LATENCY._extract_phases(entries, "home", "8", 100.0),
            {"shell": 1_000, "content": 2_000, "settled": 3_000},
        )

    def test_target_role_count_and_scenario_are_verified(self) -> None:
        status = {
            "local_node_id": "local",
            "nodes": [
                {"node_id": "local", "role": "voter", "is_leader": False, "reachable": True},
                {"node_id": "leader", "role": "voter", "is_leader": True, "reachable": True},
                {"node_id": "peer", "role": "voter", "is_leader": False, "reachable": True},
            ],
        }
        self.assertEqual(
            LATENCY._validate_target_status(status, "follower", 3, "healthy"),
            ("local", ("leader", "local", "peer")),
        )
        with self.assertRaisesRegex(LATENCY.ArtifactError, "role"):
            LATENCY._validate_target_status(status, "leader", 3, "healthy")
        status["nodes"][2]["reachable"] = False
        LATENCY._validate_target_status(status, "follower", 3, "voter_unavailable")
        with self.assertRaisesRegex(LATENCY.ArtifactError, "every voter"):
            LATENCY._validate_target_status(status, "follower", 3, "healthy")
        malformed = copy.deepcopy(status)
        del malformed["nodes"][0]["is_leader"]
        with self.assertRaisesRegex(LATENCY.ArtifactError, "boolean"):
            LATENCY._validate_target_status(
                malformed, "follower", 3, "voter_unavailable"
            )

    def test_raft_observation_requires_a_valid_stable_sample(self) -> None:
        metrics = """
plurx_raft_metric_sample_valid{source="local"} 1
plurx_raft_leader_changes_total 4
plurx_raft_current_term 7
"""
        self.assertEqual(LATENCY._parse_raft_observation(metrics), (7, 4))
        with self.assertRaisesRegex(LATENCY.ArtifactError, "not valid"):
            LATENCY._parse_raft_observation(metrics.replace('local"} 1', 'local"} 0'))
        with self.assertRaisesRegex(LATENCY.ArtifactError, "incomplete"):
            LATENCY._parse_raft_observation(
                metrics.replace("plurx_raft_current_term 7\n", "")
            )

    def test_topology_probe_uses_the_authenticated_page_api(self) -> None:
        class Page:
            def evaluate(self, expression: str) -> dict:
                self.expression = expression
                return {"local_node_id": "local", "nodes": []}

        page = Page()
        LATENCY._authenticated_cluster_status(page)
        self.assertEqual(page.expression, "async () => api('/cluster/nodes')")

    def test_target_stability_rejects_membership_and_raft_changes(self) -> None:
        status = {
            "local_node_id": "local",
            "nodes": [
                {"node_id": "local", "role": "voter", "is_leader": False, "reachable": True},
                {"node_id": "leader", "role": "voter", "is_leader": True, "reachable": True},
                {"node_id": "peer", "role": "voter", "is_leader": False, "reachable": True},
            ],
        }
        metrics = """
plurx_raft_metric_sample_valid{source="local"} 1
plurx_raft_leader_changes_total 4
plurx_raft_current_term 7
"""

        class Page:
            def evaluate(self, expression: str) -> object:
                if "api('/cluster/nodes')" in expression:
                    return status
                return metrics

        args = SimpleNamespace(
            target_role="follower", voter_count=3, scenario="healthy"
        )
        expected_membership = ("local", ("leader", "local", "peer"))
        LATENCY._assert_target_stable(Page(), args, expected_membership, (7, 4))
        status["nodes"][2]["node_id"] = "replacement"
        with self.assertRaisesRegex(LATENCY.ArtifactError, "membership changed"):
            LATENCY._assert_target_stable(Page(), args, expected_membership, (7, 4))
        status["nodes"][2]["node_id"] = "peer"
        with self.assertRaisesRegex(LATENCY.ArtifactError, "term or leader"):
            LATENCY._assert_target_stable(Page(), args, expected_membership, (8, 4))

    def test_budget_evaluation_is_scenario_aware_and_needs_thirty_samples(self) -> None:
        healthy = complete_budget_artifact()
        self.assertEqual(LATENCY.budget_violations(healthy), [])
        healthy["routes"][0]["summary"]["settled"]["p95_us"] = 1_000_001
        self.assertRegex(" ".join(LATENCY.budget_violations(healthy)), "settled")
        short = artifact()
        self.assertRegex(" ".join(LATENCY.budget_violations(short)), "30 samples")
        self.assertRegex(" ".join(LATENCY.budget_violations(short)), "all four")

        delayed = complete_budget_artifact("delayed_home_optional")
        self.assertRegex(" ".join(LATENCY.budget_violations(delayed)), "does not prove")
        home = next(route for route in delayed["routes"] if route["route"] == "home")
        for delayed_sample in home["samples"]:
            delayed_sample["settled_us"] = 800_000
            delayed_sample["endpoints"].append({
                "route": "/api/v1/coming-soon",
                "start_us": 100_000,
                "end_us": 400_000,
                "status": 200,
            })
        home["summary"] = LATENCY.summarize_samples(home["samples"])
        LATENCY.validate_artifact(delayed)
        self.assertEqual(LATENCY.budget_violations(delayed), [])

        mostly_fast = copy.deepcopy(delayed)
        mostly_fast_home = next(
            route for route in mostly_fast["routes"] if route["route"] == "home"
        )
        for fast_sample in mostly_fast_home["samples"][:27]:
            fast_sample["settled_us"] = fast_sample["content_us"] + 1_000
            fast_sample["endpoints"][-1]["end_us"] = (
                fast_sample["endpoints"][-1]["start_us"] + 1_000
            )
        mostly_fast_home["summary"] = LATENCY.summarize_samples(
            mostly_fast_home["samples"]
        )
        LATENCY.validate_artifact(mostly_fast)
        self.assertRegex(
            " ".join(LATENCY.budget_violations(mostly_fast)),
            "does not prove",
        )

        system_delayed = complete_budget_artifact("delayed_system_optional")
        system = next(
            route
            for route in system_delayed["routes"]
            if route["route"] == "settings:system"
        )
        for delayed_sample in system["samples"]:
            delayed_sample["settled_us"] = 800_000
            delayed_sample["endpoints"].append({
                "route": "/api/v1/system/logs",
                "start_us": 100_000,
                "end_us": 400_000,
                "status": 200,
            })
        system["summary"] = LATENCY.summarize_samples(system["samples"])
        LATENCY.validate_artifact(system_delayed)
        self.assertEqual(LATENCY.budget_violations(system_delayed), [])
        system["samples"][0]["endpoints"].pop()
        self.assertRegex(
            " ".join(LATENCY.budget_violations(system_delayed)),
            "does not prove",
        )

    def test_hostile_types_and_unbounded_timings_raise_artifact_errors(self) -> None:
        bad_route = artifact()
        bad_route["routes"][0]["samples"][0]["endpoints"][0]["route"] = []
        with self.assertRaises(LATENCY.ArtifactError):
            LATENCY.validate_artifact(bad_route)
        huge = artifact()
        huge["routes"][0]["samples"][0]["shell_us"] = 10**400
        with self.assertRaisesRegex(LATENCY.ArtifactError, "maximum"):
            LATENCY.validate_artifact(huge)

    def test_checked_schema_is_closed_and_matches_the_validator_vocabulary(self) -> None:
        schema = json.loads(SCHEMA_PATH.read_text(encoding="utf-8"))
        self.assertEqual(
            schema["$schema"], "https://json-schema.org/draft/2020-12/schema"
        )
        self.assertFalse(schema["additionalProperties"])
        self.assertEqual(set(schema["required"]), set(artifact()))
        self.assertEqual(
            set(schema["$defs"]["route"]["properties"]["route"]["enum"]),
            set(LATENCY.ROUTES),
        )
        self.assertEqual(set(schema["properties"]["scenario"]["enum"]), set(LATENCY.SCENARIOS))
        self.assertEqual(
            set(schema["$defs"]["sample"]["properties"]["failure"]["enum"]),
            {None, *LATENCY.FAILURES},
        )
        endpoint_enum = set(
            schema["$defs"]["endpoint"]["properties"]["route"]["enum"]
        )
        self.assertEqual(
            endpoint_enum,
            {
                *LATENCY.API_ROUTES,
                "/api/v1/libraries/{library_id}/items",
                "/api/v1/other",
            },
        )


if __name__ == "__main__":
    unittest.main()
