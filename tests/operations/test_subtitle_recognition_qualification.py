"""The recognition diagnostic must expose errors without inventing quality proof."""
import hashlib
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
import wave

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader("recognition_qualification_test", str(ROOT / "scripts/qualify-subtitle-recognition"))
spec = importlib.util.spec_from_loader(loader.name, loader)
TOOL = importlib.util.module_from_spec(spec)
loader.exec_module(TOOL)


class RecognitionQualificationCase(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.fixture = ROOT / "crates/plurxd/src/subtitle_models/self-test.wav"

    def test_word_error_counts_omission_insertion_and_substitution(self):
        self.assertEqual(TOOL.error_rate("Ask NOT your country", "ask your country"),
                         dict(reference_words=4, hypothesis_words=3, deletions=1, insertions=0, substitutions=0, wer=0.25))
        errors = TOOL.error_rate("eins zwei drei", "eins vier drei extra")
        self.assertEqual((errors["substitutions"], errors["insertions"], errors["wer"]), (1, 1, 2 / 3))
        self.assertEqual(TOOL.error_rate("Grüße, Größe!", "GRÜSSE größe")["wer"], 0)
        self.assertIsNone(TOOL.error_rate("", "hallucination")["wer"])

    def test_fixture_noise_is_reproducible_and_silence_is_exact_zero(self):
        first = TOOL.fixture(self.fixture, self.root / "noise.wav", "noise", 11)
        second = TOOL.fixture(self.fixture, self.root / "noise-again.wav", "noise", 11)
        self.assertEqual(first["sha256"], second["sha256"])
        self.assertNotEqual(first["sha256"], TOOL.digest(self.fixture))
        silence = TOOL.fixture(self.fixture, self.root / "silence.wav", "silence", 11)
        self.assertEqual(silence["sha256"], "1a092b23d9dfe64d53943d18262c0a49eaf32c5ab5f2b016cd8edd16ec9440dd")
        with wave.open(str(self.root / "silence.wav")) as reader:
            self.assertEqual(reader.readframes(reader.getnframes()), bytes(352000))
        stress = TOOL.fixture(self.fixture, self.root / "stress.wav", "stress", 33)
        self.assertEqual(stress["seconds"], 33)
        with wave.open(str(self.root / "stress.wav")) as reader:
            self.assertEqual(reader.readframes(reader.getnframes()), self.fixture.read_bytes()[-352000:] * 3)

    def test_caption_parser_distinguishes_bounds_from_timing_accuracy(self):
        path = self.root / "caption.vtt"
        path.write_text("WEBVTT\n\n00:00:00.000 --> 00:00:02.000\n You\n\n")
        result = TOOL.captions(path, 11)
        self.assertTrue(result["structural_valid"])
        self.assertEqual(result["text"].strip(), "You")
        self.assertIn("unmeasured", result["timing_accuracy"])
        path.write_text("WEBVTT\n\n00:00:00.000 --> 00:00:12.000\n Invalid\n\n")
        self.assertFalse(TOOL.captions(path, 11)["structural_valid"])
        path.write_text("WEBVTT\n\n")
        self.assertFalse(TOOL.captions(path, 11)["structural_valid"])
        path.write_bytes(b"x" * (TOOL.MAX_OUTPUT + 1))
        with self.assertRaisesRegex(ValueError, "output_too_large"):
            TOOL.captions(path, 11)

    def test_corpus_pins_source_and_licensed_reference_separately(self):
        catalog = json.loads(TOOL.CATALOG.read_text())
        source = catalog["sources"]["jfk"]
        self.assertEqual(TOOL.digest(ROOT / source["path"]), source["sha256"])
        film = catalog["sources"]["tears-of-steel"]
        reference = TOOL.subtitle_reference(TOOL.CATALOG.parent / film["reference_file"], film["reference_sha256"])
        self.assertIn("You're a jerk", reference)
        self.assertNotIn("-->", reference)
        self.assertIn("not film quality", catalog["cases"]["stress"]["evidence"])
        self.assertIsNone(catalog["sources"]["kafka-de"]["reference"])
        german = catalog["sources"]["fleurs-de"]
        self.assertEqual(TOOL.digest(ROOT / german["path"]), german["sha256"])
        independent = json.loads((TOOL.CATALOG.parent / "fleurs-de.reference.json").read_text())
        self.assertEqual(german["reference"], independent["normalized_transcription"])
        self.assertEqual(independent["num_samples"], 207360)
        self.assertIn(independent["revision"], german["reference_source"])
        self.assertEqual(german["license_url"], "https://creativecommons.org/licenses/by/4.0/")
        self.assertEqual(TOOL.digest(ROOT / german["license_text"]), german["license_text_sha256"])
        self.assertIn("unmodified", independent["changes"])
        notice = (TOOL.CATALOG.parent / "LICENSE.txt").read_text()
        self.assertIn("https://creativecommons.org/licenses/by/4.0/", notice)
        self.assertIn("https://huggingface.co/datasets/google/fleurs", notice)
        self.assertIn("Google FLEURS", (ROOT / "THIRD-PARTY-NOTICES.md").read_text())
        with self.assertRaisesRegex(ValueError, "reference_identity_changed"):
            TOOL.subtitle_reference(TOOL.CATALOG.parent / film["reference_file"], "0" * 64)

    def test_real_film_retains_raw_failure_and_reports_exact_pcm_intersection(self):
        raw = TOOL.CATALOG.parent / "base-film.raw.vtt"
        facts = json.loads((TOOL.CATALOG.parent / "base-film.provenance.json").read_text())
        self.assertEqual(TOOL.digest(raw), facts["sha256"])
        result = TOOL.captions(raw, facts["decoded_samples_16000hz"] / 16000)
        self.assertFalse(result["structural_valid"])
        normalized, metrics = TOOL.intersect_vtt(raw.read_text(), facts["decoded_samples_16000hz"])
        self.assertEqual(metrics, dict(clipped=1, discarded=0, pcm_end_ms=734093))
        self.assertIn("00:12:13.400 --> 00:12:14.093\n [BLANK_AUDIO]", normalized)
        output = self.root / "normalized.vtt"
        output.write_text(normalized)
        self.assertTrue(TOOL.captions(output, 734.0930625)["structural_valid"])

    def test_timeout_reaps_child_and_clean_environment_is_explicit(self):
        import select
        from types import SimpleNamespace
        from unittest import mock

        program = self.root / "child.py"
        marker = self.root / "pid"
        ready_read, ready_write = os.pipe()
        writer_open = True
        started = False
        program.write_text(
            "import os,time\nfrom pathlib import Path\n"
            f"Path({str(marker)!r}).write_text(str(os.getpid()))\n"
            f"os.write({ready_write}, b'R')\n"
            "time.sleep(20)\n"
        )

        def deadline_clock():
            nonlocal started, writer_open
            if not started:
                started = True
                return 0.0
            # Drive expiry only after an acknowledged real child exists.
            # Startup has its own bounded handshake; the production helper's
            # wall-clock deadline and kill/reap path are unchanged.
            os.close(ready_write)
            writer_open = False
            readable, _, _ = select.select([ready_read], [], [], 5)
            self.assertTrue(readable, "timeout fixture did not acknowledge startup")
            self.assertEqual(os.read(ready_read, 1), b"R")
            return 1.0

        try:
            with mock.patch.object(TOOL, "time", SimpleNamespace(monotonic=deadline_clock)):
                with self.assertRaisesRegex(ValueError, "probe_timeout"):
                    TOOL.run_child([sys.executable, str(program)], self.root, "probe", 0.15,
                                   pass_fds=(ready_write,))
        finally:
            os.close(ready_read)
            if writer_open:
                os.close(ready_write)
        pid = int(marker.read_text())
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)
        program.write_text("import os,json\nprint(json.dumps(dict(os.environ)))\n")
        result = TOOL.run_child([sys.executable, "-E", "-S", str(program)], self.root, "environment", 5)
        self.assertEqual(result["exit_code"], 0)
        # CPython can add LC_CTYPE for UTF-8 coercion; the child invocation
        # itself receives exactly LC_ALL=C, never PATH or loader/proxy values.
        environment = json.loads((self.root / "environment.stdout").read_text())
        self.assertEqual(environment["LC_ALL"], "C")
        self.assertNotIn("PATH", environment)
        self.assertNotIn("HTTPS_PROXY", environment)

    def test_oversize_writer_is_stopped_and_reaped(self):
        program = self.root / "writer.py"
        program.write_text("import sys\nsys.stdout.buffer.write(b'x'*400000)\nsys.stdout.flush()\n")
        with self.assertRaisesRegex(ValueError, "probe_output_bound_exceeded"):
            TOOL.run_child([sys.executable, str(program)], self.root, "probe", 5)


if __name__ == "__main__":
    unittest.main()
