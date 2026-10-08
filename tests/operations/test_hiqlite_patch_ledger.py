from __future__ import annotations

import hashlib
import os
import re
import tomllib
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
LEDGERS = (
    (ROOT / "vendor/hiqlite/PLURX-PATCH.md", 25),
    (ROOT / "vendor/hiqlite-wal/PLURX-PATCH.md", 6),
)

NUMBER_WORDS = {6: "six", 25: "twenty-five"}

# A path a row bullet names, relative to the vendored crate root, written in
# backticks: `src/writer.rs`, `Cargo.toml`. A repository path such as
# `crates/plurx-core/...` does not match, because the backtick must open it.
CRATE_PATH = re.compile(r"`((?:src/[A-Za-z0-9_/]+\.rs)|Cargo\.toml|Cargo\.lock)`")

# K-06 clock-skew identifiers. A source whose path or text carries one is part
# of the staged-startup / membership-admission patch, so a K-06 row must name it.
K06_IDENTIFIERS = re.compile(
    r"clock_observation|ClockObservation|start_staged|PartialStartupWriter|"
    r"membership_admission|MembershipAdmission|staged_startup|StartupStorageOwner|"
    r"ReductionFenceReference|startup_listeners"
)

# The disclosed gap: Plurx-changed sources that no ledger row owns yet. Each
# PLURX-FILES.toml `unledgered` table must stay within this set, so the gap can
# shrink by adding a row but cannot grow without a reviewed edit here.
KNOWN_UNLEDGERED = {
    "hiqlite": set(),
    "hiqlite-wal": set(),
}

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


def row_bullets(body: str) -> list[str]:
    """Each table row's explanatory bullet, in table order.

    A bullet is its `- ` line plus the two-space continuation lines under it,
    taken from the prose between the table and the removal paragraph.
    """

    rows = table_rows(body)
    prose = body.split(rows[-1][4], 1)[1].split("\nRemove this vendor", 1)[0]
    bullets: list[list[str]] = []
    for line in prose.splitlines():
        if line.startswith("- "):
            bullets.append([line])
        elif bullets and line.startswith("  ") and bullets[-1] is not None:
            bullets[-1].append(line)
        elif bullets:
            bullets.append(None)  # paragraph break closes the open bullet
    return [" ".join(bullet) for bullet in bullets if bullet is not None]


def named_paths(bullet: str) -> set[str]:
    return set(CRATE_PATH.findall(bullet))


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

    def test_the_removal_condition_can_be_met(self):
        """A `plurx policy` row's drop condition is `Never`, so a removal
        sentence that waits for an upstream release to contain every patch can
        never fire. Where a ledger has policy rows, the removal paragraph must
        name exactly the rows an upstream release retires and exactly the
        policy rows that need an owner decision instead, and the intro must
        state the real patch count.
        """

        for path, expected in LEDGERS:
            with self.subTest(ledger=path.parent.name):
                body = path.read_text(encoding="utf-8")
                rows = table_rows(body)
                intro = body.split("**Owner:**", 1)[0]
                self.assertIn(
                    f"carries {NUMBER_WORDS[expected]} ",
                    intro,
                    "the intro's patch count must match the ledger table",
                )
                removal = body.split("\nRemove this vendor", 1)[1].split("\n\n", 1)[0]
                self.assertNotRegex(
                    removal,
                    r"contains\s+all\s+\w+\s+patches",
                    "an upstream release cannot contain a Plurx policy row",
                )
                policy = {int(row[0]) for row in rows if row[2] == "plurx policy"}
                if not policy:
                    continue
                retirable = {int(row[0]) for row in rows} - policy
                named = [
                    {int(number) for number in re.findall(r"\d+", listed)}
                    for listed in re.findall(r"\(rows? ([\d, and]+)", removal)
                ]
                self.assertEqual(
                    named,
                    [retirable, policy],
                    "the removal paragraph must list the upstream-retirable rows, "
                    "then the policy rows, exactly as the table classifies them",
                )

    def test_every_row_names_an_existing_source_file(self):
        for path, _expected in LEDGERS:
            crate = path.parent
            with self.subTest(ledger=crate.name):
                body = path.read_text(encoding="utf-8")
                bullets = row_bullets(body)
                self.assertEqual(len(bullets), len(table_rows(body)))
                for number, bullet in enumerate(bullets, start=1):
                    named = named_paths(bullet)
                    self.assertTrue(
                        named,
                        f"row {number}'s bullet names no `src/...` or Cargo file, "
                        "so nothing ties it to the source it patches",
                    )
                    missing = sorted(p for p in named if not (crate / p).is_file())
                    self.assertEqual(missing, [], f"row {number} names files that do not exist")

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


