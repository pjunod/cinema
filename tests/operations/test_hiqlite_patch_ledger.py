from __future__ import annotations

import re
import tomllib
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
LEDGERS = (
    (ROOT / "vendor/hiqlite/PLURX-PATCH.md", 19),
    (ROOT / "vendor/hiqlite-wal/PLURX-PATCH.md", 3),
)

# `quick-xml` 0.39.4 is the release RUSTSEC-2026-0194 and RUSTSEC-2026-0195
# name. 0.41 is the first constraint that cannot resolve back onto it and the
# one `vendor/s3-simple` carries; upstream s3-simple 0.9 moved to 0.42.
QUICK_XML_FLOOR = (0, 41, 0)


def version_tuple(version: str) -> tuple[int, int, int]:
    """(major, minor, patch), padded, so "0.41" and "0.41.0" compare equal."""

    parts = [int(part) for part in re.findall(r"\d+", version)[:3]]
    parts += [0] * (3 - len(parts))
    return parts[0], parts[1], parts[2]


def table_rows(body: str) -> list[list[str]]:
    rows = []
    for line in body.splitlines():
        if not re.match(r"^\| \d+ \|", line):
            continue
        rows.append([field.strip() for field in line.strip("|").split("|")])
    return rows


def lock_packages(path: Path) -> list[dict]:
    return tomllib.loads(path.read_text(encoding="utf-8"))["package"]


def banned_crates() -> list[tuple[str, str | None]]:
    """(name, exact version or None) for every `[bans] deny` row in deny.toml.

    deny.toml is the single statement of dependency policy, so the fork's graph
    is judged against that file rather than against a second list that could
    drift away from it. An entry whose shape this parser does not model raises
    instead of being skipped: a ban that silently stops being checked is worse
    than no ban.
    """

    policy = tomllib.loads((ROOT / "deny.toml").read_text(encoding="utf-8"))
    bans = []
    for entry in policy["bans"]["deny"]:
        spec = entry["crate"]
        if ":" not in spec:
            bans.append((spec, None))
            continue
        name, requirement = spec.split(":", 1)
        if not requirement.startswith("="):
            raise ValueError(
                f"deny.toml bans {spec!r} with a requirement this contract "
                "cannot evaluate; teach it the new shape rather than letting "
                "the ban go unchecked against the fork graph"
            )
        bans.append((name, requirement[1:]))
    return bans


class HiqlitePatchLedgerCase(unittest.TestCase):
    def test_each_prose_patch_has_one_owned_ledger_row(self):
        for path, expected in LEDGERS:
            with self.subTest(ledger=path.parent.name):
                body = path.read_text(encoding="utf-8")
                rows = table_rows(body)
                prose = body.split(rows[-1][4], 1)[1].split("\nRemove this vendor", 1)[0]

                self.assertIn("**Owner:** Paul Junod (repository owner).", body)
                self.assertEqual([int(row[0]) for row in rows], list(range(1, expected + 1)))
                self.assertEqual(len(rows), expected)
                self.assertEqual(
                    len(re.findall(r"(?m)^- ", prose)),
                    expected,
                    "the table and the explanatory patch bullets must move together",
                )

                for number, _patch, kind, upstream, drop_condition in rows:
                    self.assertIn(kind, {"generic bug", "plurx policy", "dependency-only"})
                    self.assertTrue(drop_condition, f"patch {number} has no exit condition")
                    if kind == "generic bug":
                        self.assertTrue(
                            upstream == "pending M6" or upstream.startswith(("http://", "https://")),
                            f"generic patch {number} needs an honest pending marker or public URL",
                        )
                    elif kind == "dependency-only":
                        # A constraint patch is retired by an upstream release,
                        # never by plurx changing its mind, so "Never;" is the
                        # one answer it cannot honestly give.
                        self.assertEqual(upstream, "—")
                        self.assertFalse(
                            drop_condition.startswith("Never;"),
                            f"dependency-only patch {number} must name the upstream "
                            "release that retires the constraint",
                        )
                    else:
                        self.assertEqual(upstream, "—")
                        self.assertTrue(drop_condition.startswith("Never;"))

    def test_unused_s3_edge_is_gated_and_the_override_is_scoped_to_the_fork(self):
        hiqlite = tomllib.loads((ROOT / "vendor/hiqlite/Cargo.toml").read_text())
        cryptr = hiqlite["dependencies"]["cryptr"]
        self.assertEqual(cryptr, {"version": "0.10", "default-features": False})
        self.assertEqual(hiqlite["features"]["s3"], ["backup", "cryptr/s3"])

        # The fork advertises `backup`/`s3`, so the fix for the graph that
        # feature enables belongs in the manifest cargo honours when it is
        # enabled -- this one.
        self.assertEqual(
            hiqlite["patch"]["crates-io"]["s3-simple"],
            {"path": "../s3-simple"},
            "the vendored s3-simple override is what keeps the advertised "
            "backup/S3 graph off quick-xml 0.39.4 and aws-lc-sys 0.39.1",
        )

        workspace = tomllib.loads((ROOT / "Cargo.toml").read_text())
        self.assertIn("vendor/s3-simple", workspace["workspace"]["exclude"])
        # Not in the workspace root: plurx builds hiqlite without backup/s3, so
        # the workspace graph contains no s3-simple and the row would be an
        # unused patch. deny.toml's aws-lc-sys ban is the tripwire if that ever
        # changes; see the comment above [patch.crates-io] in Cargo.toml.
        self.assertNotIn("s3-simple", workspace["patch"]["crates-io"])
        self.assertNotIn(
            ("s3-simple", "0.8.0"),
            {
                (package["name"], package["version"])
                for package in lock_packages(ROOT / "Cargo.lock")
            },
            "the default workspace graph must stay free of s3-simple",
        )
        # It is absent from the workspace lock, so it cannot be in the
        # root-lock provenance table scripts/vendor-audit-lock rewrites.
        self.assertNotIn(
            '("s3-simple", "0.8.0")', (ROOT / "scripts/vendor-audit-lock").read_text()
        )


