"""Run the bounded POSIX source-reader contract in the ordinary preflight."""
import os
import unittest

from tools.dv_processing import test_metadata as metadata_checks


@unittest.skipUnless(os.name == "posix", "the native helper uses POSIX descriptors")
class DvSourceReaderTests(unittest.TestCase):
    def test_source_reader_cursor_budget_and_owned_shutdown(self):
        metadata_checks.MetadataAdmission().test_bounded_source_reader()


if __name__ == "__main__":
    unittest.main()
