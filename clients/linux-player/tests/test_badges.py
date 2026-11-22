"""Cinema's quality-pill facts: whitelisted, server-authored, and current."""
import json
import unittest
from unittest import mock

from plurx_player.badges import renderer_facts, server_enhancement, source_facts, video_facts
from plurx_player.playback import Playback

TOKEN = "secret-token-value"
GENERATION = "0f8fad5b-d9cb-469f-a165-70867728950e"


class SourceFacts(unittest.TestCase):
    def test_only_grade_fields_pass(self):
        decision = {"source": {"hdr": "dolby_vision", "hdr_format": "Dolby Vision · Profile 7", "dv_profile": 7,
                               "dv_el_present": True, "video_codec": "hevc", "bit_depth": 10,
                               "filename": "/private/movie.mkv", "url": "https://media1/x?token=" + TOKEN},
                    "token": TOKEN}
        facts = source_facts(decision)
        self.assertEqual(facts, {"hdr": "dolby_vision", "hdr_format": "Dolby Vision · Profile 7", "dv_profile": 7,
                                 "dv_el_present": True, "video_codec": "hevc", "bit_depth": 10})
        self.assertEqual(source_facts({"source": {"dv_profile": "7", "dv_el_present": 1}}), {})
        self.assertEqual(source_facts(None), {})


class ServerEnhancement(unittest.TestCase):
    NATIVE = {"active": ["DYN", "XYZ"], "details": [
        {"badge": "FEL", "active": False, "text": "FEL: delivered for the display to decode"},
        {"badge": "RPU", "active": False, "text": "RPU: carried to the display, which applies it"},
        {"badge": "DYN", "active": True, "text": "DYN: dynamic metadata reaches the display"},
        {"badge": "DYN", "text": "x" * 241}, "junk"]}

    def test_native_route_keeps_known_badges_and_bounded_details(self):
        shown = server_enhancement({"enhancement": self.NATIVE})
        self.assertEqual(shown["active"], ["DYN"])
        self.assertEqual([d["badge"] for d in shown["details"]], ["FEL", "RPU", "DYN"])
        self.assertIsNone(server_enhancement({"enhancement": {"active": "DYN"}}))
        self.assertIsNone(server_enhancement(None))

    def test_processed_route_needs_its_current_report(self):
        raw = dict(self.NATIVE, requires_effective_processing=True)
        self.assertIsNone(server_enhancement({"enhancement": raw}))
        report = {"fel_contributed": True, "sdr_processed": False,
                  "applied_operations": ["PolynomialReshape", {"DisplayTargetMapping": {"nits": 1000}}]}
        shown = server_enhancement({"enhancement": raw}, report)
        self.assertEqual(shown["active"], ["FEL", "RPU", "DYN"])
        self.assertIn("1000-nit", next(d["text"] for d in shown["details"] if d["badge"] == "DYN"))
        report = {"fel_contributed": False, "applied_operations": ["TargetMapping"]}
        self.assertEqual(server_enhancement({"enhancement": raw}, report)["active"], [])


class RendererFacts(unittest.TestCase):
    def test_mapper_lines_classify(self):
        self.assertEqual(renderer_facts("shared native Dolby renderer", 998.6),
                         {"kind": "native", "label": "shared native Dolby renderer", "target_nits": 999})
        closed = renderer_facts("closed-form CM v4.0 target=0.005-1000 nits gamut=P3 (trim none")
        self.assertEqual((closed["kind"], closed["target_nits"]), ("closed_form", 1000))
        self.assertEqual(renderer_facts("none (no Dolby Vision RPU)")["kind"], "none")
        self.assertEqual(renderer_facts("none (the display reports no colour description; libplac")["kind"],
                         "libplacebo")
        self.assertIsNone(renderer_facts(None))
        self.assertIsNone(renderer_facts("closed-form CM v4.0 target=bad")["target_nits"])


class PublishVideo(unittest.TestCase):
    def player(self):
        mpv = mock.Mock()
        player = Playback(mock.Mock(), mpv, {"file_id": 1}, {}, lambda: 0, lambda _: None)
        player.decision = {"source": {"hdr": "dolby_vision", "dv_profile": 7, "filename": "/private/m.mkv"}}
        return player, mpv

    def published(self, mpv):
        return [c.args[1] for c in mpv.set.call_args_list if c.args[0] == "user-data/cinema-video"]

    def test_renderer_line_publishes_once_without_credentials(self):
        player, mpv = self.player()
        player.session = {"session_id": TOKEN, "playlist_url": "https://media1/" + TOKEN,
                          "enhancement": {"active": ["DYN"], "details": []}}
        player.on_log({"prefix": "vo/gpu-next", "level": "info", "text": "plurx mapper: closed-form CM v4.0 target=0.005-1000 nits"})
        player.on_log({"prefix": "vo/gpu-next", "level": "info", "text": "plurx mapper: closed-form CM v4.0 target=0.005-1000 nits"})
        values = self.published(mpv)
        self.assertEqual(len(values), 1)
        self.assertEqual(values[0]["renderer"]["kind"], "closed_form")
        self.assertEqual(values[0]["enhancement"]["active"], ["DYN"])
        for secret in (TOKEN, "/private/", "https://"):
            self.assertNotIn(secret, json.dumps(values[0]))
        player.reset_renderer_facts()
        self.assertIsNone(self.published(mpv)[-1]["renderer"])

    def test_chrome_update_carries_current_facts_and_stays_last(self):
        player, mpv = self.player()
        player.publish_chrome()
        self.assertEqual(self.published(mpv)[-1]["source"]["dv_profile"], 7)
        self.assertEqual(mpv.set.call_args.args[0], "user-data/cinema")

    def test_processed_pills_follow_the_generation_matched_report(self):
        player, mpv = self.player()
        report = {"generation": GENERATION, "hdr10_enhanced": True, "fel_contributed": True,
                  "applied_operations": ["MmrReshape", {"DisplayTargetMapping": {"nits": 1000}}]}
        player.session = {"session_id": "s", "delivered_dynamic_range": "hdr10",
                          "control": {"generation": GENERATION}, "effective_processing": report,
                          "enhancement": {"active": ["FEL"], "details": [], "requires_effective_processing": True}}
        player.publish_video()
        self.assertEqual(self.published(mpv)[-1]["enhancement"]["active"], ["FEL", "RPU", "DYN"])
        player.session["effective_processing"] = dict(report, generation="other")
        player.publish_video()
        self.assertIsNone(self.published(mpv)[-1]["enhancement"])


if __name__ == "__main__":
    unittest.main()
