"""ARCHITECTURE.md quotes source constants instead of copying silent numbers."""

from __future__ import annotations

from pathlib import Path
import unittest

from validation.doc_versions import validate_documented_constants


ROOT = Path(__file__).resolve().parents[2]


def repository_read(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


class ArchitectureConstantCase(unittest.TestCase):
    def test_repository_quotes_match_their_sources(self) -> None:
        self.assertEqual(validate_documented_constants(repository_read), ())

    def test_a_source_change_rejects_the_old_documented_value(self) -> None:
        def read(path: str) -> str:
            contents = repository_read(path)
            if path == "crates/plurx-core/src/transcode/mod.rs":
                return contents.replace(
                    "pub const SEGMENT_SECONDS: u32 = 2;",
                    "pub const SEGMENT_SECONDS: u32 = 3;",
                    1,
                )
            return contents

        errors = validate_documented_constants(read)

        self.assertEqual(len(errors), 1, errors)
        self.assertIn("docs/ARCHITECTURE.md", errors[0])
        self.assertIn("`SEGMENT_SECONDS` = 2", errors[0])
        self.assertIn("crates/plurx-core/src/transcode/mod.rs", errors[0])
        self.assertIn("defines 3", errors[0])

    def test_an_unregistered_documented_constant_fails_closed(self) -> None:
        def read(path: str) -> str:
            contents = repository_read(path)
            if path == "docs/ARCHITECTURE.md":
                return contents + "\n`UNREGISTERED_LIMIT` = 9\n"
            return contents

        errors = validate_documented_constants(read)

        self.assertEqual(len(errors), 1, errors)
        self.assertIn("`UNREGISTERED_LIMIT` = 9", errors[0])
        self.assertIn("does not know", errors[0])

    def test_a_new_web_asset_rejects_all_four_stale_prose_surfaces(self) -> None:
        def read(path: str) -> str:
            contents = repository_read(path)
            if path == "crates/plurxd/src/http/web.rs":
                anchor = (
                    '    ("router.js",                              '
                    'WebAsset::BodyScript,  include_str!("../web/router.js")),\n'
                )
                return contents.replace(anchor, anchor + anchor, 1)
            return contents

        failure = "\n".join(validate_documented_constants(read))

        self.assertIn("docs/ARCHITECTURE.md", failure)
        for path in (
            "crates/plurxd/src/http/web.rs",
            "tests/web/asset-graph.js",
            "tests/web/shell-source.js",
        ):
            self.assertIn(path, failure)
        # W-01 adds `web/core/errors.js`, so `WEB_ASSETS` is sixty-six at HEAD
        # and the duplicated row above makes the synthetic read sixty-seven.
        self.assertIn('must say "sixty-seven"', failure)


if __name__ == "__main__":
    unittest.main()
