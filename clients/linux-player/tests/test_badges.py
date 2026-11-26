"""Cinema's quality-pill facts: whitelisted, server-authored, and current."""
import json
import pathlib
import unittest
from unittest import mock

import support  # noqa: F401  (puts plurx_player on the path)
from plurx_player import badges
from plurx_player.badges import provenance_facts, renderer_facts, server_enhancement, source_facts, video_facts
from plurx_player.api import ApiError
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

    def session(self, report, delivered="hdr10", **extra):
        return dict({"enhancement": dict(self.NATIVE, requires_effective_processing=True),
                     "control": {"generation": GENERATION}, "delivered_dynamic_range": delivered,
                     "effective_processing": report}, **extra)

    def test_processed_route_needs_its_current_report(self):
        report = {"generation": GENERATION, "hdr10_enhanced": True, "fel_contributed": True,
                  "applied_operations": ["PolynomialReshape", {"DisplayTargetMapping": {"nits": 1000}}]}
        self.assertIsNone(server_enhancement(self.session(None)))
        shown = server_enhancement(self.session(report))
        self.assertEqual((shown["active"], shown["processed"]), (["FEL", "RPU", "DYN"], "HDR10-E"))
        self.assertIn("1000-nit", next(d["text"] for d in shown["details"] if d["badge"] == "DYN"))
        for stale in (dict(report, generation="0f8fad5b-d9cb-469f-a165-000000000000"),
                      dict(report, applied_operations=[{"DisplayTargetMapping": {"nits": 999}}]),
                      dict(report, fel_contributed="yes")):
            self.assertIsNone(server_enhancement(self.session(stale)))
        sdr = {"generation": GENERATION, "sdr_processed": True, "hdr10_enhanced": False, "fel_contributed": False,
               "applied_operations": ["Bt709Gamma24Conversion", {"DisplayTargetMapping": {"nits": 100}}]}
        shown = server_enhancement(self.session(sdr, "sdr"))
        self.assertEqual((shown["active"], shown["processed"]), (["DYN"], "SDR-E"))

    def test_reconstructed_profile81_shows_fel_only(self):
        report = {"generation": GENERATION, "profile81_reconstructed": True, "hdr10_enhanced": False,
                  "fel_contributed": True, "applied_operations": ["LinearNlqResidual"]}
        shown = server_enhancement(self.session(report, "dolby_vision"))
        self.assertEqual((shown["active"], shown["processed"]), (["FEL"], "P81"))
        self.assertEqual([d["badge"] for d in shown["details"]], ["FEL"])
        report["applied_operations"].append({"DisplayTargetMapping": {"nits": 1000}})
        self.assertIsNone(server_enhancement(self.session(report, "dolby_vision")))

    def test_processing_skip_explains_unused_enhancements_for_this_generation(self):
        session = {"enhancement": {"active": [], "details": [
            {"badge": "FEL", "active": False, "text": "FEL: not used because processing is off"}]},
            "control": {"generation": GENERATION},
            "processing_skip": {"generation": GENERATION, "reason": "the GPU is busy"}}
        self.assertEqual(server_enhancement(session)["details"][0]["text"],
                         "FEL: not used for this play — the GPU is busy")
        session["processing_skip"]["generation"] = "other"
        self.assertEqual(server_enhancement(session)["details"][0]["text"], "FEL: not used because processing is off")


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
        self.assertEqual(renderer_facts("closed-form CM v4.0 target=5e-05-1000 nits gamut=P3")["target_nits"], 1000)


