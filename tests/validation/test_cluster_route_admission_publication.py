"""Both request-gate slots must be published exactly once per refresh.

`cluster_capacity_gate` consults the local maintenance flag and then the local
serving role on every request, and answers 503 for `true` / `Fenced`. Each slot
is refreshed once per heartbeat behind an awaited database read, so publishing
an interim closed value is a live outage window on a healthy node — once every
ten seconds, for as long as the read takes.

Unit tests on the publication guards cannot see that: a guard can be entirely
correct while its refresh publishes around it, which is the shape both defects
had. These contracts pin the call sites. Every assertion is positive as well as
negative — a check that only forbids a spelling passes vacuously the moment the
thing it names is renamed.
"""

from __future__ import annotations

import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
MEMBERSHIP = ROOT / "crates/plurx-core/src/cluster/membership.rs"
GATE = ROOT / "crates/plurxd/src/http/mod.rs"

# (function, slot field, guard type, committed value in the success path)
REFRESHES = (
    (
        "async fn refresh_local_route_admission(",
        "local_serving_role",
        "LocalServingRolePublication",
    ),
    (
        "async fn refresh_local_maintenance(",
        "local_maintenance",
        "LocalMaintenancePublication",
    ),
)


def strip_rust(source: str) -> str:
    """Blank out line comments and string literals, preserving offsets.

    Brace counting has to ignore braces inside `"..."` and `// ...`; blanking
    rather than deleting keeps every index the caller already holds valid.
    """
    out = list(source)
    index = 0
    length = len(source)
    while index < length:
        char = source[index]
        if char == "/" and source.startswith("//", index):
            while index < length and source[index] != "\n":
                out[index] = " "
                index += 1
        elif char == '"':
            out[index] = " "
            index += 1
            while index < length:
                if source[index] == "\\":
                    out[index] = " "
                    if index + 1 < length:
                        out[index + 1] = " "
                    index += 2
                    continue
                if source[index] == '"':
                    out[index] = " "
                    index += 1
                    break
                out[index] = " "
                index += 1
        else:
            index += 1
    return "".join(out)


def function_body(source: str, signature: str) -> str:
    """Return one function body from `source`, braces balanced."""
    if source.count(signature) != 1:
        raise AssertionError(f"{signature!r} must appear exactly once")
    masked = strip_rust(source)
    start = source.index(signature)
    opening = masked.index("{", masked.index(")", start))
    depth = 0
    for index in range(opening, len(masked)):
        if masked[index] == "{":
            depth += 1
        elif masked[index] == "}":
            depth -= 1
            if depth == 0:
                return source[opening : index + 1]
    raise AssertionError(f"unbalanced braces in {signature!r}")


class RequestGatePublicationTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.source = MEMBERSHIP.read_text(encoding="utf-8")

    def test_each_refresh_publishes_its_slot_exactly_once(self) -> None:
        for signature, slot, guard in REFRESHES:
            with self.subTest(refresh=signature):
                body = function_body(self.source, signature)
                # Positive anchors first: without these the negative
                # assertions below pass vacuously after a rename.
                self.assertIn(
                    slot,
                    body,
                    f"{signature} must still name {slot}; if the field was "
                    f"renamed, rename it here too rather than losing the check",
                )
                self.assertIn(f"{guard}::new(", body)
                self.assertEqual(
                    len(re.findall(r"\bpublication\.commit\(", body)),
                    1,
                    f"{signature} must publish once, at the end; every extra "
                    f"publication is a window in which the request gate "
                    f"answers 503 on a healthy node",
                )

    def test_no_refresh_writes_its_slot_outside_the_guard(self) -> None:
        for signature, slot, _ in REFRESHES:
            with self.subTest(refresh=signature):
                body = function_body(self.source, signature)
                # Tolerates whitespace, a borrow bound to a local, and a
                # method call written with a space before the paren.
                direct = re.findall(
                    rf"{re.escape(slot)}\s*(?:\)\s*)?[\s.]*\.?\s*store\s*\(",
                    body,
                )
                self.assertEqual(
                    direct,
                    [],
                    f"{signature} must not write {slot} directly — "
                    f"publication goes through its guard, whose Drop is what "
                    f"fails closed",
                )
                self.assertNotIn(
                    ".store(",
                    body,
                    f"{signature} must contain no atomic store at all; the "
                    f"only publication is the guard commit",
                )

    def test_nothing_in_the_module_writes_either_slot_by_name(self) -> None:
        """A helper called from the refresh is the same defect, one frame down.

        The guards publish through their own `self.slot`, so no site anywhere
        in the module should name either field and store to it. This is what
        stops the fix being undone by extracting the store into a helper.
        """
        for _, slot, _ in REFRESHES:
            with self.subTest(slot=slot):
                self.assertIn(slot, self.source)
                self.assertEqual(
                    re.findall(
                        rf"{re.escape(slot)}\s*(?:\)\s*)?[\s.]*\.?\s*store\s*\(",
                        strip_rust(self.source),
                    ),
                    [],
                    f"{slot} is written outside its publication guard; the "
                    f"guard exists so that every write is the single "
                    f"end-of-refresh commit",
                )

    def test_each_guard_fails_closed_when_dropped_uncommitted(self) -> None:
        for closed, guard in (
            ("LocalServingRole::Fenced.encoded()", "LocalServingRolePublication"),
            ("true", "LocalMaintenancePublication"),
        ):
            with self.subTest(guard=guard):
                body = function_body(self.source, f"impl Drop for {guard}<'_> {{")
                self.assertIn("if !self.committed", body)
                self.assertIn(closed, body)

    def test_the_route_gate_still_answers_503_for_a_closed_slot(self) -> None:
        gate = GATE.read_text(encoding="utf-8")
        body = function_body(gate, "async fn cluster_capacity_gate(")
        self.assertIn("local_maintenance_active()", body)
        self.assertIn("LocalServingRole::Fenced", body)
        self.assertIn("StatusCode::SERVICE_UNAVAILABLE", body)
        self.assertIn('"/healthz" | "/metrics"', body)


if __name__ == "__main__":
    unittest.main()
