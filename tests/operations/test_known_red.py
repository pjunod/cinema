import datetime as dt
import pathlib
import tempfile
import unittest

from validation.known_red import IgnoredTest, KnownRedError, ignored_tests, load_entries, validate_entries


ROOT = pathlib.Path(__file__).resolve().parents[2]


class KnownRedContractTest(unittest.TestCase):
    def test_every_checked_in_entry_is_current_and_ignored(self):
        ignored = ignored_tests(ROOT)
        self.assertEqual(len(ignored), 11)
        self.assertTrue(all(item.reason for item in ignored))
        validate_entries(load_entries(), ignored, dt.date(2026, 9, 20))

    def test_scanner_ignores_comments_and_reads_reasoned_attributes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "crates/example/src/lib.rs"
            source.parent.mkdir(parents=True)
            source.write_text(
                '/// mention #[ignore] only\n#[test]\n#[ignore = "owned reason"]\nfn held_case() {}\n',
                encoding="utf-8",
            )
            self.assertEqual(
                ignored_tests(root),
                (IgnoredTest("held_case", "owned reason", "crates/example/src/lib.rs", 3),),
            )

    def test_nonignored_and_expired_entries_fail_closed(self):
        ignored = (IgnoredTest("held_case", "reason", "crates/a.rs", 1),)
        base = {
            "suite": "rust",
            "test": "held_case",
            "owner": "ci",
            "reason": "repair pending",
            "expires": dt.date(2026, 10, 1),
        }
        validate_entries((base,), ignored, dt.date(2026, 9, 20))
        with self.assertRaises(KnownRedError):
            validate_entries(({**base, "test": "green_case"},), ignored, dt.date(2026, 9, 20))
        with self.assertRaises(KnownRedError):
            validate_entries(({**base, "expires": dt.date(2026, 9, 20)},), ignored, dt.date(2026, 9, 20))


if __name__ == "__main__":
    unittest.main()
