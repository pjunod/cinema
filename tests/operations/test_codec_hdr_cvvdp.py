"""Authored boundary controls for the optional offline perceptual adapter."""

import copy
from fractions import Fraction
import json
from pathlib import Path
import runpy
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[2]


def pair(namespace):
    stream = dict(namespace["GRADE"], width=64, height=64, chroma_location="left", time_base="1/60000")
    frames = [dict(stream, best_effort_timestamp=index * 1001,
                   best_effort_timestamp_time=f"{float(index * Fraction(1001, 60000)):.6f}")
              for index in range(3)]
    return stream, frames


def display():
    return {"schema": 1, "colorspace": "BT.2020-PQ", "resolution": [64, 64],
            "diagonal_size_inches": 30, "viewing_distance_meters": 0.7472,
            "peak_luminance_cd_m2": 1500, "contrast": 1000000, "ambient_lux": 10,
            "reflectivity": 0.005, "exposure": 1}


def prediction(frames=3):
    return {"schema": 1, "device": "cpu", "temporal_resampling": False, "jod": 10.0,
            "N_frames": frames, "frames_read_reference": list(range(frames)),
            "frames_read_test": list(range(frames)), "features_finite": True,
            "model_info": "authored control, not model execution", "model_version": "control"}


class CodecHdrCvvdpTests(unittest.TestCase):
    def setUp(self):
        self.base = runpy.run_path(str(ROOT / "scripts/codec-hdr-score"))
        self.ns = runpy.run_path(str(ROOT / "scripts/codec-hdr-cvvdp"))

    def test_exact_fractional_clocks_and_equivalent_time_bases(self):
        reference = pair(self.base)
        distorted = copy.deepcopy(reference)
        distorted[0]["time_base"] = "1/120000"
        for frame in distorted[1]:
            frame["best_effort_timestamp"] *= 2
        result = self.ns["validate_clocks"](self.base, reference, distorted)
        self.assertEqual(result["frames_per_second"], "60000/1001")
        self.assertEqual(result["exact_timestamps"], ["0", "1001/60000", "1001/30000"])

    def test_invalid_clocks_and_siting_refuse_even_shared_invalid_pairs(self):
        for label, mutate in [
            ("irregular", lambda p: p[1][2].update(best_effort_timestamp=2003)),
            ("duplicate", lambda p: p[1][2].update(best_effort_timestamp=1001)),
            ("missing pts", lambda p: p[1][1].pop("best_effort_timestamp")),
            ("noninteger", lambda p: p[1][1].update(best_effort_timestamp=1001.5)),
            ("missing base", lambda p: p[0].pop("time_base")),
            ("bad base", lambda p: p[0].update(time_base="0/1")),
            ("missing siting", lambda p: p[1][2].pop("chroma_location")),
            ("center siting", lambda p: p[1][2].update(chroma_location="center")),
            ("container siting", lambda p: p[0].update(chroma_location="center")),
            ("late grade", lambda p: p[1][2].update(color_transfer="bt709")),
        ]:
            with self.subTest(label=label):
                a = pair(self.base); mutate(a)
                with self.assertRaises((self.ns["Refusal"], self.base["Refusal"])):
                    self.ns["validate_clocks"](self.base, a, copy.deepcopy(a))
        a = pair(self.base); b = copy.deepcopy(a)
        for frame in b[1]:
            frame["best_effort_timestamp"] += 1
        with self.assertRaises(self.ns["Refusal"]):
            self.ns["validate_clocks"](self.base, a, b)

    def test_display_contract_and_no_implicit_scaling(self):
        self.ns["validate_display"](display(), 64, 64)
        for field, value in [("colorspace", "sRGB"), ("resolution", [128, 64]),
                             ("contrast", 1), ("ambient_lux", -1),
                             ("peak_luminance_cd_m2", float("nan")), ("reflectivity", 2)]:
            with self.subTest(field=field):
                d = display(); d[field] = value
                with self.assertRaises(self.ns["Refusal"]):
                    self.ns["validate_display"](d, 64, 64)
        d = display(); d.pop("exposure")
        with self.assertRaises(self.ns["Refusal"]):
            self.ns["validate_display"](d)

    def test_raw_budget_length_and_frame_hashes(self):
        p = {"width": 3840, "height": 2160, "frames": 48}
        self.assertEqual(self.ns["raw_size"](p), 3840 * 2160 * 48 * 12)
        with self.assertRaises(self.ns["Refusal"]):
            self.ns["raw_size"](dict(p, frames=600))
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "raw"
            path.write_bytes(b"a" * 12 + b"b" * 12)
            q = {"width": 1, "height": 1, "frames": 2}
            hashes = self.ns["frame_hashes"](path, q)
            self.assertNotEqual(*hashes)
            self.ns["verify_raw"](path, 24)
            for data in (b"a" * 23, b"a" * 25):
                path.write_bytes(data)
                with self.assertRaises(self.ns["Refusal"]):
                    self.ns["verify_raw"](path, 24)
                with self.assertRaises(self.ns["Refusal"]):
                    self.ns["frame_hashes"](path, q)

    def test_export_log_requires_all_exact_pts_and_ordinals(self):
        p = self.ns["validate_clocks"](self.base, pair(self.base), pair(self.base))
        text = "[Parsed_showinfo_1] config in time_base: 1/60000, frame_rate: 60000/1001\n"
        text += "".join(f"[Parsed_showinfo_1] n: {i} pts: {i * 1001} pts_time:ignored\n" for i in range(3))
        self.assertEqual(self.ns["validate_export_log"](text, p)["source_pts"], [0, 1001, 2002])
        for changed in (text.replace("2002", "2003"), text.replace("n: 2", "n: 1"),
                        "\n".join(text.splitlines()[:-1]), text.replace("1/60000", "1/60")):
            with self.assertRaises(self.ns["Refusal"]):
                self.ns["validate_export_log"](changed, p)

    def test_model_result_requires_finite_complete_cpu_prediction(self):
        self.ns["validate_result"](prediction(), 3)
        for key, value in [("N_frames", 2), ("frames_read_reference", [0, 2]),
                           ("frames_read_test", [0, 1]), ("jod", float("inf")),
                           ("features_finite", False), ("device", "mps"),
                           ("temporal_resampling", True), ("model_version", "")]:
            with self.subTest(key=key):
                p = prediction(); p[key] = value
                with self.assertRaises(self.ns["Refusal"]):
                    self.ns["validate_result"](p, 3)

    def test_runtime_manifest_pin_detects_file_replacement(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp).resolve() / "python"
            path.write_bytes(b"authored executable identity; never executed")
            manifest = {"schema": 1, "python_invocation": str(path), "files": [
                {"path": str(path), "resolved": str(path), "bytes": path.stat().st_size,
                 "sha256": self.ns["sha256"](path)}]}
            self.assertEqual(self.ns["verify_runtime"](manifest), str(path))
            pin = Path(tmp).resolve() / "runtime.json"
            pin.write_text(json.dumps(manifest))
            digest = self.ns["sha256"](pin)
            self.ns["read_json"](pin, digest)
            pin.write_text("{}")
            with self.assertRaises(self.ns["Refusal"]):
                self.ns["read_json"](pin, digest)
            path.write_bytes(b"changed")
            with self.assertRaises(self.ns["Refusal"]):
                self.ns["verify_runtime"](manifest)

    def test_cli_requires_complete_pins_without_running_worker(self):
        with self.assertRaises(SystemExit) as caught:
            self.ns["main"](self.base, ["score-perceptual", "--reference", "one"])
        self.assertEqual(caught.exception.code, 2)

    def orchestration(self, alteration=None):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            paths = {name: root / name for name in ("reference", "distorted", "ffmpeg", "ffprobe", "python")}
            for name, path in paths.items():
                path.write_bytes(name.encode())
            python = paths["python"]
            manifest = {"schema": 1, "python_invocation": str(python), "scope": "authored test closure", "files": [
                {"path": str(python), "resolved": str(python), "bytes": python.stat().st_size,
                 "sha256": self.ns["sha256"](python)}]}
            runtime = root / "runtime.json"; runtime.write_text(json.dumps(manifest))
            dp = root / "display.json"; dp.write_text(json.dumps(display()))
            args = SimpleNamespace(**{name: str(paths[name]) for name in ("reference", "distorted", "ffmpeg", "ffprobe")},
                reference_sha256=self.ns["sha256"](paths["reference"]), distorted_sha256=self.ns["sha256"](paths["distorted"]),
                cvvdp_runtime=str(runtime), cvvdp_runtime_sha256=self.ns["sha256"](runtime),
                cvvdp_display=str(dp), cvvdp_display_sha256=self.ns["sha256"](dp),
                output=str(root / "out"), duration=0.1, shadow_code=80, highlight_code=900)
            seen = []
            def probe(command):
                fields = command[command.index("-show_entries") + 1]
                self.assertIn("time_base", fields); self.assertIn("best_effort_timestamp,", fields)
                self.assertIn("chroma_location", fields)
                a, af = pair(self.base)
                return json.dumps({"streams": [a], "frames": af})
            def execute(command, out, name, file_limit=None):
                seen.append(name)
                out = Path(out); (out / (name + ".stdout")).write_text("")
                if name.startswith("export-"):
                    self.assertNotIn("-r", command)
                    self.assertEqual(command[command.index("-fps_mode") + 1], "passthrough")
                    self.assertIn("showinfo", command[command.index("-vf") + 1])
                    Path(command[-1]).write_bytes(bytes(64 * 64 * 3 * 12 - (1 if alteration == "raw" else 0)))
                    log = "[Parsed_showinfo_1] config in time_base: 1/60000, frame_rate: 60000/1001\n"
                    log += "".join(f"[Parsed_showinfo_1] n: {i} pts: {i * 1001} pts_time:ignored\n" for i in range(3))
                    (out / (name + ".stderr")).write_text(log)
                else:
                    value = prediction()
                    if alteration == "partial": value["frames_read_test"] = [0, 1]
                    (out / (name + ".stdout")).write_text(json.dumps(value))
                return {"authored": name}
            score = Mock(return_value={"hdr_perceptual_model": None, "metrics": "incumbent control"})
            namespace = dict(self.base, run=probe, score=score)
            with patch.dict(self.ns["score_perceptual"].__globals__, execute=execute):
                if alteration:
                    with self.assertRaises(self.ns["Refusal"]):
                        self.ns["score_perceptual"](namespace, args)
                    if alteration == "raw": self.assertNotIn("cvvdp", seen)
                else:
                    result = self.ns["score_perceptual"](namespace, args)
                    self.assertEqual(result["metrics"], "incumbent control")
                    self.assertEqual(result["hdr_perceptual_model"]["prediction"]["jod"], 10)
                    self.assertEqual(len(result["hdr_perceptual_model"]["raw"]["reference"]["frame_sha256"]), 3)
                    self.assertTrue((root / "out" / "report.json").is_file())
                score.assert_called_once()

    def test_optional_orchestration_reuses_scorer_and_complete_export(self):
        self.orchestration()

    def test_truncated_export_and_partial_model_consumption_refuse(self):
        self.orchestration("raw")
        self.orchestration("partial")

    def test_incumbent_score_remains_available_without_model_runtime(self):
        existing = runpy.run_path(str(ROOT / "tests/operations/test_codec_hdr_score.py"))
        case = existing["CodecHdrScoreTests"]()
        admitted = pair(self.base)
        result = case.score_probe_pair(self.base, admitted, copy.deepcopy(admitted))
        self.assertIsNone(result["hdr_perceptual_model"])
        self.assertEqual(result["pairing"]["frames"], 3)


if __name__ == "__main__":
    unittest.main()