class ForkSourceManifestCase(unittest.TestCase):
    """Every vendored Rust source is tied to the ledger row that changes it.

    Upstream sources are not available offline, so each fork carries
    `PLURX-FILES.toml`, which classified every `src/**/*.rs` against the
    crates.io 0.14.0 package on 2026-10-04 (whitespace-insensitively):

    - `patched` files carry Plurx changes and must each be named by at least
      one PLURX-PATCH.md row bullet;
    - `unledgered` files carry Plurx changes no row owns yet; they are a
      disclosed gap bounded by KNOWN_UNLEDGERED and named in the ledger text;
    - `formatting` (rustfmt/import order only) and `upstream` (identical)
      files are pinned by SHA-256, so an edit to one fails here unless the
      same change also edits its pin.

    A new source file fails until it is classified. Separately, any source
    carrying a K-06 identifier must be named by a K-06 row, whatever the
    manifest says.

    Limits, stated plainly: re-pinning passes. A change that edits a pinned
    file and writes the new digest into PLURX-FILES.toml is green here, so a
    reviewer must read every PLURX-FILES.toml diff: a changed digest under
    `formatting` or `upstream` is an unledgered patch unless the file really
    is still identical (or formatting-only) against upstream, which
    `test_manifest_matches_upstream` can re-derive. And the tie is per file,
    not per hunk. A new hunk in
    a file that is already `patched` passes as long as some row names that
    file, even if no row describes the hunk; review of the row prose is still
    what catches that. The manifest's classification was made once against
    upstream; with `PLURX_HIQLITE_UPSTREAM_DIR` pointing at a directory that
    holds the unpacked `hiqlite-0.14.0/` and `hiqlite-wal-0.14.0/` crates,
    `test_manifest_matches_upstream` re-derives it exactly.
    """

    def manifest(self, crate: Path) -> dict:
        return tomllib.loads((crate / "PLURX-FILES.toml").read_text(encoding="utf-8"))

    def test_every_source_is_classified_and_patched_files_are_named_by_rows(self):
        for ledger, _expected in LEDGERS:
            crate = ledger.parent
            with self.subTest(fork=crate.name):
                manifest = self.manifest(crate)
                patched = set(manifest["patched"])
                unledgered = set(manifest["unledgered"])
                pinned = {**manifest["formatting"], **manifest["upstream"]}
                groups = [patched, unledgered, set(manifest["formatting"]), set(manifest["upstream"])]
                self.assertEqual(
                    sum(len(group) for group in groups),
                    len(set().union(*groups)),
                    "a source is classified twice",
                )
                sources = {str(p.relative_to(crate)) for p in (crate / "src").rglob("*.rs")}
                self.assertEqual(
                    sorted(sources - set().union(*groups)),
                    [],
                    "an unclassified source: add it to PLURX-FILES.toml, and if it "
                    "carries a Plurx change, name it in the row that owns it",
                )
                self.assertEqual(sorted(set().union(*groups) - sources), [], "stale manifest entry")

                for name, digest in pinned.items():
                    actual = hashlib.sha256((crate / name).read_bytes()).hexdigest()
                    self.assertEqual(
                        actual,
                        digest,
                        f"{name} is pinned as unpatched but changed (now {actual}); "
                        "move it to `patched` and name it in the row that owns the change "
                        "(re-pinning the digest instead would hide a patch from the ledger)",
                    )

                body = ledger.read_text(encoding="utf-8")
                named = set().union(*(named_paths(b) for b in row_bullets(body)))
                self.assertEqual(
                    sorted(patched - named),
                    [],
                    "patched sources that no PLURX-PATCH.md row bullet names",
                )
                self.assertLessEqual(unledgered, KNOWN_UNLEDGERED[crate.name])
                self.assertEqual(
                    sorted(unledgered & named),
                    [],
                    "a row now owns this file; drop it from `unledgered`",
                )
                for name in unledgered:
                    self.assertIn(f"`{name}`", body, f"the ledger must disclose {name}")

    def test_k06_sources_are_named_by_a_k06_row(self):
        for ledger, _expected in LEDGERS:
            crate = ledger.parent
            with self.subTest(fork=crate.name):
                body = ledger.read_text(encoding="utf-8")
                k06_named = set().union(
                    *(
                        named_paths(bullet)
                        for row, bullet in zip(table_rows(body), row_bullets(body))
                        if row[1].startswith("K-06")
                    )
                )
                carrying = sorted(
                    str(p.relative_to(crate))
                    for p in (crate / "src").rglob("*.rs")
                    if K06_IDENTIFIERS.search(str(p.relative_to(crate)))
                    or K06_IDENTIFIERS.search(p.read_text(encoding="utf-8"))
                )
                self.assertTrue(carrying, "the K-06 identifiers no longer match any source")
                self.assertEqual(
                    [name for name in carrying if name not in k06_named],
                    [],
                    "sources carrying K-06 identifiers that no K-06 row names",
                )

    def test_manifest_matches_upstream(self):
        upstream_root = os.environ.get("PLURX_HIQLITE_UPSTREAM_DIR")
        if not upstream_root:
            self.skipTest("set PLURX_HIQLITE_UPSTREAM_DIR to re-derive the classification")

        def normalized(path: Path) -> list[str]:
            return [re.sub(r"\s+", "", line) for line in path.read_text().splitlines() if line.strip()]

        for ledger, _expected in LEDGERS:
            crate = ledger.parent
            with self.subTest(fork=crate.name):
                upstream = Path(upstream_root) / f"{crate.name}-0.14.0"
                manifest = self.manifest(crate)
                identical = {
                    str(p.relative_to(crate))
                    for p in (crate / "src").rglob("*.rs")
                    if (upstream / p.relative_to(crate)).is_file()
                    and normalized(upstream / p.relative_to(crate)) == normalized(p)
                }
                self.assertEqual(sorted(identical), sorted(manifest["upstream"]))


