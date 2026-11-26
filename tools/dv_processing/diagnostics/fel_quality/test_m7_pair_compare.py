"""Keep the guarded proposal distinct from the completed encoder recipe."""

import hashlib
import json
import unittest

from m7_pair_compare import validate_rate_proposal


def digest(document):
    return hashlib.sha256(json.dumps(document, sort_keys=True).encode()).hexdigest()


class RateProposalBindingTest(unittest.TestCase):
    def setUp(self):
        self.chosen = "michael5400-m7-eval-np1-L2"
        self.target = 124000
        self.request_sha = digest({"schema": 1, "output_root": "/lab/np1"})
        self.proposal = {
            "target_kbps": self.target,
            "request_sha256": self.request_sha,
            "driver_argv": ["--mode", "lumazfsp", "--prefix", self.chosen, "--override", "b=124000k"],
        }
        self.encode_recipe = {
            "status": "encoded",
            "overrides": ["x265-params+=cbqpoffs=-9:crqpoffs=-12:aq-strength=0", "b=124000k"],
        }

    def bind(self, proposal, author_recipe_sha):
        validate_rate_proposal(proposal, {"recipe_sha256": author_recipe_sha},
                               self.encode_recipe, chosen=self.chosen, target=self.target,
                               request_sha=self.request_sha, proposal_sha=digest(proposal))

    def test_author_runtime_binds_guarded_proposal_instead_of_completed_recipe(self):
        self.bind(self.proposal, digest(self.proposal))
        with self.assertRaisesRegex(ValueError, "must identify the guarded rate proposal"):
            self.bind(self.proposal, digest(self.encode_recipe))

    def test_guarded_proposal_must_match_selected_attempt_and_sealed_request(self):
        for update, expected in (
            ({"request_sha256": "a" * 64}, "invalid request_sha256"),
            ({"target_kbps": 124001}, "invalid target_kbps"),
            ({"driver_argv": ["--prefix", "michael5400-m7-eval-np1-L1", "--override", "b=124000k"]},
             "argv differs from the selected attempt"),
        ):
            with self.subTest(update=update):
                changed = self.proposal | update
                with self.assertRaisesRegex(ValueError, expected):
                    self.bind(changed, digest(changed))
