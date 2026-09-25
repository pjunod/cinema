"""THIRD-PARTY-NOTICES.md is a claim about what plurx redistributes.

Attribution is only discharged if the notices are accurate AND they reach the
recipient, so this suite checks both halves: the crate table against the
lockfile that produced it, and the packaging paths that carry the notices into
a published image. Every assertion here corresponds to a way the file has
already been wrong or nearly shipped wrong.
"""

from __future__ import annotations

from collections import Counter
from pathlib import Path
import re
import unittest

from validation.release_dockerfile import render, render_binary_export


ROOT = Path(__file__).resolve().parents[2]
NOTICES = ROOT / "THIRD-PARTY-NOTICES.md"
LOCKFILE = ROOT / "Cargo.lock"
DOCKERFILE = ROOT / "Dockerfile"
DOCKERIGNORE = ROOT / ".dockerignore"

# Copied into the runtime image. A context copy, not a build-stage copy:
# validation/release_dockerfile.py rejects any build-stage copy it does not
# recognise as a known binary, and it matches the literal token, so even a
# comment naming that form fails the release rewrite.
IMAGE_COPIES = (
    "COPY LICENSE NOTICE THIRD-PARTY-NOTICES.md /usr/share/doc/plurx/",
    "COPY licenses/ /usr/share/doc/plurx/licenses/",
)

# Vendored under vendor/, so they carry no `source` key in the lockfile and are
# attributed in section 3 rather than in the resolved-dependency table.
VENDORED = {"hiqlite", "hiqlite-wal", "rust_decimal", "s3-simple"}


def lockfile_externals() -> set[tuple[str, str]]:
    """(name, version) for every crate resolved from a registry or git.

    A `source` key is what distinguishes a fetched dependency from a
    path-resolved one, which is why first-party and vendored crates fall out
    here without being named individually.

    Keyed by name AND version, never by name alone: 26 crates currently
    resolve at two or three versions at once (`rand` at 0.9.5 and 0.10.2,
    `thiserror` at 1 and 2), and each distinct version is a distinct artifact
    that ships. Collapsing them hides whichever one sorts last.
    """

    blocks = LOCKFILE.read_text(encoding="utf-8").split("[[package]]")[1:]
    resolved = set()
    for block in blocks:
        if "\nsource = " not in block:
            continue
        name = re.search(r'\nname = "([^"]+)"', block)
        version = re.search(r'\nversion = "([^"]+)"', block)
        if name and version:
            resolved.add((name.group(1), version.group(1)))
    return resolved


def notices_table() -> set[tuple[str, str]]:
    """(name, version) for every row of the full dependency table."""

    body = NOTICES.read_text(encoding="utf-8")
    block = body.split("<summary>", 1)[1].split("</details>", 1)[0]
    rows = re.findall(r"^\| `([^`]+)` \| ([^|]+?) \| ", block, flags=re.MULTILINE)
    return {(name, version.strip()) for name, version in rows}


def notices_licenses() -> list[str]:
    """The license column of every row of the full dependency table."""

    body = NOTICES.read_text(encoding="utf-8")
    block = body.split("<summary>", 1)[1].split("</details>", 1)[0]
    return re.findall(r"^\| `[^`]+` \| [^|]+? \| (.+?) \|$", block, flags=re.MULTILINE)


def license_summary() -> dict[str, int]:
    """Section 4's license-expression summary, as {expression: crate count}."""

    body = NOTICES.read_text(encoding="utf-8")
    section = body.split("\n## 4. Rust dependencies", 1)[1].split("<details>", 1)[0]
    return {
        expression: int(count)
        for expression, count in re.findall(
            r"^\| `([^`]+)` \| (\d+) \|$", section, flags=re.MULTILINE
        )
    }


