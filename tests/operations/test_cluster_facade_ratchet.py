"""The cluster-facade census ceiling may only shrink.

`crates/plurxd/src/cluster/census.rs` pins, per production file and class,
every place feature code reaches around `state.cluster` to the raw
membership, replication and serving handles (docs/cluster/
CLUSTER-FACADE-BUILD.md section 4.1). Its `CEILING` is the table's sum, and
the Rust test keeps the two equal. A Rust test cannot see history, so a
change that adds a reach-around and raises its pin and the ceiling together
would pass there. This test reads `CEILING` at the merge base and at HEAD and
fails if HEAD is larger. A base without the census has no baseline.
"""

from __future__ import annotations

import os
from pathlib import Path
import re
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[2]
CENSUS = "crates/plurxd/src/cluster/census.rs"
CEILING = re.compile(r"^const CEILING: usize = (\d+);$", re.MULTILINE)


def ceiling(source: str) -> int:
    found = CEILING.findall(source)
    if len(found) != 1:
        raise ValueError(f"{CENSUS} must declare exactly one `const CEILING: usize = N;`")
    return int(found[0])


def raised(base: int | None, head: int) -> str | None:
    """Why HEAD's ceiling is refused, or None when it may land."""
    if base is not None and head > base:
        return (
            f"{CENSUS}: CEILING rose from {base} to {head}. The census only "
            "shrinks: ask `state.cluster` instead of adding a reach-around."
        )
    return None


def _git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=ROOT, check=True, capture_output=True, text=True
    ).stdout


def resolve_base() -> str | None:
    comparison = (
        os.environ.get("PLURX_VALIDATION_BASE")
        or os.environ.get("PLURX_VALIDATION_MERGE_TARGET")
        or "origin/main"
    )
    try:
        return _git("merge-base", "HEAD", comparison).strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None


def base_ceiling(base: str | None) -> int | None:
    if base is None:
        return None
    try:
        return ceiling(_git("show", f"{base}:{CENSUS}"))
    except subprocess.CalledProcessError:
        return None


class ClusterFacadeRatchetTest(unittest.TestCase):
    def test_the_ceiling_never_rises_against_the_merge_base(self) -> None:
        head = ceiling((ROOT / CENSUS).read_text(encoding="utf-8"))
        self.assertIsNone(raised(base_ceiling(resolve_base()), head))

    def test_a_raised_ceiling_is_refused_and_a_lowered_one_lands(self) -> None:
        self.assertIsNotNone(raised(500, 501))
        self.assertIsNone(raised(500, 500))
        self.assertIsNone(raised(500, 470))
        self.assertIsNone(raised(None, 501), "a base without the census has no baseline")

    def test_the_ceiling_is_read_from_its_one_declaration(self) -> None:
        self.assertEqual(ceiling("const CEILING: usize = 42;\n"), 42)
        with self.assertRaises(ValueError):
            ceiling("const CEILING: usize = 1;\nconst CEILING: usize = 2;\n")
        with self.assertRaises(ValueError):
            ceiling("// no ceiling here\n")


if __name__ == "__main__":
    unittest.main()