class RingOnlyProviderGraphCase(unittest.TestCase):
    """K-08 section 3.6 option A: the workspace compiles one rustls provider.

    The behavioural half lives in crates/plurx-core/tests/rustls_single_provider.rs,
    which only runs where Rust tests run. This half reads the lockfile and the
    fork manifest, so the graph cannot regain aws-lc through a lane that never
    compiles plurx-core's test binaries.
    """

    AWS_LC = {"aws-lc-rs", "aws-lc-sys"}

    def test_the_workspace_lock_resolves_no_aws_lc(self):
        names = {package["name"] for package in lock_packages(ROOT / "Cargo.lock")}
        self.assertFalse(
            names & self.AWS_LC,
            "a dependency re-enabled a second rustls provider; run "
            "`cargo tree -e features -i aws-lc-rs` to find the edge",
        )

    def test_the_fork_names_no_aws_lc_route(self):
        dependencies = tomllib.loads(
            (ROOT / "vendor/hiqlite/Cargo.toml").read_text(encoding="utf-8")
        )["dependencies"]
        self.assertEqual(dependencies["axum-server"]["features"], ["tls-rustls-no-provider"])
        rustls = dependencies["rustls"]
        self.assertIs(rustls["default-features"], False)
        self.assertIn("ring", rustls["features"])
        self.assertNotIn("prefer-post-quantum", rustls["features"])
        tokio_rustls = dependencies["tokio-rustls"]
        self.assertIs(tokio_rustls["default-features"], False)
        self.assertIn("ring", tokio_rustls["features"])
        for spec in (rustls, tokio_rustls):
            self.assertFalse(
                {"aws-lc-rs", "aws_lc_rs", "fips"} & set(spec["features"]),
                "the fork asks for aws-lc directly",
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