class LicenseNoticesCase(unittest.TestCase):
    def test_the_license_summary_counts_the_full_list(self):
        """The (crate, version) checks below cannot see the summary table, so
        it kept aws-lc and CC0 rows after the crates behind them had left the
        graph. It is derived from the full list and must equal it.
        """

        listed = dict(sorted(Counter(notices_licenses()).items()))
        self.assertTrue(listed, "the full crate list did not parse")
        self.assertEqual(
            dict(sorted(license_summary().items())),
            listed,
            "the section 4 summary disagrees with the full crate list; "
            "recompute it from the list's license column",
        )

    def test_every_stated_crate_count_is_the_list_length(self):
        body = NOTICES.read_text(encoding="utf-8")
        rows = len(notices_licenses())
        stated = [
            int(count)
            for count in re.findall(r"(\d+) source-bearing crates", body)
            + re.findall(r"Full crate list \((\d+)\)", body)
        ]
        self.assertGreaterEqual(len(stated), 3, "a crate count stopped parsing")
        self.assertEqual(
            set(stated),
            {rows},
            f"THIRD-PARTY-NOTICES.md states crate counts {stated} for a list "
            f"of {rows} rows",
        )

    def test_every_resolved_dependency_is_attributed(self):
        missing = sorted(lockfile_externals() - notices_table())
        self.assertEqual(
            missing,
            [],
            "Cargo.lock resolves crates that THIRD-PARTY-NOTICES.md does not "
            "attribute, as (crate, version). A dependency that ships without "
            "its notice is the failure this file exists to prevent.",
        )

    def test_the_table_lists_nothing_that_is_no_longer_a_dependency(self):
        stale = sorted(notices_table() - lockfile_externals())
        self.assertEqual(
            stale,
            [],
            "THIRD-PARTY-NOTICES.md attributes (crate, version) pairs the "
            "build no longer resolves. Stale rows make the file look "
            "maintained while the real graph drifts.",
        )

    def test_vendored_crates_are_attributed_separately(self):
        body = NOTICES.read_text(encoding="utf-8")
        for name in sorted(VENDORED):
            self.assertIn(
                f"`{name}`",
                body,
                f"{name} is vendored under vendor/ and carries no lockfile "
                "source, so it is invisible to the resolved-dependency table "
                "and has to be named in section 3.",
            )

    def test_referenced_license_texts_exist(self):
        body = NOTICES.read_text(encoding="utf-8")
        for target in sorted(set(re.findall(r"\]\((licenses/[^)]+)\)", body))):
            self.assertTrue(
                (ROOT / target).is_file(),
                f"{target} is cited as a full license text but is not in the "
                "tree, so the license it stands in for ships with nothing.",
            )
        self.assertTrue((ROOT / "LICENSE").is_file())
        self.assertTrue((ROOT / "NOTICE").is_file())

    def test_every_relative_link_resolves(self):
        body = NOTICES.read_text(encoding="utf-8")
        broken = [
            target
            for target in re.findall(r"\]\(([^)#][^)]*)\)", body)
            if not target.startswith(("http://", "https://", "mailto:"))
            and not (ROOT / target).exists()
        ]
        self.assertEqual(broken, [], "broken relative links in the notices")

    def test_the_runtime_image_carries_the_notices(self):
        """Apache-2.0 4(a)/(d) and the OFL both condition redistribution on
        the notices travelling with the work, and the web UI -- fonts, icons
        and hls.js included -- is compiled into the binary. An image holding
        only the binary redistributes all of it with nothing attached.
        """

        dockerfile = DOCKERFILE.read_text(encoding="utf-8")
        for line in IMAGE_COPIES:
            self.assertIn(line, dockerfile)

    def test_the_notices_survive_the_release_rewrite(self):
        """Both published variants are generated from the tagged runtime
        stage rather than built from this Dockerfile, so a copy that does not
        survive `render` reaches no released image.
        """

        dockerfile = DOCKERFILE.read_text(encoding="utf-8")
        generated = render(dockerfile)
        for line in IMAGE_COPIES:
            self.assertIn(line, generated)
        render_binary_export(dockerfile)

    def test_the_build_context_does_not_exclude_the_notices(self):
        """`.dockerignore` excludes `*.md`, which silently covers
        THIRD-PARTY-NOTICES.md and breaks the image build at the COPY. The
        negation has to come after the pattern it undoes -- last match wins.
        """

        lines = [
            line.strip()
            for line in DOCKERIGNORE.read_text(encoding="utf-8").splitlines()
            if line.strip() and not line.strip().startswith("#")
        ]
        self.assertIn("*.md", lines)
        self.assertIn("!THIRD-PARTY-NOTICES.md", lines)
        self.assertGreater(
            lines.index("!THIRD-PARTY-NOTICES.md"),
            lines.index("*.md"),
            "the negation must follow *.md or it is overridden",
        )

    def test_no_document_still_calls_the_project_unlicensed(self):
        """A repo that says two different things about its license is worse
        than one that says nothing.
        """

        stale = []
        for path in list(ROOT.glob("*.md")) + list(ROOT.glob("docs/**/*.md")):
            for number, line in enumerate(
                path.read_text(encoding="utf-8", errors="ignore").splitlines(), 1
            ):
                if re.search(r"licen[cs]e", line, re.IGNORECASE) and re.search(
                    r"private for now|all rights reserved|licensing will be decided",
                    line,
                    re.IGNORECASE,
                ):
                    stale.append(f"{path.relative_to(ROOT)}:{number}")
        self.assertEqual(stale, [], "documents still claiming no license")


if __name__ == "__main__":
    unittest.main()
