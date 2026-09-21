import datetime as dt
import pathlib
import tempfile
import unittest

from validation.known_red import (
    IgnoredTest,
    KnownRedError,
    ignored_tests,
    load_entries,
    validate_entries,
    validate_listed_tests,
)


ROOT = pathlib.Path(__file__).resolve().parents[2]


class KnownRedContractTest(unittest.TestCase):
    def _crate(self, root: pathlib.Path, lib: str) -> pathlib.Path:
        crate = root / "crates/example"
        (crate / "src").mkdir(parents=True)
        (crate / "Cargo.toml").write_text(
            '[package]\nname = "example"\nversion = "0.1.0"\nedition = "2021"\n',
            encoding="utf-8",
        )
        source = crate / "src/lib.rs"
        source.write_text(lib, encoding="utf-8")
        return crate

    @staticmethod
    def _entry(identity: str) -> dict[str, object]:
        return {
            "suite": "rust",
            "test": identity,
            "owner": "ci",
            "reason": "repair pending",
            "expires": dt.date(2026, 10, 1),
        }

    def test_every_checked_in_entry_is_current_and_ignored(self):
        ignored = ignored_tests(ROOT)
        # S-01 adds two reasoned ffmpeg fixture ignores to the P-01 baseline.
        self.assertEqual(len(ignored), 13)
        self.assertTrue(all(item.reason for item in ignored))
        self.assertTrue(all(item.path in item.identity for item in ignored))
        self.assertTrue(all(item.cargo_name in item.identity for item in ignored))
        validate_entries(load_entries(), ignored, dt.date(2026, 9, 20))

    def test_comments_and_raw_text_cannot_create_ignore_attributes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self._crate(
                root,
                '''
// #[test]\n// #[ignore = "comment"]\n// fn commented() {}\n
/* #[ignore = "block"] fn blocked() {} */
const EXAMPLE: &str = r###"#[ignore = "raw"] fn raw_text() {}"###;

#[test]
#[ignore = r"owned reason"]
fn held_case() {}
''',
            )
            ignored = ignored_tests(root)
            self.assertEqual(len(ignored), 1)
            self.assertEqual(ignored[0].cargo_name, "held_case")
            self.assertEqual(ignored[0].reason, "owned reason")

    def test_bare_or_empty_ignore_reason_fails_closed(self):
        for attribute in (
            "#[ignore]",
            '#[ignore = ""]',
            '#[ignore = b"bytes are not a Rust ignore reason"]',
            "#[cfg_attr(test, ignore)]",
            '#[cfg_attr(test, ignore = "")]',
        ):
            with self.subTest(attribute=attribute), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                self._crate(root, f"#[test]\n{attribute}\nfn held_case() {{}}\n")
                with self.assertRaisesRegex(KnownRedError, "ignore"):
                    ignored_tests(root)

    def test_detached_ignore_does_not_attach_across_an_unrelated_item(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self._crate(
                root,
                '''
#[ignore = "owned reason"]
const DETACHED: () = ();

#[test]
fn later_test() {}
''',
            )
            with self.assertRaisesRegex(KnownRedError, "detached"):
                ignored_tests(root)

    def test_ignore_must_share_the_attribute_bundle_of_a_test(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self._crate(root, '#[ignore = "owned reason"]\nfn helper() {}\n')
            with self.assertRaisesRegex(KnownRedError, "not attached to a test"):
                ignored_tests(root)

    def test_reasoned_ignore_has_a_stable_source_and_module_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self._crate(
                root,
                '''
mod suite {
    #[cfg_attr(test, tokio::test, ignore = "hardware owner: remove after lab qualification")]
    async fn held_case() {}
}
''',
            )
            ignored = ignored_tests(root)
            self.assertEqual(
                ignored,
                (
                    IgnoredTest(
                        identity="crates/example/src/lib.rs::suite::held_case",
                        cargo_name="suite::held_case",
                        reason="hardware owner: remove after lab qualification",
                        path="crates/example/src/lib.rs",
                        line=3,
                    ),
                ),
            )

    def test_same_function_name_across_modules_and_files_is_not_a_bare_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            crate = self._crate(
                root,
                '''
mod alpha {
    #[test]
    #[ignore = "alpha owner"]
    fn held_case() {}
}
mod beta;
''',
            )
            (crate / "src/beta.rs").write_text(
                '#[test]\n#[ignore = "beta owner"]\nfn held_case() {}\n',
                encoding="utf-8",
            )
            ignored = ignored_tests(root)
            self.assertEqual(
                {item.identity for item in ignored},
                {
                    "crates/example/src/lib.rs::alpha::held_case",
                    "crates/example/src/beta.rs::beta::held_case",
                },
            )
            with self.assertRaisesRegex(KnownRedError, "one ignored test identity"):
                validate_entries(
                    (self._entry("held_case"),), ignored, dt.date(2026, 9, 20)
                )
            validate_entries(
                (self._entry("crates/example/src/lib.rs::alpha::held_case"),),
                ignored,
                dt.date(2026, 9, 20),
            )
            validate_listed_tests(
                ignored, ("alpha::held_case", "beta::held_case")
            )

    def test_duplicate_catalog_and_cargo_identities_fail_closed(self):
        ignored = (
            IgnoredTest(
                "crates/example/src/lib.rs::suite::held_case",
                "suite::held_case",
                "reason",
                "crates/example/src/lib.rs",
                1,
            ),
        )
        entry = self._entry(ignored[0].identity)
        with self.assertRaisesRegex(KnownRedError, "duplicates"):
            validate_entries((entry, entry), ignored, dt.date(2026, 9, 20))
        with self.assertRaisesRegex(KnownRedError, "ambiguous"):
            validate_listed_tests(
                ignored, ("suite::held_case", "suite::held_case")
            )
        with self.assertRaisesRegex(KnownRedError, "absent"):
            validate_listed_tests(ignored, ("prefix::suite::held_case",))

    def test_nonignored_and_expired_entries_fail_closed(self):
        ignored = (
            IgnoredTest(
                "crates/example/src/lib.rs::held_case",
                "held_case",
                "reason",
                "crates/example/src/lib.rs",
                1,
            ),
        )
        base = self._entry(ignored[0].identity)
        validate_entries((base,), ignored, dt.date(2026, 9, 20))
        with self.assertRaises(KnownRedError):
            validate_entries(
                ({**base, "test": "crates/example/src/lib.rs::green_case"},),
                ignored,
                dt.date(2026, 9, 20),
            )
        with self.assertRaises(KnownRedError):
            validate_entries(
                ({**base, "expires": dt.date(2026, 9, 20)},),
                ignored,
                dt.date(2026, 9, 20),
            )


if __name__ == "__main__":
    unittest.main()
