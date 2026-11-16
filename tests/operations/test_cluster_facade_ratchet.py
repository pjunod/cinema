"""The cluster-facade census may only tighten.

`crates/plurxd/src/cluster/census.rs` pins, per production file and class,
every place feature code reaches around `state.cluster` to the raw
membership, replication and serving handles (docs/cluster/
CLUSTER-FACADE-BUILD.md section 4.1). The Rust test keeps the pins equal to
the source; it cannot see history, so a change that adds a reach-around and
raises its pin in the same commit passes there. This test reads the census
at the merge base and at HEAD and refuses anything that loosens it:

* a `CEILING` above the base's;
* a (file, class) pin that is new, or above its base value — moving a site
  from one file to another is a new reach-around in the second file;
* an `EXEMPT` path the base did not exempt;
* a name dropped from `TYPES`, `ROLE` or `ROSTER`, or one added to
  `NOT_CLUSTER`, either of which stops counting a spelling.

The base is `PLURX_VALIDATION_BASE` (scripts/paperwork sets it to the pull
request's base in the fast lane's paperwork job), else the merge base with
`PLURX_VALIDATION_MERGE_TARGET` or `origin/main`. Under CI (`CI` or
`GITHUB_ACTIONS` set), or when a base was named, a base that cannot be
resolved fails; a local checkout without one skips, saying so. A base
without the census file (the pull request that adds it) has no baseline.
"""

from __future__ import annotations

from dataclasses import dataclass
import os
from pathlib import Path
import re
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[2]
CENSUS = "crates/plurxd/src/cluster/census.rs"
CEILING = re.compile(r"^const CEILING: usize = (\d+);$", re.MULTILINE)
STRING = re.compile(r'"((?:[^"\\]|\\.)*)"')
PIN = re.compile(r'\(\s*"([^"]+)",\s*"([^"]+)",\s*(\d+)\s*,?\s*\)')


@dataclass
class Census:
    ceiling: int
    pins: dict[tuple[str, str], int]
    exempt: set[str]
    names: dict[str, set[str]]


def ceiling(source: str) -> int:
    found = CEILING.findall(source)
    if len(found) != 1:
        raise ValueError(f"{CENSUS} must declare exactly one `const CEILING: usize = N;`")
    return int(found[0])


def block(source: str, name: str, strict: bool = True) -> str:
    """The body of `const NAME: … = &[ … ];`; at a base, an older census
    without the list reads as an empty one."""
    match = re.search(rf"^const {name}: [^=]+= &\[(.*?)\];", source, re.MULTILINE | re.DOTALL)
    if match is None and not strict:
        return ""
    if match is None:
        raise ValueError(f"{CENSUS} declares no `const {name}`")
    return match.group(1)


def first_strings(body: str) -> set[str]:
    """The first string of each `(…)` tuple, or every string of a flat list."""
    tuples = re.findall(r"\(\s*(\"(?:[^\"\\]|\\.)*\")", body)
    if tuples:
        return {STRING.match(item).group(1) for item in tuples}
    return set(STRING.findall(body))


def parse(source: str, strict: bool = True) -> Census:
    return Census(
        ceiling=ceiling(source),
        pins={(f, c): int(n) for f, c, n in PIN.findall(block(source, "REACH_AROUNDS"))},
        exempt=first_strings(block(source, "EXEMPT")),
        names={
            name: first_strings(block(source, name, strict))
            for name in ("TYPES", "ROLE", "ROSTER", "NOT_CLUSTER")
        },
    )


def loosened(base: Census | None, head: Census) -> list[str]:
    """Every way HEAD's census is looser than the base's."""
    if base is None:
        return []
    found = []
    if head.ceiling > base.ceiling:
        found.append(f"CEILING rose from {base.ceiling} to {head.ceiling}")
    for key, count in sorted(head.pins.items()):
        before = base.pins.get(key)
        if before is None:
            found.append(f"{key[0]} [{key[1]}]: a new pin ({count})")
        elif count > before:
            found.append(f"{key[0]} [{key[1]}]: pin rose from {before} to {count}")
    for path in sorted(head.exempt - base.exempt):
        found.append(f"EXEMPT gained {path}")
    for name in ("TYPES", "ROLE", "ROSTER"):
        for dropped in sorted(base.names[name] - head.names[name]):
            found.append(f"{name} dropped {dropped}")
    for added in sorted(head.names["NOT_CLUSTER"] - base.names["NOT_CLUSTER"]):
        found.append(f"NOT_CLUSTER gained {added}")
    return found


def _git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=ROOT, check=True, capture_output=True, text=True
    ).stdout


class Unresolvable(Exception):
    pass


def resolve_base() -> str | None:
    """The base commit; None only on a local checkout that has none."""
    named = os.environ.get("PLURX_VALIDATION_BASE")
    strict = bool(named or os.environ.get("CI") or os.environ.get("GITHUB_ACTIONS"))
    try:
        if named:
            return _git("rev-parse", "--verify", f"{named}^{{commit}}").strip()
        target = os.environ.get("PLURX_VALIDATION_MERGE_TARGET") or "origin/main"
        return _git("merge-base", "HEAD", target).strip()
    except (subprocess.CalledProcessError, FileNotFoundError) as error:
        if strict:
            raise Unresolvable(f"the census base cannot be resolved: {error}") from error
        return None