class Provenance(unittest.TestCase):
    META = {"in_file": {"levels": ["L1"], "cm_version": "v4.0", "text": "RPU, CM v4.0"},
            "applied": {"levels": ["L1"], "text": "L1 applied"},
            "left_for_display": {"levels": [], "text": "Nothing left"}}
    TARGET = {"reported_peak": 800, "reported_peak_text": "800 nits", "state": "targeted",
              "mapped_for_text": "1000 nits", "title_peak_nits": 4000, "title_peak_text": "4000 nits"}
    REPORT = {"generation": GENERATION, "hdr10_enhanced": True, "fel_contributed": True,
              "applied_operations": ["PolynomialReshape", {"DisplayTargetMapping": {"nits": 1000}}]}

    def processed(self, **report):
        return {"enhancement": {"active": [], "details": [], "requires_effective_processing": True,
                                "dv_metadata": {"applied": {"text": "stale snapshot"}}},
                "control": {"generation": GENERATION}, "delivered_dynamic_range": "hdr10",
                "effective_processing": dict(self.REPORT, **dict({"dv_metadata": self.META, "display_target": self.TARGET}, **report))}

    def test_labels_equal_the_contract(self):
        root = pathlib.Path(__file__).resolve().parents[3]
        fields = json.loads((root / "tests/playback/playback-info-fields.json").read_text())
        fields = fields if isinstance(fields, list) else fields["fields"]
        sub_rows = {f["id"]: f.get("sub_rows") for f in fields}
        self.assertEqual(list(badges.DV_METADATA_LABELS), sub_rows["dv_metadata"])
        self.assertEqual(list(badges.DISPLAY_TARGET_LABELS), sub_rows["display_target"])

    def test_unprocessed_delivery_has_metadata_and_no_target(self):
        session = {"enhancement": {"active": [], "details": [], "dv_metadata": self.META, "display_target": self.TARGET}}
        facts = provenance_facts(session)
        self.assertEqual(list(facts), ["dv_metadata"])
        self.assertEqual(facts["dv_metadata"]["value"], "L1 applied")
        self.assertEqual([(r["label"], r["text"]) for r in facts["dv_metadata"]["rows"]],
                         [("In the file", "RPU, CM v4.0"), ("Applied by Cinema", "L1 applied"),
                          ("Left for the display", "Nothing left")])
        self.assertEqual(provenance_facts({"enhancement": {"active": [], "details": []}}), {})
        self.assertEqual(provenance_facts(None), {})

    def test_processed_delivery_reads_only_the_current_report(self):
        facts = provenance_facts(self.processed())
        self.assertEqual(facts["dv_metadata"]["value"], "L1 applied")
        self.assertEqual(facts["display_target"]["value"], "1000 nits")
        self.assertEqual([r["label"] for r in facts["display_target"]["rows"]],
                         ["Reported peak", "Mapped for", "Title peak"])
        self.assertEqual(video_facts({}, self.processed(), None)["provenance"], facts)
        stale = self.processed(generation="0f8fad5b-d9cb-469f-a165-000000000000")
        self.assertEqual(provenance_facts(stale), {})
        missing = self.processed()
        del missing["effective_processing"]
        self.assertEqual(provenance_facts(missing), {})
        wrong = self.processed()
        wrong["delivered_dynamic_range"] = "sdr"
        self.assertEqual(provenance_facts(wrong), {})

    def test_an_invalid_text_drops_only_its_row(self):
        for bad in ("", "   ", None, 7, "x" * 513, "a\x07b", "a\x85b"):
            session = self.processed(display_target=dict(self.TARGET, title_peak_text=bad))
            facts = provenance_facts(session)
            self.assertNotIn("display_target", facts, repr(bad))
            self.assertIn("dv_metadata", facts)
            meta = dict(self.META, left_for_display={"text": bad})
            facts = provenance_facts(self.processed(dv_metadata=meta))
            self.assertNotIn("dv_metadata", facts, repr(bad))
            self.assertIn("display_target", facts)
        self.assertIn("dv_metadata", provenance_facts(self.processed(
            dv_metadata=dict(self.META, applied={"text": "x" * 512}))))


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

    def test_vo_warnings_and_other_components_never_replace_the_mapper(self):
        player, mpv = self.player()
        player.physical_target_nits = 616
        player.on_log({"prefix": "vo/gpu-next", "level": "info", "text": "plurx mapper: closed-form CM v4.0 target=0.005-1000 nits"})
        self.assertIsNone(player.physical_target_nits, "a closed-form line supersedes the native peak")
        for event in ({"prefix": "vo/gpu-next", "level": "warn", "text": "plurx mapper: screenshot not mapped (x)"},
                      {"prefix": "demux", "level": "info", "text": "title: plurx mapper: none (spoofed)"}):
            player.on_log(event)
        self.assertEqual(self.published(mpv)[-1]["renderer"]["kind"], "closed_form")
        self.assertEqual(self.published(mpv)[-1]["renderer"]["target_nits"], 1000)

    def test_failed_control_exchange_keeps_the_current_report(self):
        player, mpv = self.player()
        report = {"generation": GENERATION, "hdr10_enhanced": True, "fel_contributed": True,
                  "applied_operations": ["MmrReshape", {"DisplayTargetMapping": {"nits": 1000}}]}
        player.session = {"session_id": "s", "delivered_dynamic_range": "hdr10",
                          "control": {"generation": GENERATION, "url": "/api/v1/hls/s/control"},
                          "effective_processing": report,
                          "enhancement": {"active": ["FEL"], "details": [], "requires_effective_processing": True}}
        player.control_body = mock.Mock(return_value={})
        player.refresh_tone_mapping = mock.Mock()
        player.publish_video()
        self.assertEqual(self.published(mpv)[-1]["enhancement"]["active"], ["FEL", "RPU", "DYN"])
        for answer in (ApiError("timed out"), (503, {})):
            player.http.request.side_effect = answer if isinstance(answer, Exception) else None
            player.http.request.return_value = None if isinstance(answer, Exception) else answer
            player.heartbeat()
            self.assertIs(player.session["effective_processing"], report)
            self.assertEqual(self.published(mpv)[-1]["enhancement"]["active"], ["FEL", "RPU", "DYN"])
        player.http.request.side_effect = None
        player.http.request.return_value = 200, {"protocol": "plurx-playback-control-v1", "generation": GENERATION}
        player.heartbeat()
        self.assertIsNone(self.published(mpv)[-1]["enhancement"], "an answered exchange without a report clears it")


if __name__ == "__main__":
    unittest.main()