class ForkBackupGraphPolicyCase(unittest.TestCase):
    """The graph `backup`/`s3` actually enables, judged as a resolution.

    Gating `cryptr/s3` took S3 out of the configuration plurx builds. It did
    not take it out of the configuration hiqlite still advertises, and a
    feature nothing in this repository compiles is exactly the one whose graph
    nobody looks at. `vendor/hiqlite/Cargo.lock` is that resolution: a cargo
    lockfile pins every optional dependency of every feature, so these
    assertions read the backup/S3 edge whether or not anything selects it.
    """

    def setUp(self):
        self.packages = lock_packages(ROOT / "vendor/hiqlite/Cargo.lock")

    def test_the_vendored_override_is_what_resolves(self):
        s3_simple = [p for p in self.packages if p["name"] == "s3-simple"]
        self.assertEqual(len(s3_simple), 1, "exactly one s3-simple must resolve")
        self.assertNotIn(
            "source",
            s3_simple[0],
            "s3-simple resolved from the registry, which means "
            "vendor/hiqlite/Cargo.toml's [patch.crates-io] row stopped "
            "applying and the 0.8.0 constraints are back",
        )

    def test_no_resolved_quick_xml_is_advisory_affected(self):
        found = [p["version"] for p in self.packages if p["name"] == "quick-xml"]
        self.assertTrue(found, "the backup/S3 graph must still resolve a parser")
        for version in found:
            self.assertGreaterEqual(
                version_tuple(version),
                QUICK_XML_FLOOR,
                f"quick-xml {version} is at or below the release "
                "RUSTSEC-2026-0194 and RUSTSEC-2026-0195 name",
            )

    def test_the_enabled_graph_obeys_the_repository_ban_list(self):
        violations = []
        for name, exact in banned_crates():
            for package in self.packages:
                if package["name"] != name:
                    continue
                if exact is None or package["version"] == exact:
                    violations.append(f"{package['name']} {package['version']}")
        self.assertEqual(
            violations,
            [],
            "deny.toml forbids these crates, and `cargo deny` only ever sees "
            "the workspace graph -- enabling hiqlite's backup/S3 feature must "
            "not reintroduce one behind its back",
        )

    def test_the_vendored_manifest_states_the_constraints_the_lock_proves(self):
        s3_simple = tomllib.loads((ROOT / "vendor/s3-simple/Cargo.toml").read_text())
        dependencies = s3_simple["dependencies"]
        self.assertGreaterEqual(
            version_tuple(dependencies["quick-xml"]["version"]), QUICK_XML_FLOOR
        )
        for dropped in ("aws-lc-rs", "aws-lc-sys", "quinn", "quinn-proto"):
            self.assertNotIn(
                dropped,
                dependencies,
                f"{dropped} is unreferenced in s3-simple's sources and absent "
                "from upstream 0.9; re-declaring it pins a second crypto or "
                "QUIC stack into the backup/S3 graph for nothing",
            )


if __name__ == "__main__":
    unittest.main()
