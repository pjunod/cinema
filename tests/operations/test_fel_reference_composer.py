"""The independent FEL composer reference (ETSI GS CCM 001) keeps passing
its hand-computed vectors.

tools/dv_processing/fel_reference_composer.h is the oracle the FEL
reference composition plan measures the helper against; a change to it that
breaks a vector would silently move every later parity verdict.
"""
import os
import unittest

from tools.dv_processing import test_metadata as metadata_checks


@unittest.skipUnless(os.name == "posix", "the vectors build with the host C compiler")
class FelReferenceComposerTests(unittest.TestCase):
    def test_hand_computed_vectors(self):
        metadata_checks.MetadataAdmission().test_fel_reference_composer_vectors()


if __name__ == "__main__":
    unittest.main()
