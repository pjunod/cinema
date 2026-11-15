"""Focused arithmetic admission regression; no GPU or media dependency."""
import os
from pathlib import Path
import subprocess
import shlex
import tempfile
import unittest


class MetadataAdmission(unittest.TestCase):
    def test_source_reader_remote_capability_budget_and_interrupt(self):
        from tools.dv_processing.controls.test_movie_source_io import (
            test_private_movie_source_url_budget_and_nested_io,
        )
        source = Path(__file__).with_name("segment_decode_render.c")
        with tempfile.TemporaryDirectory() as directory:
            test_private_movie_source_url_budget_and_nested_io(
                source, Path(directory) / "source-controls", static_ffmpeg=True,
            )

    def test_bounded_source_reader(self):
        source = Path(__file__).with_name("test_source_reader.c")
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "source-reader"
            subprocess.run([os.environ.get("CC", "cc"), "-O2", "-std=c11",
                            "-Wall", "-Wextra", "-Werror", "-pthread", str(source),
                            "-o", str(binary)], check=True, timeout=30)
            subprocess.run([str(binary)], check=True, timeout=20)

    def test_source_reader_preserves_unread_avio_bytes_across_seeks(self):
        source = Path(__file__).with_name("test_source_reader_avio.c")
        flags = shlex.split(subprocess.check_output(
            ["pkg-config", "--static", "--cflags", "--libs", "libavformat",
             "libavcodec", "libavutil"], text=True, timeout=15))
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "source-reader-avio"
            subprocess.run([os.environ.get("CC", "cc"), "-O2", "-std=c11",
                            "-Wall", "-Wextra", "-Werror", "-pthread", str(source),
                            "-o", str(binary), *flags, "-lm", "-ldl"],
                           check=True, timeout=60)
            subprocess.run([str(binary)], check=True, timeout=20)

    def test_title_mastering_constants_survive_missing_blocks(self):
        """Run the production accumulator, including nonadjacent conflicts."""
        source = Path(__file__).with_name("test_title_constants.c")
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "title-constants"
            subprocess.run([os.environ.get("CC", "cc"), "-std=c11", "-Wall",
                            "-Wextra", "-Werror", str(source), "-o", str(binary)],
                           check=True, timeout=30)
            subprocess.run([str(binary)], check=True, timeout=10)

    def test_nonbinding_residual_limit(self):
        source = Path(__file__).with_name("test_nlq_clipping.c")
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "nlq-limit"
            subprocess.run([os.environ.get("CC", "cc"), "-std=c11", "-Wall",
                            "-Wextra", "-Werror", str(source), "-lm", "-o",
                            str(binary)], check=True, timeout=30)
            subprocess.run([str(binary)], check=True, timeout=10)

    def test_finite_rgb48_rounding(self):
        source = Path(__file__).with_name("test_rgb48.c")
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "rgb48-rounding"
            subprocess.run([os.environ.get("CC", "cc"), "-O2", "-fno-math-errno", "-frounding-math",
                            "-std=c11", "-Wall", "-Wextra", "-Werror",
                            str(source), "-lm", "-o", str(binary)],
                           check=True, timeout=30)
            subprocess.run([str(binary)], check=True, timeout=15)

    def test_refit_composition(self):
        """F+ composition, lattice, statistics and pooling; the nonidentity
        mapping's vectors must equal the ones plurx-core's dv_refit replays."""
        source = Path(__file__).with_name("test_refit_compose.c")
        fixture = (Path(__file__).resolve().parents[2] / "crates/plurx-core/tests/"
                   "fixtures/dv-refit/composer-p7-mapping.json")
        flags = shlex.split(subprocess.check_output(
            ["pkg-config", "--static", "--cflags", "--libs", "libavutil"],
            text=True, timeout=15))
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "refit-compose"
            vectors = Path(directory) / "vectors.json"
            subprocess.run([os.environ.get("CC", "cc"), "-O2", "-fno-math-errno",
                            "-frounding-math", "-ffp-contract=off", "-std=c11", "-Wall",
                            "-Wextra", "-Werror", str(source), "-o", str(binary), *flags,
                            "-lm", "-lpthread", "-ldl"], check=True, timeout=120)
            subprocess.run([str(binary), str(vectors)], check=True, timeout=60)
            self.assertEqual(vectors.read_bytes(), fixture.read_bytes())

    def test_public_piecewise_fel_mapping(self):
        """Pinned public headers/libraries; CPU-only mapping and refusal grid."""
        source = Path(__file__).with_name("test_decoded_metadata.c")
        flags = shlex.split(subprocess.check_output(
            ["pkg-config", "--static", "--cflags", "--libs", "libavformat",
             "libavcodec", "libavutil", "libplacebo"], text=True, timeout=15))
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "decoded-metadata"
            subprocess.run([os.environ.get("CC", "cc"), "-O2", "-std=c11",
                            "-Wall", "-Wextra", "-Werror", str(source),
                            "-o", str(binary), *flags, "-lm", "-lpthread", "-ldl"],
                           check=True, timeout=120)
            subprocess.run([str(binary)], check=True, timeout=15)


if __name__ == "__main__":
    unittest.main()
