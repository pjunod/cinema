"""The local serving role must be published exactly once per refresh.

`cluster_capacity_gate` loads `local_serving_role` on every request and answers
503 for `Fenced`, so an interim publication inside
`refresh_local_route_admission` is a live outage window on a healthy node, once
per heartbeat. A unit test on the publication guard cannot see that: the guard
can be correct while the refresh publishes around it. This pins the call site.
"""

from __future__ import annotations

import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
MEMBERSHIP = ROOT / "crates/plurx-core/src/cluster/membership.rs"
REFRESH = "async fn refresh_local_route_admission("


def refresh_body(source: str) -> str:
    """Return the body of `refresh_local_route_admission`, braces balanced."""
    start = source.index(REFRESH)
    opening = source.index("{", source.index(")", start))
    depth = 0
    for index in range(opening, len(source)):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                return source[opening : index + 1]
    raise AssertionError("unbalanced braces in refresh_local_route_admission")


class ClusterRouteAdmissionPublicationTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.source = MEMBERSHIP.read_text(encoding="utf-8")
        cls.body = refresh_body(cls.source)

    def test_the_refresh_publishes_the_serving_role_exactly_once(self) -> None:
        commits = re.findall(r"\bpublication\.commit\(", self.body)
        self.assertEqual(
            len(commits),
            1,
            "refresh_local_route_admission must publish the serving role once, "
            "at the end; every extra publication is a window in which the "
            "route gate answers 503 on a healthy node",
        )

    def test_the_refresh_never_stores_the_serving_role_directly(self) -> None:
        direct = re.findall(r"local_serving_role\s*\n?\s*\.store\(", self.body)
        self.assertEqual(
            direct,
            [],
            "refresh_local_route_admission must not touch local_serving_role "
            "directly — publication goes through LocalServingRolePublication, "
            "whose Drop is what fails closed",
        )

    def test_the_guard_fails_closed_when_it_is_dropped_uncommitted(self) -> None:
        guard = self.source[self.source.index("impl Drop for LocalServingRolePublication") :]
        guard = guard[: guard.index("\n}\n") + 3]
        self.assertIn("if !self.committed", guard)
        self.assertIn("LocalServingRole::Fenced.encoded()", guard)

    def test_the_route_gate_still_answers_503_for_a_fenced_node(self) -> None:
        gate = (ROOT / "crates/plurxd/src/http/mod.rs").read_text(encoding="utf-8")
        body = gate[gate.index("async fn cluster_capacity_gate(") :]
        body = body[: body.index("\nfn ")]
        self.assertIn("LocalServingRole::Fenced", body)
        self.assertIn("StatusCode::SERVICE_UNAVAILABLE", body)
        self.assertIn('"/healthz" | "/metrics"', body)


if __name__ == "__main__":
    unittest.main()