def base_census(base: str) -> Census | None:
    try:
        source = _git("show", f"{base}:{CENSUS}")
    except subprocess.CalledProcessError:
        return None
    return parse(source, strict=False)


SOURCE = """
const TYPES: &[&str] = &[
    "MembershipManager",
    "ServingFence",
];
const ROLE: &[&str] = &["is_replicated"];
const ROSTER: &[&str] = &["activity_peers", "media_peers"];
const NOT_CLUSTER: &[(&str, &str)] = &[
    ("status", "every HTTP response has one"),
];
const EXEMPT: &[(&str, &str)] = &[
    ("cluster.rs", "the facade itself"),
    (
        "serving_fence.rs",
        "the fence",
    ),
];
const REACH_AROUNDS: &[(&str, &str, usize)] = &[
    ("a.rs", "membership", 3),
    ("b.rs", "roster", 2),
];
const CEILING: usize = 5;
"""


class ClusterFacadeRatchetTest(unittest.TestCase):
    def test_the_census_never_loosens_against_the_merge_base(self) -> None:
        head = parse((ROOT / CENSUS).read_text(encoding="utf-8"))
        base = resolve_base()
        if base is None:
            self.skipTest("no merge base in this checkout (no origin/main); CI names one")
        self.assertEqual(loosened(base_census(base), head), [])

    def test_the_census_source_parses(self) -> None:
        census = parse(SOURCE)
        self.assertEqual(census.ceiling, 5)
        self.assertEqual(census.pins, {("a.rs", "membership"): 3, ("b.rs", "roster"): 2})
        self.assertEqual(census.exempt, {"cluster.rs", "serving_fence.rs"})
        self.assertEqual(census.names["ROSTER"], {"activity_peers", "media_peers"})
        self.assertEqual(census.names["NOT_CLUSTER"], {"status"})
        head = parse((ROOT / CENSUS).read_text(encoding="utf-8"))
        self.assertEqual(sum(head.pins.values()), head.ceiling)
        self.assertIn("MembershipManager", head.names["TYPES"])

    def edited(self, old: str, new: str) -> Census:
        self.assertIn(old, SOURCE)
        return parse(SOURCE.replace(old, new))

    def test_a_raised_ceiling_is_refused_and_a_lowered_one_lands(self) -> None:
        base = parse(SOURCE)
        raised = self.edited("CEILING: usize = 5", "CEILING: usize = 6")
        self.assertEqual(loosened(base, raised), ["CEILING rose from 5 to 6"])
        lowered = self.edited('("b.rs", "roster", 2)', '("b.rs", "roster", 1)')
        self.assertEqual(loosened(base, lowered), [])
        self.assertEqual(loosened(None, raised), [], "a base without the census has no baseline")

    def test_a_moved_or_raised_pin_is_refused(self) -> None:
        base = parse(SOURCE)
        moved = self.edited('("b.rs", "roster", 2)', '("c.rs", "roster", 2)')
        self.assertEqual(loosened(base, moved), ["c.rs [roster]: a new pin (2)"])
        raised = self.edited('("a.rs", "membership", 3)', '("a.rs", "membership", 4)')
        self.assertIn("a.rs [membership]: pin rose from 3 to 4", loosened(base, raised))

    def test_a_grown_exemption_is_refused(self) -> None:
        base = parse(SOURCE)
        grown = self.edited('("cluster.rs", "the facade itself"),', '("cluster.rs", "the facade itself"),\n    ("live_tv.rs", "x"),')
        self.assertEqual(loosened(base, grown), ["EXEMPT gained live_tv.rs"])

    def test_a_shrunk_name_list_is_refused(self) -> None:
        base = parse(SOURCE)
        self.assertEqual(
            loosened(base, self.edited('"activity_peers", ', "")),
            ["ROSTER dropped activity_peers"],
        )
        self.assertEqual(
            loosened(base, self.edited('    "ServingFence",\n', "")),
            ["TYPES dropped ServingFence"],
        )
        self.assertEqual(
            loosened(base, self.edited('("status", "every HTTP response has one"),', '("status", "x"),\n    ("heartbeat", "x"),')),
            ["NOT_CLUSTER gained heartbeat"],
        )

    def test_an_unresolvable_base_fails_under_ci_and_skips_locally(self) -> None:
        saved = {key: os.environ.get(key) for key in ("PLURX_VALIDATION_BASE", "PLURX_VALIDATION_MERGE_TARGET", "CI", "GITHUB_ACTIONS")}
        try:
            for key in saved:
                os.environ.pop(key, None)
            os.environ["PLURX_VALIDATION_MERGE_TARGET"] = "refs/does/not/exist"
            self.assertIsNone(resolve_base(), "a local checkout without a base skips")
            os.environ["CI"] = "true"
            with self.assertRaises(Unresolvable):
                resolve_base()
            del os.environ["CI"]
            os.environ["PLURX_VALIDATION_BASE"] = "0" * 40
            with self.assertRaises(Unresolvable):
                resolve_base()
        finally:
            for key, value in saved.items():
                if value is None:
                    os.environ.pop(key, None)
                else:
                    os.environ[key] = value


if __name__ == "__main__":
    unittest.main()
